use super::iec::*;
use crate::{
    dnf::{recursive_decompose, Dnf, RecursiveDecompose},
    product_tree::ProductTree,
    union_combination::*,
    utils::hashmap_reduce,
    Game, OwnerId, ShapleyValues,
};
use clap::ValueEnum;
use rayon::prelude::*;
use std::collections::BTreeSet;

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, ValueEnum)]
pub enum AblationType {
    NoHorizontal,
    NoVertical,
    NoHybrid,
}

pub fn cal_sv_recursive_decompose_ablation(
    game: &Game,
    ablation_type: AblationType,
) -> ShapleyValues {
    let d = recursive_decompose(&game.dnf, &game.owner_set);
    let tree = DecomposeTree::new(d, true, ablation_type);
    let gamma_map = IECoeffs::from([(0, 1)]);
    tree.cal_sv(&gamma_map)
}

enum DecomposeTree {
    Var(OwnerId),
    And {
        coeffs: Option<IECoeffs>,
        products: Vec<IECoeffs>,
        children: Vec<DecomposeTree>,
    },
    Or {
        coeffs: Option<IECoeffs>,
        products: Vec<IECoeffs>,
        children: Vec<DecomposeTree>,
    },
    Hybrid {
        coeffs: Option<IECoeffs>,
        hybrid_coeffs: HybridCoeffs,
        hybrid_exp: Dnf<usize>,
        children: Vec<DecomposeTree>,
    },
    Leaf {
        coeffs: Option<IECoeffs>,
        exp: Dnf<OwnerId>,
    },
}

impl DecomposeTree {
    fn new(input: RecursiveDecompose<OwnerId>, is_root: bool, ablation_type: AblationType) -> Self {
        match input {
            RecursiveDecompose::Var(id) => Self::Var(id),
            RecursiveDecompose::And(children) if ablation_type != AblationType::NoVertical => {
                let children: Vec<_> = children
                    .into_par_iter()
                    .map(|c| DecomposeTree::new(c, false, ablation_type))
                    .collect();
                let mut children_coeffs = Vec::with_capacity(children.len());
                for c in &children {
                    children_coeffs.push(c.coeffs());
                }
                let product_tree: ProductTree<IECoeffs> =
                    ProductTree::new(children_coeffs, vertical_op, !is_root);
                let products = product_tree.all_products(vertical_identity, vertical_op);
                let coeffs = if is_root {
                    None
                } else {
                    Some(product_tree.root())
                };
                Self::And {
                    coeffs,
                    products,
                    children,
                }
            }
            RecursiveDecompose::Or(children) if ablation_type != AblationType::NoHorizontal => {
                let children: Vec<_> = children
                    .into_par_iter()
                    .map(|c| DecomposeTree::new(c, false, ablation_type))
                    .collect();
                let mut children_coeffs = Vec::with_capacity(children.len());
                for c in &children {
                    children_coeffs.push(c.coeffs());
                }
                let product_tree: ProductTree<IECoeffs> =
                    ProductTree::new(children_coeffs, horizontal_op, !is_root);
                let products = product_tree.all_products(horizontal_identity, horizontal_op);
                let coeffs = if is_root {
                    None
                } else {
                    Some(product_tree.root())
                };
                Self::Or {
                    coeffs,
                    products,
                    children,
                }
            }
            RecursiveDecompose::Hybrid {
                hybrid_exp,
                sub_exps,
            } if ablation_type != AblationType::NoHybrid => {
                let children: Vec<_> = sub_exps
                    .into_par_iter()
                    .map(|c| DecomposeTree::new(c, false, ablation_type))
                    .collect();
                let mut children_coeffs = Vec::with_capacity(children.len());
                for c in &children {
                    children_coeffs.push(c.coeffs());
                }
                let hybrid_coeffs = HybridCoeffs::new(&children_coeffs);
                let coeffs = if is_root {
                    None
                } else {
                    Some(hybrid_coeffs.exp_coeffs(&hybrid_exp))
                };
                Self::Hybrid {
                    coeffs,
                    hybrid_coeffs,
                    hybrid_exp,
                    children,
                }
            }
            _ => {
                let exp: Dnf<OwnerId> = input.expand();
                let coeffs = if is_root {
                    None
                } else {
                    let exp_unions = leaf_exp_to_unions(&exp);
                    let coeffs = leaf_exp_unions_coeffs(&exp_unions);
                    Some(coeffs)
                };
                Self::Leaf { coeffs, exp }
            }
        }
    }

