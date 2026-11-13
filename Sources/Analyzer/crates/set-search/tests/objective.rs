use mixlib_set_search::{
    bounds::ScoreEnvelope,
    objective::{arc_error, transition_shortfall, CompleteScore},
};

#[test]
fn optimistic_score_covers_every_small_completion() {
    for quality_upper in [-9., 0., 80.] {
        let envelope = ScoreEnvelope {
            min_duration_seconds: 3420.,
            max_duration_seconds: 3600.,
            sustained_quality_upper: quality_upper,
            transition_shortfall_lower: 20.,
            replay_seconds_lower: 9.,
            arc_error_lower: 0.03,
            development_penalty_lower: 0.08,
        };
        let upper = envelope.upper_bound().unwrap();
        let mut best = f64::NEG_INFINITY;
        for duration in [3420., 3500., 3600.] {
            for quality in [quality_upper - 3., quality_upper] {
                for fit in [0., 5.] {
                    for replay in [9., 15.] {
                        for arc in [0.03, 0.07] {
                            for development in [0.08, 0.2] {
                                let score = CompleteScore {
                                    duration_seconds: duration,
                                    sustained_quality_seconds: quality,
                                    transition_shortfall: transition_shortfall(fit),
                                    replay_seconds: replay,
                                    arc_error: arc,
                                    development_penalty: development,
                                }
                                .value();
                                assert!(score <= upper);
                                best = best.max(score);
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(upper, best);
    }
}

#[test]
fn score_matches_the_existing_hour_and_two_hour_normalization() {
    for duration in [3420., 3599., 7020., 7200.] {
        let inputs = CompleteScore {
            duration_seconds: duration,
            sustained_quality_seconds: 20.7,
            transition_shortfall: 77.3,
            replay_seconds: 210.2,
            arc_error: arc_error(111.3, 3301.5),
            development_penalty: 0.15,
        };
        let legacy = 20.7 / (duration / 60.)
            - 77.3 / (duration / 60.)
            - (8. * (111.3 / 3301.5) + 2. * 0.15)
            - 2. * (210.2 / duration);
        assert!((inputs.value() - legacy).abs() < 1e-12);
    }
    assert_eq!(arc_error(0.125, 0.25), 0.125);
}
