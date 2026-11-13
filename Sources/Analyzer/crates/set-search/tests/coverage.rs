use mixlib_set_search::coverage::{
    component_paced_overlap_score_upper, covered_upper, measured_assignment_upper,
    measured_credit_upper, overlap_clock_score_upper, paced_overlap_score_upper,
    unrestricted_credit_upper, JoinCredit, MeasuredJoin, RegionCeiling, TrackClockBounds,
};
use mixlib_set_search::objective::CompleteScore;

#[test]
fn the_union_keeps_unknown_regions_and_retained_ceilings_visible() {
    use RegionCeiling::{Empty, Unknown, Upper};
    assert_eq!(covered_upper(Some(-2.), &[Upper(-4.), Empty]), Some(-2.));
    assert_eq!(
        covered_upper(Some(-2.), &[Upper(-1.99), Empty]),
        Some(-1.99)
    );
    assert_eq!(covered_upper(Some(-2.), &[Upper(-4.), Unknown]), None);
    assert_eq!(covered_upper(Some(-2.), &[Upper(f64::NAN)]), None);
    assert_eq!(covered_upper(None, &[Empty]), None);
    assert_eq!(covered_upper(None, &[Upper(60.)]), Some(60.));
}

#[test]
fn signed_clock_assignment_covers_exhaustive_paths_and_prunes_a_negative_incumbent() {
    let capacities = [1100., 1200., 1400., 1300.];
    let edges = [
        (0, 1, 10., 1., 0.),
        (0, 1, 0., 6., 1.),
        (1, 2, 0., 0., 36.),
        (1, 3, 0., 0., 37.),
        (2, 3, 0., 0., 35.),
        (3, 2, 0., 0., 34.),
        (3, 0, 0., 0., 40.),
    ]
    .map(|(from, to, dense, handoff, fit_shortfall)| MeasuredJoin {
        from,
        to,
        dense,
        handoff,
        fit_shortfall,
    });
    let (minimum, maximum, incumbent) = (3420., 3600., -0.3);
    let bound =
        measured_assignment_upper(&capacities, minimum, maximum, Some(incumbent), edges).unwrap();
    let RegionCeiling::Upper(ceiling) = bound.ceiling else {
        panic!("at least one path fits the clock");
    };
    assert_eq!(bound.minimum_joins, Some(2));
    assert!(ceiling < incumbent);
    let surplus = bound.surplus_upper.unwrap();
    assert!(surplus < 0.);

    // Independent path enumeration. Include both ends of each feasible
    // duration interval to exercise the signed denominator choice.
    let mut stack: Vec<_> = (0..capacities.len())
        .map(|first| (first, 1 << first, capacities[first], 0., 0., 0.))
        .collect();
    let mut checked = 0;
    while let Some((last, used, capacity, dense, handoff, fit)) = stack.pop() {
        if capacity >= minimum {
            for duration in [minimum, capacity.min(maximum)] {
                let actual = CompleteScore {
                    duration_seconds: duration,
                    sustained_quality_seconds: dense + handoff,
                    transition_shortfall: fit,
                    replay_seconds: 0.,
                    arc_error: 0.,
                    development_penalty: 0.,
                }
                .value();
                assert!(actual <= ceiling);
                assert!((actual - incumbent) * duration <= surplus);
                checked += 1;
            }
        }
        for edge in edges
            .iter()
            .filter(|e| e.from == last && used & (1 << e.to) == 0)
        {
            stack.push((
                edge.to,
                used | (1 << edge.to),
                capacity + capacities[edge.to],
                dense + edge.dense,
                handoff + edge.handoff,
                fit + edge.fit_shortfall,
            ));
        }
    }
    assert!(checked > 0);
    assert_eq!(
        measured_assignment_upper(&[100.; 4], minimum, maximum, None, edges)
            .unwrap()
            .ceiling,
        RegionCeiling::Empty
    );
}

