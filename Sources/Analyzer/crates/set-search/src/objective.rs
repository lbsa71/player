//! The complete-set objective shared with the final musical validator.

pub const PREFERRED_TRANSITION_FIT: f64 = 25.;
pub const ARC_WEIGHT: f64 = 8.;
pub const DEVELOPMENT_WEIGHT: f64 = 2.;
pub const REPLAY_WEIGHT: f64 = 2.;
pub const SECONDS_PER_MINUTE: f64 = 60.;

#[derive(Debug, Clone, Copy)]
pub struct CompleteScore {
    pub duration_seconds: f64,
    /// Quality credit after subtracting the pacing penalty.
    pub sustained_quality_seconds: f64,
    pub transition_shortfall: f64,
    pub replay_seconds: f64,
    pub arc_error: f64,
    pub development_penalty: f64,
}

impl CompleteScore {
    /// For complete sets, whose duration exceeds one minute.
    pub fn value(self) -> f64 {
        debug_assert!(self.duration_seconds >= SECONDS_PER_MINUTE);
        numerator(
            self.sustained_quality_seconds,
            self.transition_shortfall,
            self.replay_seconds,
        ) / self.duration_seconds
            - composition_cost(self.arc_error, self.development_penalty)
    }
}

pub fn arc_error(weighted_error: f64, scored_seconds: f64) -> f64 {
    weighted_error / scored_seconds.max(1.)
}

pub fn transition_shortfall(fit: f64) -> f64 {
    (PREFERRED_TRANSITION_FIT - fit).max(0.)
}

// These operations are monotone in the bounds used by the search, including
// under ordinary IEEE 754 rounding. Keep their order shared with the evaluator.
pub(crate) fn numerator(sustained: f64, shortfall: f64, replay_seconds: f64) -> f64 {
    SECONDS_PER_MINUTE * (sustained - shortfall) - REPLAY_WEIGHT * replay_seconds
}

pub(crate) fn composition_cost(arc_error: f64, development_penalty: f64) -> f64 {
    ARC_WEIGHT * arc_error + DEVELOPMENT_WEIGHT * development_penalty
}