    fn coeffs(&self) -> IECoeffs {
        match self {
            DecomposeTree::Var(_) => IECoeffs::from([(1, 1)]),
            DecomposeTree::And { coeffs, .. } => coeffs.clone().unwrap(),
            DecomposeTree::Or { coeffs, .. } => coeffs.clone().unwrap(),
            DecomposeTree::Hybrid { coeffs, .. } => coeffs.clone().unwrap(),
            DecomposeTree::Leaf { coeffs, .. } => coeffs.clone().unwrap(),
        }
    }

    fn cal_sv(&self, gamma_map: &IECoeffs) -> ShapleyValues {
        match self {
            DecomposeTree::Var(owner_id) => {
                let map_group_with_owner = IECoeffs::from([(1, 1)]);
                let sv = (&map_group_with_owner * gamma_map).to_sv();
                ShapleyValues::from([(*owner_id, sv)])
            }
            DecomposeTree::And {
                products, children, ..
            } => {
                let var_children: Vec<_> = children
                    .iter()
                    .enumerate()
                    .filter_map(|(i, c)| match c {
                        Self::Var(id) => Some((i, id)),
                        _ => None,
                    })
                    .collect();

                let mut ans = children
                    .par_iter()
                    .enumerate()
                    .filter(|(_, c)| !matches!(c, Self::Var(_)))
                    .map(|(i, c)| {
                        let iece_map = &products[i];
                        let next_gamma_map = gamma_map * iece_map;
                        c.cal_sv(&next_gamma_map)
                    })
                    .reduce(ShapleyValues::default, hashmap_reduce);

                if let Some((i, _)) = var_children.first() {
                    let iece_map = &products[*i];
                    let next_gamma_map = gamma_map * iece_map;
                    let sv = (&IECoeffs::from([(1, 1)]) * &next_gamma_map).to_sv();
                    for (_, id) in var_children {
                        ans.insert(*id, sv);
                    }
                }

                ans
            }
            DecomposeTree::Or {
                products, children, ..
            } => {
                let var_children: Vec<_> = children
                    .iter()
                    .enumerate()
                    .filter_map(|(i, c)| match c {
                        Self::Var(id) => Some((i, id)),
                        _ => None,
                    })
                    .collect();

                let mut ans = children
                    .par_iter()
                    .enumerate()
                    .filter(|(_, c)| !matches!(c, Self::Var(_)))
                    .map(|(i, c)| {
                        let iece_map = &products[i];
                        let next_gamma_map = gamma_map - &(gamma_map * iece_map);
                        c.cal_sv(&next_gamma_map)
                    })
                    .reduce(ShapleyValues::default, hashmap_reduce);

                if let Some((i, _)) = var_children.first() {
                    let iece_map = &products[*i];
                    let next_gamma_map = gamma_map - &(gamma_map * iece_map);
                    let sv = (&IECoeffs::from([(1, 1)]) * &next_gamma_map).to_sv();
                    for (_, id) in var_children {
                        ans.insert(*id, sv);
                    }
                }

                ans
            }
            DecomposeTree::Hybrid {
                hybrid_coeffs,
                hybrid_exp,
                children,
                ..
            } => children
                .par_iter()
                .enumerate()
                .map(|(i, c)| {
                    let owner_set = BTreeSet::from([i]);
                    let exp_p2 = hybrid_exp.partial_eval(&owner_set, true);
                    let exp_p3 = hybrid_exp.partial_exp_complement(&owner_set);
                    let exp_p2_unions = exp_to_input_unions(&exp_p2);
                    let exp_p3_unions = exp_to_input_unions(&exp_p3);
                    let map_p2 = hybrid_coeffs.exp_unions_coeffs(&exp_p2_unions);
                    let iece_map =
                        hybrid_coeffs.exp_unions_interaction(&exp_p2_unions, &exp_p3_unions);
                    let next_gamma_map = gamma_map * &(map_p2 - iece_map);
                    c.cal_sv(&next_gamma_map)
                })
                .reduce(ShapleyValues::default, hashmap_reduce),
            DecomposeTree::Leaf { exp, .. } => exp
                .all_variables()
                .par_iter()
                .map(|&c| {
                    let owner_set = BTreeSet::from([c]);
                    let exp_p2 = exp.partial_eval(&owner_set, true);
                    let exp_p3 = exp.partial_exp_complement(&owner_set);

                    let exp_p2_unions = leaf_exp_to_unions(&exp_p2);
                    let map_p2 = leaf_exp_unions_coeffs(&exp_p2_unions);

                    let exp_p3_unions = leaf_exp_to_unions(&exp_p3);
                    let iece_map = leaf_exp_unions_interaction(&exp_p2_unions, &exp_p3_unions);

                    let next_gamma_map = if exp_p2.all_variables().is_empty() {
                        gamma_map - &(gamma_map * &iece_map)
                    } else {
                        gamma_map * &(map_p2 - iece_map)
                    };

                    let map_group_with_owner = IECoeffs::from([(1, 1)]);
                    let sv = (&map_group_with_owner * &next_gamma_map).to_sv();
                    ShapleyValues::from([(c, sv)])
                })
                .reduce(ShapleyValues::default, hashmap_reduce),
        }
    }
}