#[test]
fn signed_assignment_rounds_outward_for_all_127_joins() {
    let edges = (0..127)
        .map(|from| MeasuredJoin {
            from,
            to: from + 1,
            dense: 0.13_f64.next_down(),
            handoff: 0.28_f64.next_down(),
            fit_shortfall: 0.41_f64.next_up(),
        })
        .collect::<Vec<_>>();
    let bound =
        measured_assignment_upper(&[57.; 128], 7020., 7200., Some(0.01), edges.iter().copied())
            .unwrap();
    let RegionCeiling::Upper(ceiling) = bound.ceiling else {
        panic!("the measured path can reach a two-hour ending");
    };
    assert_eq!(bound.minimum_joins, Some(123));
    let duration = 7199.5;
    let actual = CompleteScore {
        duration_seconds: duration,
        sustained_quality_seconds: edges.iter().map(|e| e.dense).sum::<f64>()
            + edges.iter().map(|e| e.handoff).sum::<f64>(),
        transition_shortfall: edges.iter().map(|e| e.fit_shortfall).sum(),
        replay_seconds: 0.,
        arc_error: 0.,
        development_penalty: 0.,
    }
    .value();
    assert!(actual <= ceiling);
    assert!((actual - 0.01) * duration <= bound.surplus_upper.unwrap());
}

#[test]
fn measured_assignment_ceiling_covers_exhaustive_unique_song_paths() {
    let credit = |from, to, dense, handoff| JoinCredit {
        from,
        to,
        dense,
        handoff,
    };
    let edges = [
        credit(0, 1, 10., 0.),
        credit(0, 1, 9., 0.),
        credit(0, 2, 12., 0.),
        credit(0, 3, 11., 0.),
        credit(1, 2, 0., 4.),
        credit(2, 3, 0., 5.),
        credit(3, 1, 0., 5.),
        credit(3, 0, 3.5, 0.),
        credit(2, 0, f64::EPSILON, 0.),
    ];
    fn visit(edges: &[JoinCredit], last: usize, used: u8, dense: f64, handoff: f64) -> f64 {
        edges
            .iter()
            .filter(|e| e.from == last && used & (1 << e.to) == 0)
            .map(|e| {
                visit(
                    edges,
                    e.to,
                    used | (1 << e.to),
                    dense + e.dense,
                    handoff + e.handoff,
                )
            })
            .fold(dense + handoff, f64::max)
    }
    let exact = (0..4)
        .map(|first| visit(&edges, first, 1 << first, 0., 0.))
        .fold(0., f64::max);
    let bound = measured_credit_upper(4, edges).unwrap();
    assert!(exact <= bound && bound < 30.);
    assert!(bound < unrestricted_credit_upper(3, 120.).unwrap());
    assert_eq!(measured_credit_upper(4, [credit(1, 2, -1., 4.)]), None);
    assert_eq!(measured_credit_upper(4, []), Some(0.));
}

#[test]
fn universal_ceiling_accounts_for_rounded_admission_and_scorer_sums() {
    let cap: f64 = 120. + 1e-6;
    let mathematical_cap = cap.next_up();
    let split = [(cap / 4., cap * 0.75), (1e-15, cap), (cap, 0.)];
    for joins in [0, 1, 14, 72, 127] {
        let actual = (0..joins).fold((0., 0.), |(dense, handoff), i| {
            let (d, h) = split[i % split.len()];
            assert!(d + h <= cap);
            (dense + d, handoff + h)
        });
        assert!(actual.0 + actual.1 <= unrestricted_credit_upper(joins, mathematical_cap).unwrap());
    }
}

