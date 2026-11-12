//! Fixed-order subproblems with an admissible composition-objective bound.
//! Reordering or replacing songs creates an independent root, not a descendant
//! of a pruned branch. Branches never authorize audio or assert cue completeness.
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct Branch {
    pub order: Vec<usize>,
    /// Measured-edge indices, or nil, one per adjacent song pair.
    pub choices: Vec<Option<usize>>,
    /// Only this prefix is immutable. Later choices remain provisional.
    pub locked: usize,
}

impl Branch {
    pub fn new(order: Vec<usize>, choices: Vec<Option<usize>>) -> Option<Self> {
        let branch = Self {
            order,
            choices,
            locked: 0,
        };
        branch.valid().then_some(branch)
    }

    fn valid(&self) -> bool {
        !self.order.is_empty()
            && self.choices.len() == self.order.len() - 1
            && self.locked <= self.choices.len()
            && self.order.iter().collect::<BTreeSet<_>>().len() == self.order.len()
    }

    pub fn leaf(&self) -> bool {
        self.valid() && self.locked == self.choices.len()
    }

    /// Bound every unmeasured/rejected/nil open adjacency at 120 quality seconds.
    /// Existing provisional assignments are not commitments and cannot lower
    /// this bound. Composition penalties are optimistically zero.
    ///
    /// `quality` returns dense + handoff quality seconds for a measured edge,
    /// not its fit score. Compare only with an approved plan's composition
    /// objective. `None` means unknown and cannot authorize pruning. The caller
    /// owns timing compatibility and checking edge identities against `order`.
    pub fn upper_bound(&self, quality: impl Fn(usize) -> f64, min_seconds: f64) -> Option<f64> {
        if !self.valid() || !min_seconds.is_finite() || min_seconds <= 0. {
            return None;
        }
        let limit = mixlib_planner::MAX_TACTIC_CONTINUATION_SECONDS;
        let mut total = 0.;
        for choice in &self.choices[..self.locked] {
            let value = choice.map_or(0., &quality);
            if !value.is_finite() || !(0.0..=limit).contains(&value) {
                return None;
            }
            total += value;
        }
        total += (self.choices.len() - self.locked) as f64 * limit;
        let bound = total * 60. / min_seconds;
        // Outward allowance protects effectively tied completions from a
        // different floating-point summation order during final evaluation.
        let upper = bound + (bound.abs() + 1.) * 1e-12;
        upper.is_finite().then_some(upper)
    }

    /// Tighten the quality bound with unavoidable fit penalties from locked
    /// measured joins. Composition subtracts max(25 - fit, 0) per join divided
    /// by actual minutes. Dividing that locked cost by the longest permitted
    /// duration is an optimistic lower bound on the penalty. Open joins retain
    /// perfect future fit and all other penalties remain zero.
    pub fn upper_bound_with_fit(
        &self,
        quality: impl Fn(usize) -> f64,
        fit: impl Fn(usize) -> f64,
        min_seconds: f64,
        max_seconds: f64,
    ) -> Option<f64> {
        if !max_seconds.is_finite() || max_seconds < min_seconds {
            return None;
        }
        let quality_bound = self.upper_bound(quality, min_seconds)?;
        let mut shortfall = 0.;
        for choice in self.choices[..self.locked].iter().flatten() {
            let score = fit(*choice);
            if !score.is_finite() {
                return None;
            }
            shortfall += (25. - score).max(0.);
        }
        let penalty = shortfall * 60. / max_seconds;
        let bound = quality_bound - penalty;
        // Include the cost's magnitude in the outward rounding allowance so
        // near cancellation between benefit and penalty remains conservative.
        let upper = bound + (quality_bound.abs() + penalty.abs() + 1.) * 1e-12;
        upper.is_finite().then_some(upper)
    }

