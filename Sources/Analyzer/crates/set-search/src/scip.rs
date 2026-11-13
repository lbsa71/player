//! Optional SCIP adapter for sparse binary master problems.
//!
//! The native model is created and dropped within one solve. The reported
//! dual bound is a numerical solver result. Certificate code must establish
//! its validity and coverage before pruning any musical domain with it.

use russcip::prelude::*;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

#[derive(Debug)]
pub struct BinaryModel {
    /// Objective coefficients indexed by variable.
    pub objective: Vec<f64>,
    pub rows: Vec<Row>,
}

#[derive(Debug)]
pub struct Row {
    pub terms: Vec<(usize, f64)>,
    pub lower: Option<f64>,
    pub upper: Option<f64>,
}

#[derive(Debug, PartialEq)]
pub enum Stop {
    Optimal,
    Infeasible,
    TimeLimit,
    Interrupted,
    Other,
}

#[derive(Debug)]
pub struct Candidate {
    pub selected: Vec<usize>,
    pub objective: f64,
}

#[derive(Debug)]
pub struct SolveResult {
    pub stop: Stop,
    pub candidate: Option<Candidate>,
    pub native_upper_bound: Option<f64>,
}

#[derive(Debug)]
pub enum SolveError {
    InvalidModel,
    Native(russcip::Retcode),
}

impl From<russcip::Retcode> for SolveError {
    fn from(error: russcip::Retcode) -> Self {
        Self::Native(error)
    }
}

struct InterruptOnCancellation(Arc<AtomicBool>);

impl Eventhdlr for InterruptOnCancellation {
    fn get_type(&self) -> EventMask {
        EventMask::PRESOLVE_ROUND | EventMask::NODE_FOCUSED | EventMask::LP_SOLVED
    }

    fn execute(&mut self, model: Model<Solving>, _: SCIPEventhdlr, _: Event) {
        if self.0.load(Ordering::Relaxed) {
            // SCIP invokes this callback on the thread that owns the model.
            unsafe { russcip::ffi::SCIPinterruptSolve(model.scip_ptr()) };
        }
    }
}

pub fn solve(
    problem: &BinaryModel,
    time_limit: Duration,
    cancelled: &Arc<AtomicBool>,
) -> Result<SolveResult, SolveError> {
    if problem.objective.iter().any(|c| !c.is_finite())
        || problem.rows.iter().any(|row| {
            row.lower.is_some_and(|v| !v.is_finite())
                || row.upper.is_some_and(|v| !v.is_finite())
                || row.lower.zip(row.upper).is_some_and(|(lo, hi)| lo > hi)
                || row.terms.iter().any(|&(index, coefficient)| {
                    index >= problem.objective.len() || !coefficient.is_finite()
                })
        })
    {
        return Err(SolveError::InvalidModel);
    }

    if cancelled.load(Ordering::Relaxed) {
        return Ok(SolveResult {
            stop: Stop::Interrupted,
            candidate: None,
            native_upper_bound: None,
        });
    }

    let mut model = Model::try_new()?
        .set_bool_param("misc/catchctrlc", false)?
        .set_real_param("limits/time", time_limit.as_secs_f64())?
        .include_default_plugins()
        .create_prob("set-search")
        .maximize()
        .hide_output();
    // The C API expects SCIP's finite infinity sentinel for open row bounds.
    // IEEE infinity trips assertions in the source-built native library.
    let infinity = unsafe { russcip::ffi::SCIPinfinity(model.scip_ptr()) };
    let variables: Vec<_> = problem
        .objective
        .iter()
        .map(|&coefficient| model.add(var().bin().obj(coefficient)))
        .collect();

    for row in &problem.rows {
        model.add(
            cons()
                .expr(row.terms.iter().map(|&(i, c)| (&variables[i], c)))
                .bounds(
                    row.lower.unwrap_or(-infinity),
                    row.upper.unwrap_or(infinity),
                ),
        );
    }

    model.include_eventhdlr(
        "set-search-cancel",
        "Observe the application's cancellation flag",
        Box::new(InterruptOnCancellation(Arc::clone(cancelled))),
    );
    let solved = model.try_solve()?;
    let stop = match solved.status() {
        Status::Optimal => Stop::Optimal,
        Status::Infeasible => Stop::Infeasible,
        Status::TimeLimit => Stop::TimeLimit,
        Status::UserInterrupt => Stop::Interrupted,
        _ => Stop::Other,
    };
    let candidate = solved.best_sol().map(|solution| Candidate {
        selected: variables
            .iter()
            .enumerate()
            .filter_map(|(i, var)| (solution.val(var) > 0.5).then_some(i))
            .collect(),
        objective: solution.obj_val(),
    });
    let bound = solved.best_bound();
    Ok(SolveResult {
        stop,
        candidate,
        native_upper_bound: (bound.is_finite() && bound.abs() < infinity).then_some(bound),
    })
}