#[test]
fn disjoint_overlaps_bound_score_at_both_clocks_and_sample_rates() {
    let slack = 1e-6;
    for (minimum, maximum) in [(3420., 3600.), (7020., 7200.)] {
        for rate in [44100., 48000.] {
            for joins in [1, 14, 72, 127] {
                let upper = overlap_clock_score_upper(minimum, maximum, joins, slack).unwrap();
                assert!((60.0..60.00001).contains(&upper));
                for duration in [minimum, (minimum + maximum) / 2., maximum] {
                    for replay in [0, 12345] {
                        let frames = (duration * rate) as u64 - replay;
                        let (dense, handoff) =
                            (0..joins).fold((0., 0.), |(dense, handoff), join| {
                                let end = frames * (join + 1) as u64 / joins as u64;
                                let start = frames * join as u64 / joins as u64;
                                let admitted = (end - start) as f64 / rate + slack;
                                if join % 3 == 0 {
                                    (dense + admitted, handoff)
                                } else {
                                    (dense, handoff + admitted)
                                }
                            });
                        let score = CompleteScore {
                            duration_seconds: (frames + replay) as f64 / rate,
                            sustained_quality_seconds: dense + handoff,
                            transition_shortfall: 0.,
                            replay_seconds: replay as f64 / rate,
                            arc_error: 0.,
                            development_penalty: 0.,
                        }
                        .value();
                        assert!(
                            score <= upper,
                            "{joins} joins at {duration}s yielded {score} > {upper}"
                        );
                    }
                }
            }
        }
    }
    assert_eq!(overlap_clock_score_upper(3420., 3600., 0, slack), Some(0.));
    assert_eq!(overlap_clock_score_upper(3601., 3600., 10, slack), None);
}

#[test]
fn runway_ceiling_covers_all_unique_song_paths_with_unmeasured_credit() {
    let floors = [14., 9., 30., 8., 20.];
    let (minimum, maximum, cap, slack) = (120., 130., 50., 1e-6);
    let upper = paced_overlap_score_upper(minimum, maximum, cap, slack, &floors).unwrap();
    let unpaced = overlap_clock_score_upper(minimum, maximum, floors.len() - 1, slack).unwrap();
    let mut paths = (0..floors.len()).map(|i| vec![i]).collect::<Vec<_>>();
    let mut highest: f64 = 0.;
    while let Some(path) = paths.pop() {
        if path.len() >= 2 {
            let solo = path[1..path.len() - 1]
                .iter()
                .map(|&i| floors[i])
                .sum::<f64>();
            // Independent enumeration of candidate clocks and split between
            // original audio and replay. Maximize each join's allowed credit.
            for clock in 120..=130 {
                for replay in [0., 5.] {
                    let mut remaining = (f64::from(clock) - solo - replay).max(0.);
                    let mut dense = 0.;
                    let mut handoff = 0.;
                    for join in 1..path.len() {
                        let overlap = remaining.min(cap);
                        let credit = (overlap + slack).min(cap);
                        if join % 2 == 1 {
                            dense += credit;
                        } else {
                            handoff += credit;
                        }
                        remaining -= overlap;
                    }
                    highest = highest.max(
                        CompleteScore {
                            duration_seconds: f64::from(clock),
                            sustained_quality_seconds: dense + handoff,
                            transition_shortfall: 0.,
                            replay_seconds: replay,
                            arc_error: 0.,
                            development_penalty: 0.,
                        }
                        .value(),
                    );
                }
            }
        }
        for next in 0..floors.len() {
            if !path.contains(&next) {
                let mut child = path.clone();
                child.push(next);
                paths.push(child);
            }
        }
    }
    assert!(highest <= upper && upper < unpaced - 7.);
    assert!((upper - highest) < 0.001);
}

#[test]
fn disconnected_route_bound_keeps_one_way_unmeasured_bridges() {
    let floors = [8., 12., 10., 9., 11.];
    let mut edges = [[false; 5]; 5];
    for (from, to) in [(0, 1), (1, 2), (2, 0), (3, 4)] {
        edges[from][to] = true;
    }
    let bound = |edges: &[[bool; 5]; 5]| {
        component_paced_overlap_score_upper(
            120.,
            130.,
            25.,
            1e-6,
            TrackClockBounds {
                solo_seconds_lower: &floors,
                duration_seconds_upper: None,
                preferred_development_lower: None,
            },
            Default::default(),
            |a, b| edges[a][b],
        )
        .unwrap()
    };
    let separated = bound(&edges);
    edges[2][3] = true;
    let connected = bound(&edges);
    assert!((25. ..25.001).contains(&separated));
    assert!((47.5..47.6).contains(&connected));
    for (edges, upper) in [
        (edges, connected),
        (
            {
                edges[2][3] = false;
                edges
            },
            separated,
        ),
    ] {
        let mut paths = (0..floors.len()).map(|i| vec![i]).collect::<Vec<_>>();
        while let Some(path) = paths.pop() {
            let joins = path.len() - 1;
            if joins > 0 {
                let solo = path[1..joins].iter().map(|&i| floors[i]).sum::<f64>();
                for clock in 120..=130 {
                    let time = f64::from(clock);
                    let score = 60. * (25. * joins as f64).min(time - solo) / time;
                    assert!(
                        score <= upper,
                        "{path:?} at {time}s gives {score} > {upper}"
                    );
                }
            }
            for (to, &allowed) in edges[*path.last().unwrap()].iter().enumerate() {
                if allowed && !path.contains(&to) {
                    let mut child = path.clone();
                    child.push(to);
                    paths.push(child);
                }
            }
        }
    }
}

