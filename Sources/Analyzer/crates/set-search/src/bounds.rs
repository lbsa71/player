//! Bounds on the complete-set score. Callers own proofs of input bounds.

use crate::objective::{composition_cost, numerator, SECONDS_PER_MINUTE};

#[derive(Debug, Clone, Copy)]
pub struct ScoreEnvelope {
    pub min_duration_seconds: f64,
    pub max_duration_seconds: f64,
    pub sustained_quality_upper: f64,
    pub transition_shortfall_lower: f64,
    pub replay_seconds_lower: f64,
    /// A lower bound on weighted error divided by max(scored seconds, 1).
    pub arc_error_lower: f64,
    pub development_penalty_lower: f64,
}

impl ScoreEnvelope {
    /// Invalid or non-finite bounds provide no pruning evidence.
    pub fn upper_bound(self) -> Option<f64> {
        if self.min_duration_seconds < SECONDS_PER_MINUTE
            || self.max_duration_seconds < self.min_duration_seconds
            || !self.sustained_quality_upper.is_finite()
            || [
                self.min_duration_seconds,
                self.max_duration_seconds,
                self.transition_shortfall_lower,
                self.replay_seconds_lower,
                self.arc_error_lower,
                self.development_penalty_lower,
            ]
            .into_iter()
            .any(|value| !value.is_finite() || value < 0.)
        {
            return None;
        }

        let numerator_upper = numerator(
            self.sustained_quality_upper,
            self.transition_shortfall_lower,
            self.replay_seconds_lower,
        );
        let duration = if numerator_upper < 0. {
            self.max_duration_seconds
        } else {
            self.min_duration_seconds
        };
        let upper = numerator_upper / duration
            - composition_cost(self.arc_error_lower, self.development_penalty_lower);
        upper.is_finite().then_some(upper)
    }
}
