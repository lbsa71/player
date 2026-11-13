//! Score and proof contracts for duration-set search.
pub mod bounds;
pub mod coverage;
pub mod objective;

#[cfg(feature = "scip")]
pub mod scip;

#[cfg(feature = "scip")]
pub mod model;

pub mod clock_choices;