#[test]
fn required_internal_runway_bounds_every_order_of_an_either_direction_chain() {
    use mixlib_set_search::coverage::RequiredRunway;
    let floors = [4., 39., 5., 6., 7.];
    let bounds = |required| {
        component_paced_overlap_score_upper(
            120.,
            130.,
            42.,
            1e-6,
            TrackClockBounds {
                solo_seconds_lower: &floors,
                duration_seconds_upper: None,
                preferred_development_lower: None,
            },
            required,
            |_, _| true,
        )
        .unwrap()
    };
    let relaxed = bounds(RequiredRunway::default());
    let restricted = bounds(RequiredRunway {
        members: 0b111,
        internal: 0b010,
    });
    assert!(restricted < relaxed - 10.);
    let mut paths = (0..floors.len()).map(|i| vec![i]).collect::<Vec<_>>();
    let mut checked = 0;
    while let Some(path) = paths.pop() {
        if path.windows(3).any(|w| w == [0, 1, 2] || w == [2, 1, 0]) {
            let solo = path[1..path.len() - 1]
                .iter()
                .map(|&i| floors[i])
                .sum::<f64>();
            for clock in 120..=130 {
                let duration = f64::from(clock);
                let score = 60. * (42. * (path.len() - 1) as f64).min(duration - solo) / duration;
                assert!(
                    score <= restricted,
                    "{path:?} at {clock}s gives {score} > {restricted}"
                );
                checked += 1;
            }
        }
        for next in 0..floors.len() {
            if !path.contains(&next) {
                let mut next_path = path.clone();
                next_path.push(next);
                paths.push(next_path);
            }
        }
    }
    assert!(checked > 100);

    // The unrelated component has plenty of overlap capacity but does not
    // contain the required songs.
    let separated_floors = [4., 39., 5., 1., 1., 1., 1.];
    let upper = component_paced_overlap_score_upper(
        120.,
        130.,
        35.,
        1e-6,
        TrackClockBounds {
            solo_seconds_lower: &separated_floors,
            duration_seconds_upper: None,
            preferred_development_lower: None,
        },
        RequiredRunway {
            members: 0b111,
            internal: 0b010,
        },
        |a, b| (a < 3) == (b < 3),
    )
    .unwrap();
    let no_requirements = component_paced_overlap_score_upper(
        120.,
        130.,
        35.,
        1e-6,
        TrackClockBounds {
            solo_seconds_lower: &separated_floors,
            duration_seconds_upper: None,
            preferred_development_lower: None,
        },
        RequiredRunway::default(),
        |a, b| (a < 3) == (b < 3),
    )
    .unwrap();
    assert!((35. ..35.001).contains(&upper));
    assert!(no_requirements > upper + 10.);
}

