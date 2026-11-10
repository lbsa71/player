use mixlib_audio::{PreparedSourceEdit, StereoEffectChain};
use mixlib_types::*;
use std::f64::consts::TAU;

const RATE: u32 = 8000;
fn curve(start: u64, end: u64, a: f64, b: f64) -> Vec<ScalarPoint> {
    vec![
        ScalarPoint {
            output_frame: start,
            value: a,
        },
        ScalarPoint {
            output_frame: end,
            value: b,
        },
    ]
}
fn effect(kind: EffectKind, input_end: u64, end: u64) -> EffectSpec {
    EffectSpec {
        placement: 0,
        lane: StemLane::Stereo,
        start_frame: 0,
        input_end_frame: input_end,
        end_frame: end,
        wet: curve(0, end, 1., 1.),
        kind,
    }
}
fn run(spec: &EffectSpec, input: impl Fn(u64) -> [f32; 2]) -> Vec<[f32; 2]> {
    let mut chain = StereoEffectChain::new(std::slice::from_ref(spec), RATE).unwrap();
    (0..spec.end_frame)
        .map(|i| chain.process(i, input(i)).unwrap())
        .collect()
}
fn energy(samples: &[[f32; 2]]) -> f64 {
    samples.iter().map(|s| (s[0] as f64).powi(2)).sum::<f64>() / samples.len() as f64
}
fn tone(hz: f64, frame: u64) -> [f32; 2] {
    let v = (TAU * hz * frame as f64 / RATE as f64).sin() as f32 * 0.25;
    [v, v]
}
fn projection(samples: &[[f32; 2]], hz: f64) -> f64 {
    let mut real = 0.;
    let mut imag = 0.;
    for (i, s) in samples.iter().enumerate() {
        let phase = TAU * hz * i as f64 / RATE as f64;
        real += s[0] as f64 * phase.cos();
        imag += s[0] as f64 * phase.sin();
    }
    (real * real + imag * imag).sqrt() / samples.len() as f64
}

#[test]
fn filters_remove_the_correct_frequency_region_and_preserve_stereo() {
    for (mode, pass, reject) in [
        (FilterMode::LowPass, 100., 2500.),
        (FilterMode::HighPass, 2500., 100.),
    ] {
        let spec = effect(
            EffectKind::Filter {
                mode,
                cutoff_hz: curve(0, 16000, 700., 700.),
                resonance: 0.707,
            },
            16000,
            16000,
        );
        let passed = run(&spec, |i| tone(pass, i));
        let removed = run(&spec, |i| tone(reject, i));
        assert!(energy(&passed[4000..]) > energy(&removed[4000..]) * 100.);
        assert!(passed.iter().all(|s| s[0] == s[1]));
    }
    let sweep = effect(
        EffectKind::Filter {
            mode: FilterMode::LowPass,
            cutoff_hz: curve(0, 16000, 200., 3000.),
            resonance: 0.707,
        },
        16000,
        16000,
    );
    let output = run(&sweep, |i| tone(1600., i));
    assert!(energy(&output[12000..15000]) > energy(&output[1000..3000]) * 20.);
}

#[test]
fn delay_places_repeats_at_the_written_frames_and_clears_its_tail() {
    let spec = effect(
        EffectKind::Delay {
            delay_frames: curve(0, 4000, 400., 400.),
            feedback: 0.5,
            freeze_at: None,
            high_pass_hz: 0.,
            low_pass_hz: 3500.,
        },
        100,
        4000,
    );
    let output = run(&spec, |i| if i == 0 { [0.5; 2] } else { [0.; 2] });
    assert!(energy(&output[..399]) < 1e-12);
    let first = energy(&output[400..440]);
    let second = energy(&output[800..840]);
    assert!(first > 0.001);
    assert!(second > first * 0.15 && second < first * 0.35);
    assert_eq!(output.last().unwrap(), &[0.; 2]);
}