#[derive(Debug, Clone)]
struct LeafExpUnion {
    input_set: BTreeSet<OwnerId>,
    num_of_imp: usize,
}

fn leaf_exp_to_unions(exp: &Dnf<OwnerId>) -> UnionCombination<LeafExpUnion> {
    let var_len = exp.all_variables().len();
    let imp_list: Vec<_> = exp.iter().collect();
    UnionCombination::new(
        imp_list.len(),
        |i| LeafExpUnion {
            num_of_imp: 1,
            input_set: imp_list[i].0.clone(),
        },
        |old, i| {
            let new_imp = imp_list[i];
            let mut new_set = old.input_set.clone();
            new_set.extend(new_imp.iter().copied());
            // whether new set is full and cur_id != MAX_ID
            if new_set.len() == var_len && i != imp_list.len() - 1 {
                None
            } else {
                Some(LeafExpUnion {
                    num_of_imp: old.num_of_imp + 1,
                    input_set: new_set,
                })
            }
        },
    )
}

fn leaf_exp_unions_coeffs(exp_unions: &UnionCombination<LeafExpUnion>) -> IECoeffs {
    exp_unions
        .0
        .par_iter()
        .map(|u| {
            let u = u.get();
            let sign = if u.num_of_imp % 2 == 0 { -1 } else { 1 };
            IECoeffs::from([(u.input_set.len(), sign)])
        })
        .sum()
}