#[test]
fn development_shortage_ceiling_covers_every_tiny_route_and_clock() {
    let preferred = [120., 150., 130., 150., 145.];
    let durations = [100., 105., 150., 160., 162.];
    let runway = [0.; 5];
    let required = mixlib_set_search::coverage::RequiredRunway {
        members: 0b10010,
        internal: 0,
    };
    let bound = |preferred| {
        component_paced_overlap_score_upper(
            380.,
            400.,
            120.,
            0.,
            TrackClockBounds {
                solo_seconds_lower: &runway,
                duration_seconds_upper: Some(&durations),
                preferred_development_lower: preferred,
            },
            required,
            |a, b| a != b,
        )
        .unwrap()
    };
    let ceiling = bound(Some(&preferred));
    assert!(ceiling < bound(None) - 0.1);

    let mut paths = (0..preferred.len()).map(|i| vec![i]).collect::<Vec<_>>();
    while let Some(path) = paths.pop() {
        let used = path.iter().fold(0, |mask, &i| mask | (1 << i));
        for next in (0..preferred.len()).filter(|&i| used & (1 << i) == 0) {
            let mut child = path.clone();
            child.push(next);
            paths.push(child);
        }
        if used & required.members != required.members {
            continue;
        }
        let capacity = path.iter().map(|&i| durations[i]).sum::<f64>();
        for clock in (380..=400).map(f64::from).filter(|&t| t <= capacity) {
            let k = path.len();
            // Sweep different overlap and replay allocations. Half of each
            // adjacent overlap is credited to each song. Some trials use
            // actual preferences above the supplied floors.
            for overlap in [15., 60., 95.] {
                for replay in [0., 20.] {
                    let solo = (clock - replay - overlap * (k - 1) as f64) / k as f64;
                    if solo <= 0. {
                        continue;
                    }
                    let dwell = path
                        .iter()
                        .enumerate()
                        .map(|(i, &track)| {
                            let incident = if i == 0 || i == k - 1 { 1. } else { 2. };
                            let heard = solo + incident * overlap / 2.;
                            let preferred = preferred[track] + if i % 2 == 0 { 15. } else { 0. };
                            ((preferred - heard) / preferred).max(0.).powi(2)
                        })
                        .sum::<f64>()
                        / k as f64;
                    let score = CompleteScore {
                        duration_seconds: clock,
                        sustained_quality_seconds: (k - 1) as f64 * overlap,
                        transition_shortfall: 0.,
                        replay_seconds: replay,
                        arc_error: 0.,
                        development_penalty: dwell,
                    }
                    .value();
                    assert!(
                        score <= ceiling,
                        "{path:?} at {clock}s: {score} > {ceiling}"
                    );
                }
            }
        }
    }
}

#[test]
fn duration_capacity_excludes_short_routes_before_any_audio_evaluation() {
    use mixlib_set_search::coverage::{minimum_capacity_tracks, RequiredRunway};
    let capacities = [40., 45., 45., 40., 45.];
    let floors = [34.; 5];
    let bound = |capacity| {
        component_paced_overlap_score_upper(
            150.,
            160.,
            70.,
            1e-6,
            TrackClockBounds {
                solo_seconds_lower: &floors,
                duration_seconds_upper: capacity,
                preferred_development_lower: None,
            },
            RequiredRunway {
                members: 1,
                internal: 0,
            },
            |_, _| true,
        )
        .unwrap()
    };
    let upper = bound(Some(&capacities));
    assert!(upper < bound(None) - 10.);
    assert_eq!(minimum_capacity_tracks(150., &capacities, 1), Some(Some(4)));
    assert_eq!(
        minimum_capacity_tracks(150., &capacities[..3], 1),
        Some(None)
    );
    assert_eq!(minimum_capacity_tracks(150., &capacities, 1 << 5), None);
    assert_eq!(
        minimum_capacity_tracks(125., &[20., 70., 60.], 0),
        Some(Some(2))
    );
    assert_eq!(
        minimum_capacity_tracks(125., &[20., 70., 60.], 1),
        Some(Some(3))
    );
    for used in 1_u32..1 << capacities.len() {
        if used & 1 == 0 || used.count_ones() < 2 {
            continue;
        }
        let capacity: f64 = capacities
            .iter()
            .enumerate()
            .filter_map(|(index, &seconds)| (used & (1 << index) != 0).then_some(seconds))
            .sum();
        let joins = used.count_ones() - 1;
        for seconds in 150..=160 {
            let seconds = f64::from(seconds);
            if seconds <= capacity {
                let overlap = (seconds - 34. * f64::from(joins - 1)).min(70. * f64::from(joins));
                assert!(60. * overlap / seconds <= upper);
            }
        }
    }
}