#[test]
fn freeze_closes_capture_and_retains_bounded_audio_until_its_release() {
    for kind in [
        EffectKind::Delay {
            delay_frames: curve(0, 6000, 250., 250.),
            feedback: 0.5,
            freeze_at: Some(500),
            high_pass_hz: 0.,
            low_pass_hz: 3900.,
        },
        EffectKind::Reverb {
            decay_seconds: 0.8,
            freeze_at: Some(500),
            high_pass_hz: 0.,
            low_pass_hz: 3500.,
        },
    ] {
        let spec = effect(kind, 1000, 6000);
        let baseline = run(&spec, |i| if i == 100 { [0.3; 2] } else { [0.; 2] });
        let late = run(&spec, |i| {
            if i == 100 {
                [0.3; 2]
            } else if (600..800).contains(&i) {
                [0.8; 2]
            } else {
                [0.; 2]
            }
        });
        assert_eq!(baseline, late, "frozen processors must not admit new input");
        assert!(
            energy(&baseline[3000..5000]) > 1e-7,
            "freeze lost its captured field"
        );
        assert_eq!(baseline.last().unwrap(), &[0.; 2]);
        assert!(baseline
            .iter()
            .flatten()
            .all(|v| v.is_finite() && v.abs() < 1.));
    }
}

#[test]
fn reverb_spreads_a_hit_and_decays_without_spontaneous_energy() {
    let spec = effect(
        EffectKind::Reverb {
            decay_seconds: 0.5,
            freeze_at: None,
            high_pass_hz: 30.,
            low_pass_hz: 3000.,
        },
        100,
        10000,
    );
    let output = run(&spec, |i| if i == 10 { [0.5; 2] } else { [0.; 2] });
    assert!(
        output[200..2000]
            .iter()
            .filter(|x| x[0].abs() > 1e-5)
            .count()
            > 100
    );
    assert!(energy(&output[1000..2500]) > energy(&output[6500..8000]) * 50.);
    let silent = run(&spec, |_| [0.; 2]);
    assert!(silent.iter().all(|x| *x == [0.; 2]));
}

#[test]
fn gate_noise_and_reset_have_exact_repeatable_boundaries() {
    let spec = effect(
        EffectKind::Gate {
            period_frames: 800,
            phase_frames: 0,
            duty_cycle: 0.5,
            smoothing_frames: 40,
        },
        3200,
        3200,
    );
    let output = run(&spec, |_| [0.2; 2]);
    assert!((output[200][0] - 0.2).abs() < 1e-6);
    assert_eq!(output[500], [0.; 2]);
    assert_eq!(&output[..800], &output[800..1600]);
    assert!(output.windows(2).all(|w| (w[1][0] - w[0][0]).abs() < 0.01));
    let spec = effect(
        EffectKind::Noise {
            seed: 123,
            gain: 0.1,
        },
        1000,
        1000,
    );
    let mut chain = StereoEffectChain::new(&[spec], RATE).unwrap();
    let first: Vec<_> = (0..1000)
        .map(|i| chain.process(i, [0.; 2]).unwrap())
        .collect();
    chain.reset();
    let second: Vec<_> = (0..1000)
        .map(|i| chain.process(i, [0.; 2]).unwrap())
        .collect();
    assert_eq!(first, second);
    assert_eq!(first[0], [0.; 2]);
    assert_eq!(first[999], [0.; 2]);
    assert!(first.iter().all(|s| s[0] == s[1] && s[0].abs() <= 0.1));
    assert!(energy(&first) > 0.001);
}

#[test]
fn opposite_gate_phases_preserve_a_continuous_rhythmic_foundation() {
    let a = effect(
        EffectKind::Gate {
            period_frames: 800,
            phase_frames: 0,
            duty_cycle: 0.5,
            smoothing_frames: 40,
        },
        3200,
        3200,
    );
    let b = effect(
        EffectKind::Gate {
            period_frames: 800,
            phase_frames: 400,
            duty_cycle: 0.5,
            smoothing_frames: 40,
        },
        3200,
        3200,
    );
    let a = run(&a, |_| [0.2; 2]);
    let b = run(&b, |_| [0.2; 2]);
    assert!(a
        .iter()
        .zip(b.iter())
        .all(|(a, b)| (a[0] + b[0] - 0.2).abs() < 1e-6));
}