fn leaf_exp_unions_interaction(
    exp_unions1: &UnionCombination<LeafExpUnion>,
    exp_unions2: &UnionCombination<LeafExpUnion>,
) -> IECoeffs {
    exp_unions1
        .0
        .par_iter()
        .flat_map(|u1| {
            let u1 = u1.get();
            exp_unions2.0.par_iter().map(move |u2| {
                let u2 = u2.get();
                let input_set: BTreeSet<_> = u1.input_set.union(&u2.input_set).collect();
                let sign = if (u1.num_of_imp + u2.num_of_imp) % 2 == 0 {
                    1
                } else {
                    -1
                };
                IECoeffs::from([(input_set.len(), sign)])
            })
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{dnf, tests::assert_f64_eq, OwnerSet};

    #[test]
    fn test_cal_sv_recursive_decompose_ablation() {
        // test for complementary owners
        let game = Game {
            dnf: dnf!(1 2 3).map_variable(|id| OwnerId(*id)),
            owner_set: OwnerSet::from_iter([OwnerId(1), OwnerId(2), OwnerId(3)]),
        };

        let sv = cal_sv_recursive_decompose_ablation(&game, AblationType::NoVertical);
        assert_f64_eq(0.33333333333, sv[&OwnerId(1)]);
        assert_f64_eq(0.33333333333, sv[&OwnerId(2)]);
        assert_f64_eq(0.33333333333, sv[&OwnerId(3)]);

        let game = Game {
            dnf: dnf!(1 2 3 + 1 2 4 ).map_variable(|id| OwnerId(*id)),
            owner_set: OwnerSet::from_iter([OwnerId(1), OwnerId(2), OwnerId(3), OwnerId(4)]),
        };

        let sv = cal_sv_recursive_decompose_ablation(&game, AblationType::NoVertical);
        assert_f64_eq(0.41666666666, sv[&OwnerId(1)]);
        assert_f64_eq(0.41666666666, sv[&OwnerId(2)]);
        assert_f64_eq(0.08333333333, sv[&OwnerId(3)]);
        assert_f64_eq(0.08333333333, sv[&OwnerId(4)]);

        let game = Game {
            dnf: dnf!(1 2 3 4 + 1 2 3 5 + 6).map_variable(|id| OwnerId(*id)),
            owner_set: OwnerSet::from_iter([OwnerId(1), OwnerId(2), OwnerId(3), OwnerId(4)]),
        };

        let sv = cal_sv_recursive_decompose_ablation(&game, AblationType::NoVertical);
        assert_f64_eq(0.06666666666, sv[&OwnerId(1)]);
        assert_f64_eq(0.06666666666, sv[&OwnerId(2)]);
        assert_f64_eq(0.06666666666, sv[&OwnerId(3)]);
        assert_f64_eq(0.01666666666, sv[&OwnerId(4)]);
        assert_f64_eq(0.01666666666, sv[&OwnerId(5)]);
        assert_f64_eq(0.76666666666, sv[&OwnerId(6)]);

        // test for replaceable owners
        let game = Game {
            dnf: dnf!(1 + 2 + 3).map_variable(|id| OwnerId(*id)),
            owner_set: OwnerSet::from_iter([OwnerId(1), OwnerId(2), OwnerId(3)]),
        };

        let sv = cal_sv_recursive_decompose_ablation(&game, AblationType::NoVertical);
        assert_f64_eq(0.33333333333, sv[&OwnerId(1)]);
        assert_f64_eq(0.33333333333, sv[&OwnerId(2)]);
        assert_f64_eq(0.33333333333, sv[&OwnerId(3)]);

        let game = Game {
            dnf: dnf!(1 4 5 + 2 4 5 + 3 4 5).map_variable(|id| OwnerId(*id)),
            owner_set: OwnerSet::from_iter([
                OwnerId(1),
                OwnerId(2),
                OwnerId(3),
                OwnerId(4),
                OwnerId(5),
            ]),
        };

        let sv = cal_sv_recursive_decompose_ablation(&game, AblationType::NoVertical);
        assert_f64_eq(0.03333333333, sv[&OwnerId(1)]);
        assert_f64_eq(0.03333333333, sv[&OwnerId(2)]);
        assert_f64_eq(0.03333333333, sv[&OwnerId(3)]);
        assert_f64_eq(0.45, sv[&OwnerId(4)]);
        assert_f64_eq(0.45, sv[&OwnerId(5)]);

        // test for hybrid
        let game = Game {
            dnf: dnf!(1 2 4 + 1 2 5 + 2 3 4 + 2 3 5 + 4 5).map_variable(|id| OwnerId(*id)),
            owner_set: OwnerSet::from_iter([
                OwnerId(1),
                OwnerId(2),
                OwnerId(3),
                OwnerId(4),
                OwnerId(5),
            ]),
        };

        let sv = cal_sv_recursive_decompose_ablation(&game, AblationType::NoVertical);
        assert_f64_eq(0.06666666666, sv[&OwnerId(1)]);
        assert_f64_eq(0.23333333333, sv[&OwnerId(2)]);
        assert_f64_eq(0.06666666666, sv[&OwnerId(3)]);
        assert_f64_eq(0.31666666666, sv[&OwnerId(4)]);
        assert_f64_eq(0.31666666666, sv[&OwnerId(5)]);

        // test recursive
        let game = Game {
            dnf: dnf!(1 3 6 8 + 3 5 6 8 + 3 4 6 8 9).map_variable(|id| OwnerId(*id)),
            owner_set: OwnerSet::from_iter([
                OwnerId(1),
                OwnerId(3),
                OwnerId(4),
                OwnerId(5),
                OwnerId(6),
                OwnerId(8),
                OwnerId(9),
            ]),
        };

        let sv = cal_sv_recursive_decompose_ablation(&game, AblationType::NoVertical);
        assert_f64_eq(0.3095238095238095, sv[&OwnerId(3)]);
        assert_f64_eq(0.026190476190476153, sv[&OwnerId(5)]);
        assert_f64_eq(0.3095238095238095, sv[&OwnerId(8)]);
        assert_f64_eq(0.3095238095238095, sv[&OwnerId(6)]);
        assert_f64_eq(0.026190476190476153, sv[&OwnerId(1)]);
        assert_f64_eq(0.009523809523809545, sv[&OwnerId(4)]);
        assert_f64_eq(0.009523809523809545, sv[&OwnerId(9)]);
    }

    #[test]
    fn test_performance() {
        let game = Game {
                dnf: dnf!(0 4 12 17 + 0 7 12 17 + 0 4 5 9 17 + 0 4 5 10 17 + 0 4 9 15 17 + 0 4 10 15 17 + 4 5 10 13 17 + 4 10 12 13 17 + 4 10 13 15 17 + 7 10 12 13 17 + 0 5 6 7 9 17 + 0 5 6 7 10 17 + 0 6 7 9 15 17 + 0 6 7 10 15 17 + 5 6 7 10 13 17 + 6 7 10 13 15 17).map_variable(|id| OwnerId(*id)),
                owner_set: OwnerSet::from_iter([
                    OwnerId(0),
                    OwnerId(4),
                    OwnerId(5),
                    OwnerId(6),
                    OwnerId(7),
                    OwnerId(9),
                    OwnerId(10),
                    OwnerId(12),
                    OwnerId(13),
                    OwnerId(15),
                    OwnerId(17),
                ]),
            };

        let sv = cal_sv_recursive_decompose_ablation(&game, AblationType::NoHybrid);
        assert_f64_eq(0.013492063492063444, sv[&OwnerId(6)]);

        let sv = cal_sv_recursive_decompose_ablation(&game, AblationType::NoVertical);
        assert_f64_eq(0.013492063492063444, sv[&OwnerId(6)]);

        let sv = cal_sv_recursive_decompose_ablation(&game, AblationType::NoHorizontal);
        assert_f64_eq(0.013492063492063444, sv[&OwnerId(6)]);
    }

    #[test]
    fn test_ablation() {
        let game = Game {
            dnf: dnf!(1 2 4 + 1 2 5 + 2 3 4 + 2 3 5 + 4 5).map_variable(|id| OwnerId(*id)),
            owner_set: OwnerSet::from_iter([
                OwnerId(1),
                OwnerId(2),
                OwnerId(3),
                OwnerId(4),
                OwnerId(5),
            ]),
        };

        let sv = cal_sv_recursive_decompose_ablation(&game, AblationType::NoVertical);
        assert_f64_eq(0.06666666666, sv[&OwnerId(1)]);
        assert_f64_eq(0.23333333333, sv[&OwnerId(2)]);
        assert_f64_eq(0.06666666666, sv[&OwnerId(3)]);
        assert_f64_eq(0.31666666666, sv[&OwnerId(4)]);
        assert_f64_eq(0.31666666666, sv[&OwnerId(5)]);

        let sv = cal_sv_recursive_decompose_ablation(&game, AblationType::NoHorizontal);
        assert_f64_eq(0.06666666666, sv[&OwnerId(1)]);
        assert_f64_eq(0.23333333333, sv[&OwnerId(2)]);
        assert_f64_eq(0.06666666666, sv[&OwnerId(3)]);
        assert_f64_eq(0.31666666666, sv[&OwnerId(4)]);
        assert_f64_eq(0.31666666666, sv[&OwnerId(5)]);

        let sv = cal_sv_recursive_decompose_ablation(&game, AblationType::NoHybrid);
        assert_f64_eq(0.06666666666, sv[&OwnerId(1)]);
        assert_f64_eq(0.23333333333, sv[&OwnerId(2)]);
        assert_f64_eq(0.06666666666, sv[&OwnerId(3)]);
        assert_f64_eq(0.31666666666, sv[&OwnerId(4)]);
        assert_f64_eq(0.31666666666, sv[&OwnerId(5)]);
    }

    #[test]
    fn test_vertical_decom() {
        let game = Game {
            dnf: dnf!(1 2 5 + 1 2 6 + 1 3 5 + 1 3 6 + 4 5 + 4 6).map_variable(|id| OwnerId(*id)),
            owner_set: OwnerSet::from_iter([
                OwnerId(1),
                OwnerId(2),
                OwnerId(3),
                OwnerId(4),
                OwnerId(5),
                OwnerId(6),
            ]),
        };

        let sv = cal_sv_recursive_decompose_ablation(&game, AblationType::NoVertical);
        assert_f64_eq(0.16666666666, sv[&OwnerId(1)]);

        let sv = cal_sv_recursive_decompose_ablation(&game, AblationType::NoHorizontal);
        assert_f64_eq(0.16666666666, sv[&OwnerId(1)]);

        let sv = cal_sv_recursive_decompose_ablation(&game, AblationType::NoHybrid);
        assert_f64_eq(0.16666666666, sv[&OwnerId(1)]);
    }

    #[test]
    fn test_horizontal_decom() {
        let game = Game {
            dnf: dnf!(1 2 + 1 3 + 4 ).map_variable(|id| OwnerId(*id)),
            owner_set: OwnerSet::from_iter([OwnerId(1), OwnerId(2), OwnerId(3), OwnerId(4)]),
        };

        let sv = cal_sv_recursive_decompose_ablation(&game, AblationType::NoHorizontal);
        assert_f64_eq(0.25, sv[&OwnerId(1)]);
        assert_f64_eq(0.08333333333333337, sv[&OwnerId(2)]);
        assert_f64_eq(0.08333333333333337, sv[&OwnerId(3)]);
        assert_f64_eq(0.5833333333333334, sv[&OwnerId(4)]);

        let sv = cal_sv_recursive_decompose_ablation(&game, AblationType::NoVertical);
        assert_f64_eq(0.25, sv[&OwnerId(1)]);
        assert_f64_eq(0.08333333333333337, sv[&OwnerId(2)]);
        assert_f64_eq(0.08333333333333337, sv[&OwnerId(3)]);
        assert_f64_eq(0.5833333333333334, sv[&OwnerId(4)]);

        let sv = cal_sv_recursive_decompose_ablation(&game, AblationType::NoHybrid);
        assert_f64_eq(0.25, sv[&OwnerId(1)]);
        assert_f64_eq(0.08333333333333337, sv[&OwnerId(2)]);
        assert_f64_eq(0.08333333333333337, sv[&OwnerId(3)]);
        assert_f64_eq(0.5833333333333334, sv[&OwnerId(4)]);
    }
}
