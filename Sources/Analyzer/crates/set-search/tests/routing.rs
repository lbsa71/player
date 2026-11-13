#![cfg(feature = "scip")]

use mixlib_set_search::{
    model::{Domain, Frames, Join, Passage, PrefixConflict, RequiredPair, RouteExclusion},
    scip::{solve, Stop},
};
use std::{sync::Arc, time::Duration};

fn exact(frames: u64) -> Frames {
    Frames {
        min: frames,
        max: frames,
    }
}

#[test]
fn rejected_prefix_does_not_exclude_same_joins_after_another_opening() {
    let domain = Domain {
        passages: (0..4)
            .map(|track| Passage {
                track,
                frames: exact(100),
                can_open: track < 2,
                can_close: track == 3,
            })
            .collect(),
        joins: vec![
            edge(0, 1, -1.),
            edge(1, 2, 20.),
            edge(1, 2, 17.),
            edge(2, 3, 15.),
        ],
        required_pairs: vec![],
        clock: Frames { min: 280, max: 370 },
        minimum_tracks: 3,
    };
    let solve_master = |conflicts: &[PrefixConflict]| {
        solve(
            &domain.master_with_conflicts(conflicts).unwrap(),
            Duration::from_secs(2),
            &Arc::default(),
        )
        .unwrap()
        .candidate
        .unwrap()
    };
    assert_eq!(solve_master(&[]).objective, 35.);
    let result = solve_master(&[PrefixConflict { joins: vec![1] }]);
    assert_eq!(result.objective, 34.);
    assert!(result.selected.starts_with(&[0, 1, 2, 3, 4, 5, 7]));
}

#[test]
fn handing_off_a_complete_route_preserves_extensions_at_either_bookend() {
    let mut domain = Domain {
        passages: (0..5)
            .map(|track| Passage {
                track,
                frames: exact(100),
                can_open: track <= 1,
                can_close: track >= 3,
            })
            .collect(),
        joins: vec![
            edge(0, 1, -2.),
            edge(1, 2, 20.),
            edge(2, 3, 20.),
            edge(3, 4, -1.),
        ],
        required_pairs: vec![],
        clock: Frames { min: 280, max: 370 },
        minimum_tracks: 3,
    };
    let solve_master = |domain: &Domain, excluded: &[RouteExclusion]| {
        let result = solve(
            &domain.master_with_restrictions(&[], excluded).unwrap(),
            Duration::from_secs(2),
            &Arc::default(),
        )
        .unwrap();
        assert_eq!(result.stop, Stop::Optimal);
        result.candidate.unwrap().objective
    };
    assert_eq!(solve_master(&domain, &[]), 40.);
    let excluded = [RouteExclusion { joins: vec![1, 2] }];
    assert_eq!(solve_master(&domain, &excluded), 39.);
    domain.passages[4].can_close = false;
    assert_eq!(solve_master(&domain, &excluded), 38.);
}

fn edge(from: usize, to: usize, score: f64) -> Join {
    Join {
        from_passage: from,
        to_passage: to,
        overlap: exact(10),
        optimistic_surplus: score,
    }
}

/// Exhaustively follow the concrete cue graph without the model's flow,
/// position variables, or linear constraints.
fn enumerate(domain: &Domain, path: &mut Vec<usize>, choices: &mut Vec<usize>, best: &mut f64) {
    let tail = *path.last().unwrap();
    if path.len() >= domain.minimum_tracks && domain.passages[tail].can_close {
        let frames = path
            .iter()
            .map(|&p| domain.passages[p].frames.min)
            .sum::<u64>()
            - choices
                .iter()
                .map(|&j| domain.joins[j].overlap.min)
                .sum::<u64>();
        let pairs_ok = domain.required_pairs.iter().all(|pair| {
            path.windows(2).any(|w| {
                let a = domain.passages[w[0]].track;
                let b = domain.passages[w[1]].track;
                (a, b) == (pair.first_track, pair.second_track)
                    || (pair.either_order && (b, a) == (pair.first_track, pair.second_track))
            })
        });
        if pairs_ok && (domain.clock.min..=domain.clock.max).contains(&frames) {
            *best = best.max(
                choices
                    .iter()
                    .map(|&j| domain.joins[j].optimistic_surplus)
                    .sum(),
            );
        }
    }
    for (e, edge) in domain
        .joins
        .iter()
        .enumerate()
        .filter(|(_, e)| e.from_passage == tail)
    {
        let to = edge.to_passage;
        if path
            .iter()
            .any(|&p| domain.passages[p].track == domain.passages[to].track)
        {
            continue;
        }
        path.push(to);
        choices.push(e);
        enumerate(domain, path, choices, best);
        path.pop();
        choices.pop();
    }
}

#[test]
fn route_master_matches_exhaustive_paths_and_rejects_detached_rewards() {
    // Passage 2 is a second entry into track 1. A superficially stronger
    // incoming edge into passage 1 cannot use passage 2's outgoing edge.
    // The lucrative 5<->6 cycle is disconnected from the bookends.
    let mut domain = Domain {
        passages: [0, 1, 1, 2, 3, 4, 5]
            .into_iter()
            .enumerate()
            .map(|(i, track)| Passage {
                track,
                frames: exact(100),
                can_open: i == 0,
                can_close: i == 4,
            })
            .collect(),
        joins: vec![
            edge(0, 1, 20.),
            edge(0, 2, 4.),
            edge(0, 2, 3.),
            edge(0, 3, 7.),
            edge(1, 4, 1.),
            edge(2, 3, 15.),
            edge(3, 2, 10.),
            edge(2, 4, 2.),
            edge(3, 4, 2.),
            edge(5, 6, 100.),
            edge(6, 5, 100.),
        ],
        required_pairs: vec![RequiredPair {
            first_track: 2,
            second_track: 1,
            either_order: true,
        }],
        clock: exact(370),
        minimum_tracks: 3,
    };
    for (either_order, expected) in [(true, 21.), (false, 19.)] {
        domain.required_pairs[0].either_order = either_order;
        let mut best = f64::NEG_INFINITY;
        enumerate(&domain, &mut vec![0], &mut Vec::new(), &mut best);
        assert_eq!(best, expected);

        let result = solve(
            &domain.master().unwrap(),
            Duration::from_secs(2),
            &Arc::default(),
        )
        .unwrap();
        assert_eq!(result.stop, Stop::Optimal);
        let candidate = result.candidate.unwrap();
        assert_eq!(candidate.objective, best);
        assert!(!candidate.selected.contains(&5));
        assert!(!candidate.selected.contains(&6));
    }
}