#[test]
fn directed_capacity_reaches_a_fixed_point_and_preserves_unmeasured_bridges() {
    use mixlib_set_search::coverage::directed_capacity;
    let capacities = [50., 45., 45.];
    let fork = |a, b| a == 0 && (b == 1 || b == 2);
    let impossible = directed_capacity(120., &capacities, 0, fork).unwrap();
    assert_eq!(impossible.viable_tracks, 0);
    assert_eq!(impossible.minimum_tracks, None);
    assert_eq!(impossible.iterations, 2);

    let bridge = |a, b| fork(a, b) || (a, b) == (1, 2);
    let possible = directed_capacity(120., &capacities, 0b110, bridge).unwrap();
    assert_eq!(possible.viable_tracks, 0b111);
    assert_eq!(possible.minimum_tracks, Some(3));
    assert_eq!(
        directed_capacity(85., &capacities, 0b110, fork)
            .unwrap()
            .viable_tracks,
        0
    );
}

#[test]
fn directed_capacity_keeps_every_legal_path_on_all_four_track_graphs() {
    use mixlib_set_search::coverage::directed_capacity;
    let capacities = [20., 30., 40., 50.];
    let edges = (0..4)
        .flat_map(|a| (0..4).filter(move |&b| a != b).map(move |b| (a, b)))
        .collect::<Vec<_>>();
    for graph in 0_u16..1 << edges.len() {
        let mut adjacency = [0_u128; 4];
        for (bit, &(a, b)) in edges.iter().enumerate() {
            if graph & (1 << bit) != 0 {
                adjacency[a] |= 1 << b;
            }
        }
        for required in [0, 0b0101, 0b1001] {
            let proof = directed_capacity(85., &capacities, required, |a, b| {
                adjacency[a] & (1 << b) != 0
            })
            .unwrap();
            let mut pending = (0..4)
                .map(|i| (vec![i], 1_u128 << i, capacities[i]))
                .collect::<Vec<_>>();
            while let Some((path, used, capacity)) = pending.pop() {
                if used & required == required && capacity >= 85. {
                    assert_eq!(used & proof.viable_tracks, used);
                    assert!(proof.minimum_tracks.unwrap() <= used.count_ones() as usize);
                    for pair in path.windows(2) {
                        let [from, to] = [pair[0], pair[1]];
                        assert!(proof.permits_pair(from, to));
                        let others = used & !((1_u128 << from) | (1_u128 << to));
                        assert_eq!(proof.other_tracks_at_pair(from, to) & others, others);
                    }
                    for chain in path.windows(3) {
                        let inside = chain.iter().fold(0, |mask, &i| mask | (1_u128 << i));
                        let others = used & !inside;
                        let available = proof.outside_tracks(chain[0], chain[2], inside);
                        assert_eq!(available & others, others);
                        assert_eq!(available & inside, 0);
                    }
                }
                let last = *path.last().unwrap();
                for (next, &seconds) in capacities.iter().enumerate() {
                    let bit = 1 << next;
                    if adjacency[last] & !used & bit != 0 {
                        let mut child = path.clone();
                        child.push(next);
                        pending.push((child, used | bit, capacity + seconds));
                    }
                }
            }
        }
    }
}

#[test]
fn direct_shortcut_is_excluded_while_both_endpoints_remain_viable() {
    use mixlib_set_search::coverage::directed_capacity;
    let permits = |a, b| matches!((a, b), (0, 1) | (1, 2) | (0, 2));
    let proof = directed_capacity(120., &[50.; 3], 0, permits).unwrap();
    assert_eq!(proof.viable_tracks, 0b111);
    assert_eq!(proof.excluded_pairs, 1);
    assert!(proof.permits_pair(0, 1));
    assert!(proof.permits_pair(1, 2));
    assert!(!proof.permits_pair(0, 2));
    let required = directed_capacity(85., &[50.; 3], 0b010, permits).unwrap();
    assert_eq!(required.viable_tracks, 0b111);
    assert!(!required.permits_pair(0, 2));
}