#[test]
fn spiral_compensates_pitch_engine_latency_in_its_written_delay() {
    let spec = effect(
        EffectKind::Spiral {
            delay_frames: 2000.,
            feedback: 0.,
            pitch_semitones: curve(0, 8000, 0., 0.),
        },
        4000,
        8000,
    );
    let output = run(&spec, |i| if i == 100 { [0.25; 2] } else { [0.; 2] });
    let peak = output
        .iter()
        .enumerate()
        .max_by(|a, b| a.1[0].abs().total_cmp(&b.1[0].abs()))
        .unwrap()
        .0;
    assert!(
        peak.abs_diff(2100) <= 2,
        "delayed hit at {peak}, expected 2100"
    );
}

#[test]
fn spiral_transposes_its_repeats_and_rejects_unbounded_state() {
    let spec = effect(
        EffectKind::Spiral {
            delay_frames: 2000.,
            feedback: 0.,
            pitch_semitones: curve(0, 16000, 12., 12.),
        },
        12000,
        16000,
    );
    let output = run(&spec, |i| tone(220., i));
    let actual = projection(&output[4000..10000], 440.);
    let original = projection(&output[4000..10000], 220.);
    assert!(
        actual > 0.04 && actual > original * 8.,
        "octave component {actual}, original {original}"
    );
    assert_eq!(output[15999], [0.; 2]);
    let mut bad = spec;
    bad.kind = EffectKind::Delay {
        delay_frames: curve(0, 16000, 20., 20.),
        feedback: 1.01,
        freeze_at: None,
        high_pass_hz: 0.,
        low_pass_hz: 3000.,
    };
    assert!(StereoEffectChain::new(&[bad], RATE).is_err());
}

#[test]
fn preview_seeks_require_replay_and_nonfinite_input_is_rejected() {
    let spec = effect(
        EffectKind::Noise {
            seed: 1,
            gain: 0.05,
        },
        1000,
        1000,
    );
    let mut chain = StereoEffectChain::new(&[spec], RATE).unwrap();
    assert_eq!(
        chain.process(100, [0.; 2]).unwrap_err().code,
        "EFFECT_MISSING_PREROLL"
    );
    chain.process(0, [0.; 2]).unwrap();
    assert_eq!(
        chain.process(2, [0.; 2]).unwrap_err().code,
        "EFFECT_DISCONTINUITY"
    );
    assert_eq!(
        chain.process(1, [f32::NAN, 0.]).unwrap_err().code,
        "EFFECT_NONFINITE"
    );
}

fn edit(kind: SourceEditKind, source_frames: u64, output_frames: u64) -> SourceEditSpec {
    SourceEditSpec {
        placement: 0,
        lane: StemLane::Stereo,
        source_sample_rate_hz: RATE,
        source_start_frame: 1000,
        source_end_frame: 1000 + source_frames,
        output_start_frame: 2000,
        output_end_frame: 2000 + output_frames,
        declick_frames: 16,
        transport: SourceTransport::ContinueUnderlying,
        kind,
    }
}
fn capture(frames: u64) -> Vec<[f32; 2]> {
    (0..frames).map(|i| tone(440., i)).collect()
}

#[test]
fn loop_repeats_the_written_period_and_preserves_pitch_when_stretched() {
    let spec = edit(
        SourceEditKind::Loop {
            period_frames: 2400,
        },
        1600,
        9600,
    );
    let prepared =
        PreparedSourceEdit::new(&spec, &capture(1600), RATE, &Cancellation::default()).unwrap();
    let output = prepared.frames();
    assert_eq!(&output[2600..4500], &output[5000..6900]);
    let original = projection(&output[2600..4500], 440.);
    let retimed = projection(&output[2600..4500], 440. / 1.5);
    assert!(
        original > 0.05 && original > retimed * 10.,
        "pitch-preserved {original}, resampled {retimed}"
    );
    assert_eq!(output.first().unwrap(), &[0.; 2]);
    assert_eq!(output.last().unwrap(), &[0.; 2]);
    assert_eq!(prepared.sample(1999), None);
    assert_eq!(prepared.sample(11600), None);
}

