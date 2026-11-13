#![cfg(feature = "scip")]

use mixlib_set_search::scip::{solve, BinaryModel, Row, Stop};
use std::{
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};

fn exactly(variables: &[usize], value: f64) -> Row {
    Row {
        terms: variables.iter().map(|&v| (v, 1.)).collect(),
        lower: Some(value),
        upper: Some(value),
    }
}

#[test]
fn re_solve_after_excluding_a_candidate_from_a_small_route_model() {
    // Four paths choose a start passage and a finish passage. Exclude the
    // incompatible pair and independently enumerate the allowed assignments.
    let mut model = BinaryModel {
        objective: vec![4., 9., 8., 11.],
        rows: vec![
            exactly(&[0, 1], 1.),
            exactly(&[2, 3], 1.),
            Row {
                terms: vec![(1, 1.), (3, 1.)],
                lower: None,
                upper: Some(1.),
            },
        ],
    };
    let mut legal = [(0, 2), (0, 3), (1, 2)];
    legal.sort_by_key(|&(i, j)| -(model.objective[i] + model.objective[j]) as i64);
    for (i, j) in legal {
        let result = solve(&model, Duration::from_secs(2), &Arc::default()).unwrap();
        assert_eq!(result.stop, Stop::Optimal);
        let candidate = result.candidate.unwrap();
        let expected = model.objective[i] + model.objective[j];
        assert_eq!(candidate.selected, vec![i, j]);
        assert_eq!(candidate.objective, expected);
        assert_eq!(result.native_upper_bound, Some(expected));
        model.rows.push(Row {
            terms: vec![(i, 1.), (j, 1.)],
            lower: None,
            upper: Some(1.),
        });
    }
    let result = solve(&model, Duration::from_secs(2), &Arc::default()).unwrap();
    assert_eq!(result.stop, Stop::Infeasible);
    assert!(result.candidate.is_none());
    assert!(result.native_upper_bound.is_none());
}

#[test]
fn time_limit_is_reported_separately_from_infeasibility() {
    let model = BinaryModel {
        objective: vec![5., 8.],
        rows: vec![exactly(&[0, 1], 1.)],
    };
    let result = solve(&model, Duration::ZERO, &Arc::default()).unwrap();
    assert_eq!(result.stop, Stop::TimeLimit);
}

#[test]
fn an_already_cancelled_search_reports_no_native_bound() {
    let model = BinaryModel {
        objective: vec![1.],
        rows: vec![exactly(&[0], 1.)],
    };
    let result = solve(
        &model,
        Duration::from_secs(2),
        &Arc::new(AtomicBool::new(true)),
    )
    .unwrap();
    assert_eq!(result.stop, Stop::Interrupted);
    assert!(result.candidate.is_none());
    assert!(result.native_upper_bound.is_none());
}