    /// Resolve exactly the next adjacency while preserving the locked prefix
    /// and provisional suffix. Nil remains an explicit zero-point alternative.
    /// The supplied alternatives describe this expansion only: exhausting them
    /// is not evidence that ungenerated cue/tactic choices cannot exist.
    pub fn children(&self, alternatives: &[usize]) -> Vec<Self> {
        if !self.valid() || self.leaf() {
            return vec![];
        }
        let mut seen = BTreeSet::new();
        alternatives
            .iter()
            .copied()
            .filter(|id| seen.insert(*id))
            .map(Some)
            .chain(std::iter::once(None))
            .map(|choice| {
                let mut child = self.clone();
                child.choices[self.locked] = choice;
                child.locked += 1;
                child
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_or_repeated_song_orders_never_create_a_branch() {
        assert!(Branch::new(vec![], vec![]).is_none());
        assert!(Branch::new(vec![0, 1], vec![]).is_none());
        assert!(Branch::new(vec![0], vec![None]).is_none());
        assert!(Branch::new(vec![0, 1, 0], vec![None, None]).is_none());
        assert!(Branch::new(vec![7], vec![]).unwrap().leaf());
        let mut malformed = Branch::new(vec![0, 1], vec![None]).unwrap();
        malformed.locked = 2;
        assert!(!malformed.leaf());
        assert!(malformed.upper_bound(|_| 0., 3420.).is_none());
        assert!(malformed.children(&[0]).is_empty());
    }

    #[test]
    fn children_preserve_locks_order_and_unresolved_suffix() {
        let mut parent = Branch::new(vec![4, 2, 0, 1], vec![Some(10), None, Some(30)]).unwrap();
        parent.locked = 1;
        let children = parent.children(&[21, 21, 22]);
        assert_eq!(children.len(), 3);
        assert_eq!(children[0].choices, vec![Some(10), Some(21), Some(30)]);
        assert_eq!(children[1].choices, vec![Some(10), Some(22), Some(30)]);
        assert_eq!(children[2].choices, vec![Some(10), None, Some(30)]);
        assert!(children
            .iter()
            .all(|c| c.order == parent.order && c.locked == 2));
        assert_eq!(parent.locked, 1);
        let leaf = children[0].children(&[]).pop().unwrap();
        assert!(leaf.leaf());
        assert_eq!(leaf.choices, vec![Some(10), Some(21), None]);
        assert!(leaf.children(&[31]).is_empty());
    }

    #[test]
    fn bound_dominates_sampled_completions_and_relaxes_all_penalties() {
        let mut branch = Branch::new(
            (0..7).collect(),
            vec![Some(0), Some(1), None, None, None, None],
        )
        .unwrap();
        branch.locked = 3;
        let known = [12., 80.];
        let bound = branch.upper_bound(|i| known[i], 3420.).unwrap();
        assert!((bound - 452. / 57.).abs() < 1e-9);
        for a in [0., 31., 120.] {
            for b in [0., 79., 120.] {
                for c in [0., 119., 120.] {
                    for duration in [3420., 3510., 3600.] {
                        for penalty in [0., 1.5, 50.] {
                            let actual = (92. + a + b + c) / (duration / 60.) - penalty;
                            assert!(actual <= bound);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn zero_score_draft_keeps_open_potential_and_locking_cannot_raise_the_bound() {
        let branch = Branch::new((0..8).collect(), vec![None; 7]).unwrap();
        let parent = branch
            .upper_bound(|_| panic!("open joins are unmeasured"), 3420.)
            .unwrap();
        assert!(
            parent > 8.,
            "current zero score cannot justify deleting this branch"
        );
        for child in branch.children(&[0, 1, 2]) {
            let bound = child.upper_bound(|i| [0., 20., 120.][i], 3420.).unwrap();
            assert!(bound <= parent);
        }
        let mut provisional = branch;
        provisional.choices[0] = Some(99);
        assert_eq!(
            provisional.upper_bound(|_| panic!("still unlocked"), 3420.),
            Some(parent)
        );
    }

    #[test]
    fn invalid_measurements_or_denominators_are_unknown_not_zero_bounds() {
        let mut branch = Branch::new(vec![0, 1], vec![Some(0)]).unwrap();
        branch.locked = 1;
        for value in [-1., 120.01, f64::NAN, f64::INFINITY] {
            assert!(branch.upper_bound(|_| value, 3420.).is_none());
        }
        for minimum in [0., -1., f64::NAN, f64::INFINITY, f64::MIN_POSITIVE] {
            assert!(branch.upper_bound(|_| 120., minimum).is_none());
        }
        let bounded = branch.upper_bound(|_| 120., 3420.).unwrap();
        assert!(bounded > 120. / 57.);
    }

    #[test]
    fn fit_bound_dominates_sampled_durations_and_negative_fit_scores() {
        let mut branch =
            Branch::new(vec![0, 1, 2, 3, 4], vec![Some(0), Some(1), Some(2), None]).unwrap();
        branch.locked = 3;
        let known_quality = [12., 0., 80.];
        let known_fit = [50., 5., -10.];
        let bound = branch
            .upper_bound_with_fit(|i| known_quality[i], |i| known_fit[i], 3420., 3600.)
            .unwrap();
        let quality_only = branch.upper_bound(|i| known_quality[i], 3420.).unwrap();
        assert!(bound < quality_only);
        for future_quality in [0., 40., 120.] {
            for future_fit in [-10., 5., 25., 100.] {
                for duration in [3420., 3500., 3600.] {
                    for other_penalties in [0., 2., 50.] {
                        let shortfall = 55. + (25.0_f64 - future_fit).max(0.);
                        let actual =
                            (92. + future_quality - shortfall) / (duration / 60.) - other_penalties;
                        assert!(actual <= bound);
                    }
                }
            }
        }
    }

    #[test]
    fn committed_weak_blends_can_be_pruned_against_a_negative_incumbent() {
        let mut branch = Branch::new((0..16).collect(), (0..15).map(Some).collect()).unwrap();
        branch.locked = 15;
        let relaxed = branch.upper_bound(|_| 0., 3420.).unwrap();
        let tighter = branch
            .upper_bound_with_fit(|_| 0., |_| 5., 3420., 3600.)
            .unwrap();
        let incumbent = -4.;
        assert!(relaxed > incumbent);
        assert!(tighter < incumbent);
        assert!(tighter >= -5.);
    }

    #[test]
    fn fit_bound_never_turns_unknown_fit_or_duration_into_pruning_evidence() {
        let mut branch = Branch::new(vec![0, 1], vec![Some(0)]).unwrap();
        branch.locked = 1;
        for unknown in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(branch
                .upper_bound_with_fit(|_| 0., |_| unknown, 3420., 3600.)
                .is_none());
        }
        for invalid in [0., 3400., f64::NAN, f64::INFINITY] {
            assert!(branch
                .upper_bound_with_fit(|_| 0., |_| 5., 3420., invalid)
                .is_none());
        }
        let root = Branch::new(vec![0, 1], vec![Some(0)]).unwrap();
        assert!(root
            .upper_bound_with_fit(
                |_| panic!("unlocked quality"),
                |_| panic!("unlocked fit"),
                3420.,
                3600.
            )
            .is_some());
    }
}