#[test]
fn loop_sequence_changes_repetition_boundaries_without_changing_tone_pitch() {
    let spec = edit(
        SourceEditKind::LoopSequence {
            periods: curve(2000, 12000, 2000., 1000.),
        },
        1600,
        10000,
    );
    let prepared =
        PreparedSourceEdit::new(&spec, &capture(1600), RATE, &Cancellation::default()).unwrap();
    let out = prepared.frames();
    assert!(projection(&out[6000..9000], 440.) > 0.05);
    assert!(out.iter().flatten().all(|v| v.is_finite() && v.abs() < 0.5));
}

#[test]
fn reverse_slip_brake_backspin_and_scratch_use_distinct_source_traversal() {
    let captured: Vec<_> = (0..4000).map(|i| [i as f32 / 8000.; 2]).collect();
    for kind in [SourceEditKind::Reverse, SourceEditKind::SlipReverse] {
        let spec = edit(kind, 4000, 2000);
        let out =
            PreparedSourceEdit::new(&spec, &captured, RATE, &Cancellation::default()).unwrap();
        assert!(out.frames()[200][0] > out.frames()[1500][0]);
        assert!((out.frames()[200][0] - (3999. - 200.) / 8000.).abs() < 1e-4);
    }
    let mut outputs = vec![];
    for kind in [
        SourceEditKind::Brake { initial_speed: 2. },
        SourceEditKind::Backspin { initial_speed: 2. },
    ] {
        let spec = edit(kind, 4000, 2000);
        let out =
            PreparedSourceEdit::new(&spec, &captured, RATE, &Cancellation::default()).unwrap();
        assert_eq!(out.frames()[1999], [0.; 2]);
        outputs.push(out.frames()[200][0]);
    }
    assert!(outputs[1] > outputs[0] * 4.);
    let scratch = edit(
        SourceEditKind::Scratch {
            positions: vec![
                SourcePositionPoint {
                    output_frame: 2000,
                    source_frame: 1200.,
                },
                SourcePositionPoint {
                    output_frame: 3000,
                    source_frame: 4000.,
                },
                SourcePositionPoint {
                    output_frame: 4000,
                    source_frame: 1200.,
                },
            ],
        },
        4000,
        2000,
    );
    let out = PreparedSourceEdit::new(&scratch, &captured, RATE, &Cancellation::default()).unwrap();
    assert!(out.frames()[1000][0] > out.frames()[200][0] * 3.);
    assert!((out.frames()[200][0] - out.frames()[1800][0]).abs() < 1e-5);
}

#[test]
fn pitch_sampler_retriggers_notes_with_written_pitch_and_no_overlap() {
    let spec = edit(
        SourceEditKind::PitchSequence {
            notes: vec![
                PitchNote {
                    start_frame: 2000,
                    end_frame: 6000,
                    semitones: 0.,
                    gain: 0.8,
                },
                PitchNote {
                    start_frame: 6000,
                    end_frame: 10000,
                    semitones: 12.,
                    gain: 0.8,
                },
            ],
        },
        8000,
        8000,
    );
    let out =
        PreparedSourceEdit::new(&spec, &capture(8000), RATE, &Cancellation::default()).unwrap();
    assert!(projection(&out.frames()[500..3500], 440.) > 0.09);
    assert!(projection(&out.frames()[4500..7500], 880.) > 0.09);
    assert_eq!(out.frames()[4000], [0.; 2]);
    let mut invalid = spec.clone();
    if let SourceEditKind::PitchSequence { notes } = &mut invalid.kind {
        notes[1].start_frame = 5000;
    }
    assert!(
        PreparedSourceEdit::new(&invalid, &capture(8000), RATE, &Cancellation::default()).is_err()
    );
}

#[test]
fn unavailable_capture_oversized_motion_and_cancelled_preparation_fail() {
    let spec = edit(SourceEditKind::Brake { initial_speed: 8. }, 800, 2000);
    assert!(PreparedSourceEdit::new(&spec, &capture(800), RATE, &Cancellation::default()).is_err());
    let spec = edit(SourceEditKind::Loop { period_frames: 800 }, 800, 4000);
    assert!(PreparedSourceEdit::new(&spec, &capture(799), RATE, &Cancellation::default()).is_err());
    let cancel = Cancellation::default();
    cancel.cancel();
    assert_eq!(
        PreparedSourceEdit::new(&spec, &capture(800), RATE, &cancel)
            .err()
            .unwrap()
            .code,
        "CANCELLED"
    );
}
