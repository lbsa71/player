#[path = "../../../tests/support.rs"]
mod support;

use mixlib_audio::*;
use mixlib_types::*;
use std::{
    collections::BTreeMap,
    f64::consts::TAU,
    path::{Path, PathBuf},
};

const RATE: u32 = 48000;
const START: u64 = 48000;
const END: u64 = 96000;
const TOTAL: u64 = 144000;

fn scalar(a: u64, b: u64, x: f64, y: f64) -> Vec<ScalarPoint> {
    vec![
        ScalarPoint {
            output_frame: a,
            value: x,
        },
        ScalarPoint {
            output_frame: b,
            value: y,
        },
    ]
}
fn write_audio(path: &Path, frames: u64, sample: impl Fn(usize) -> f64) {
    support::wav(
        path,
        RATE,
        2,
        32,
        true,
        false,
        frames as usize,
        b"CC0",
        |i, _| sample(i),
    );
}
fn tone(hz: f64, frame: usize, gain: f64) -> f64 {
    gain * (TAU * hz * frame as f64 / f64::from(RATE)).sin()
}
fn fixture(
    dir: &Path,
    a: impl Fn(usize) -> f64,
    b: impl Fn(usize) -> f64,
) -> (MixPlan, BTreeMap<String, PathBuf>) {
    let paths = [dir.join("a.wav"), dir.join("b.wav")];
    write_audio(&paths[0], END, a);
    write_audio(&paths[1], END, b);
    let policy = MixingConfig::default();
    let placements = paths
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let start = if i == 0 { 0 } else { START };
            let end = if i == 0 { END } else { TOTAL };
            let points = if i == 0 {
                vec![(0, 1.), (START, 1.), (END, 0.)]
            } else {
                vec![(START, 0.), (END, 1.), (TOTAL, 1.)]
            };
            Placement {
                track_id: if i == 0 { "a".into() } else { "b".into() },
                audio_hash: WavReader::open(path)
                    .unwrap()
                    .hashes(&Cancellation::default())
                    .unwrap()
                    .1,
                audio_revision: 1,
                analysis: FileRef {
                    path: "unused.json".into(),
                    hash: hash_bytes(b"fixture"),
                },
                override_revision: 0,
                source_start_frame: 0,
                source_end_frame: END,
                time_map: TimeMap {
                    header: Header::default(),
                    map_id: format!("map-{i}"),
                    source_sample_rate_hz: RATE,
                    output_sample_rate_hz: RATE,
                    interpolation: "monotone_cubic".into(),
                    anchors: vec![
                        Anchor {
                            source_frame: 0,
                            output_frame: start,
                        },
                        Anchor {
                            source_frame: END,
                            output_frame: end,
                        },
                    ],
                    speed_ratio_limits: [0.94, 1.06],
                    max_speed_change_per_second: 0.02,
                    preserve_pitch: true,
                },
                automation: points
                    .into_iter()
                    .map(|(output_frame, gain_linear)| AutomationPoint {
                        output_frame,
                        gain_linear,
                        bands_linear: [1.; 3],
                        mute: false,
                    })
                    .collect(),
            }
        })
        .collect();
    let plan = MixPlan {
        header: Header::default(),
        plan_id: "tactic-render-test".into(),
        planner_version: "test".into(),
        renderer_version: TACTIC_RENDERER.into(),
        policy_hash: canonical_hash(&policy).unwrap(),
        policy,
        seed: 3,
        objective: "stitch".into(),
        requested_duration_seconds: None,
        complete: true,
        output: OutputFormat {
            sample_rate_hz: RATE,
            channels: 2,
            encoding: "float32".into(),
            dither: "seeded_tpdf_1_lsb".into(),
        },
        drum_intro: None,
        prepared_patches: vec![],
        placements,
        transitions: vec![Transition {
            transition_id: "join".into(),
            from_track_id: "a".into(),
            to_track_id: "b".into(),
            outgoing_region_id: "a-exit".into(),
            incoming_region_id: "b-entry".into(),
            start_output_frame: START,
            end_output_frame: END,
            template: Template::ShortCrossfade,
            costs: BTreeMap::new(),
            fallback_reason: None,
        }],
        total_output_frames: TOTAL,
        crossover_hz: [200., 2500.],
        crossover_design: "complementary_one_pole_v1".into(),
        automation_interpolation: "smoothstep_linear_amplitude".into(),
        diagnostics: vec![],
    };
    (
        plan,
        BTreeMap::from([
            ("a".into(), paths[0].clone()),
            ("b".into(), paths[1].clone()),
        ]),
    )
}
fn lane(placement: usize, lane: StemLane, gain: f64) -> LaneSchedule {
    LaneSchedule {
        placement,
        lane,
        gain: scalar(START, END, gain, gain),
        eq: vec![],
    }
}
fn tactic(id: TacticId) -> TacticPlan {
    TacticPlan {
        phrase_exit: None,
        policy_version: TACTICS_POLICY_VERSION.into(),
        tactic_id: id,
        modifiers: vec![],
        arrangement: TacticArrangement::Handoff,
        character: TacticCharacter::Subtle,
        permission: id.permission(),
        outgoing_placement: 0,
        incoming_placement: 1,
        start_frame: START,
        hold_end_frame: START,
        exit_start_frame: START,
        end_frame: END,
        bar_frames: vec![START, END],
        timing_basis: TacticTimingBasis::Bars,
        bar_bpm: None,
        pulse_ratio: None,
        edit_speed_ratio_limits: [0.94, 1.06],
        lanes: vec![lane(0, StemLane::Stereo, 1.), lane(1, StemLane::Stereo, 0.)],
        stem_assets: vec![],
        effects: vec![],
        source_edits: vec![],
        vocal_occupancy: vec![],
        vocal_guard_frames: 14400,
        evidence: vec!["known instrumental CC0 test fixture".into()],
        selection_reason: "test executes saved schedule".into(),
        emergency: None,
    }
}
fn read(path: &Path) -> Vec<[f32; 2]> {
    let mut wav = WavReader::open(path).unwrap();
    let mut frames = Vec::new();
    loop {
        let block = wav.read_frames(65536).unwrap();
        if block.is_empty() {
            break;
        }
        frames.extend(block);
    }
    frames
}
fn projection(samples: &[[f32; 2]], hz: f64) -> f64 {
    let (mut re, mut im) = (0., 0.);
    for (i, s) in samples.iter().enumerate() {
        let w = TAU * hz * i as f64 / f64::from(RATE);
        re += s[0] as f64 * w.cos();
        im += s[0] as f64 * w.sin();
    }
    (re * re + im * im).sqrt() / samples.len() as f64
}
fn render_to(
    plan: &MixPlan,
    sources: &BTreeMap<String, PathBuf>,
    path: &Path,
    range: Option<(u64, u64)>,
) -> Vec<[f32; 2]> {
    render(plan, sources, path, false, range, &Cancellation::default()).unwrap();
    read(path)
}

#[test]
fn saved_baseline_crossfade_is_gradual_and_rejects_a_disguised_short_cut() {
    let d = tempfile::tempdir().unwrap();
    let (mut plan, sources) = fixture(d.path(), |i| tone(440., i, 0.1), |i| tone(880., i, 0.1));
    let mut join = tactic(TacticId::S13);
    join.arrangement = TacticArrangement::Blend;
    join.bar_frames = (0..=4).map(|i| START + (END - START) * i / 4).collect();
    join.lanes[0].gain = scalar(START, END, 1., 0.);
    join.lanes[1].gain = scalar(START, END, 0., 1.);
    write_tactics(&mut plan, vec![join.clone()]).unwrap();
    let saved: MixPlan = validate_value(&serde_json::to_value(&plan).unwrap()).unwrap();
    let full = render_to(&saved, &sources, &d.path().join("baseline.wav"), None);
    let early = &full[START as usize + 1000..START as usize + 11000];
    let late = &full[END as usize - 11000..END as usize - 1000];
    assert!(projection(early, 440.) > 3. * projection(early, 880.));
    assert!(projection(late, 880.) > 3. * projection(late, 440.));
    let preview = render_to(
        &saved,
        &sources,
        &d.path().join("baseline-preview.wav"),
        Some((START + 7777, END + 3333)),
    );
    assert_eq!(preview, full[START as usize + 7777..END as usize + 3333]);

    let mut shortened = join.clone();
    shortened.bar_frames = vec![START, (START + END) / 2, END];
    assert!(write_tactics(&mut plan, vec![shortened]).is_err());
    join.lanes[0].gain.insert(
        1,
        ScalarPoint {
            output_frame: END - 1000,
            value: 1.,
        },
    );
    assert!(write_tactics(&mut plan, vec![join]).is_err());
}

#[test]
fn saved_filter_plan_changes_spectrum_and_preview_replays_identical_samples() {
    let d = tempfile::tempdir().unwrap();
    let (mut plan, sources) = fixture(
        d.path(),
        |i| tone(100., i, 0.1) + tone(4000., i, 0.1),
        |_| 0.,
    );
    let mut join = tactic(TacticId::S08);
    join.effects.push(EffectSpec {
        placement: 0,
        lane: StemLane::Stereo,
        start_frame: START,
        input_end_frame: END,
        end_frame: END,
        wet: scalar(START, END, 1., 1.),
        kind: EffectKind::Filter {
            mode: FilterMode::LowPass,
            cutoff_hz: scalar(START, END, 500., 500.),
            resonance: 0.707,
        },
    });
    write_tactics(&mut plan, vec![join]).unwrap();
    // Serializing and validating the saved plan is part of the execution path.
    let plan: MixPlan = validate_value(&serde_json::to_value(plan).unwrap()).unwrap();
    let full = render_to(&plan, &sources, &d.path().join("full.wav"), None);
    let region = &full[60000..84000];
    assert!(projection(region, 100.) > 0.045);
    assert!(projection(region, 4000.) < 0.002);
    let first = START + 7777;
    let last = END + 3333;
    let preview = render_to(
        &plan,
        &sources,
        &d.path().join("preview.wav"),
        Some((first, last)),
    );
    assert_eq!(preview, full[first as usize..last as usize]);
}

#[test]
fn captured_echo_is_rendered_after_outgoing_placement_and_clears_before_next_music() {
    let d = tempfile::tempdir().unwrap();
    let (mut plan, sources) = fixture(d.path(), |i| if i == 95000 { 0.3 } else { 0. }, |_| 0.);
    let mut join = tactic(TacticId::S09);
    join.effects.push(EffectSpec {
        placement: 0,
        lane: StemLane::Stereo,
        start_frame: START,
        input_end_frame: END,
        end_frame: 120000,
        wet: scalar(START, 120000, 1., 1.),
        kind: EffectKind::Delay {
            delay_frames: scalar(START, 120000, 9600., 9600.),
            feedback: 0.4,
            freeze_at: None,
            high_pass_hz: 0.,
            low_pass_hz: 18000.,
        },
    });
    write_tactics(&mut plan, vec![join]).unwrap();
    let output = render_to(&plan, &sources, &d.path().join("echo.wav"), None);
    let peak = output[100000..110000]
        .iter()
        .map(|x| x[0].abs())
        .fold(0., f32::max);
    assert!(
        peak > 0.1,
        "outgoing voice was retired before its captured tail played"
    );
    assert!(output[120000..].iter().all(|s| *s == [0.; 2]));
    let preview = render_to(
        &plan,
        &sources,
        &d.path().join("echo-preview.wav"),
        Some((100000, 122000)),
    );
    assert_eq!(preview, output[100000..122000]);
}

#[test]
fn saved_reverse_edit_replaces_audio_and_blends_once_at_its_boundaries() {
    let d = tempfile::tempdir().unwrap();
    let (mut plan, sources) = fixture(d.path(), |i| i as f64 / 1_000_000., |_| 0.);
    let mut join = tactic(TacticId::C12);
    join.source_edits.push(SourceEditSpec {
        placement: 0,
        lane: StemLane::Stereo,
        source_sample_rate_hz: RATE,
        source_start_frame: 53000,
        source_end_frame: 63000,
        output_start_frame: 53000,
        output_end_frame: 63000,
        declick_frames: 48,
        transport: SourceTransport::ContinueUnderlying,
        kind: SourceEditKind::SlipReverse,
    });
    write_tactics(&mut plan, vec![join]).unwrap();
    let output = render_to(&plan, &sources, &d.path().join("reverse.wav"), None);
    assert!((output[55500][0] - 0.060499).abs() < 1e-6);
    assert!((output[53000][0] - 0.053).abs() < 1e-6);
    let t = 1. / 48.;
    let blend = t * t * (3. - 2. * t);
    let expected = 0.062998 * blend + 0.053001 * (1. - blend);
    assert!(
        (output[53001][0] as f64 - expected).abs() < 1e-6,
        "prepared de-click was faded a second time or doubled"
    );
    assert!((output[63000][0] - 0.063).abs() < 1e-6);
}

fn add_stem(
    dir: &Path,
    sources: &mut BTreeMap<String, PathBuf>,
    placement: usize,
    lane: StemLane,
    offset: u64,
    end: u64,
    sample: impl Fn(usize) -> f64,
) -> StemAsset {
    let name = format!("stem-{placement}-{lane:?}.wav");
    let path = dir.join(&name);
    write_audio(&path, end - offset, |i| sample(i + offset as usize));
    let hash = WavReader::open(&path)
        .unwrap()
        .hashes(&Cancellation::default())
        .unwrap()
        .0;
    sources.insert(name.clone(), path);
    StemAsset {
        placement,
        lane,
        audio: FileRef { path: name, hash },
        source_offset_frame: offset,
        source_end_frame: end,
    }
}

#[test]
fn isolated_lane_ownership_uses_offset_assets_without_underlying_full_mix() {
    let d = tempfile::tempdir().unwrap();
    let (mut plan, mut sources) = fixture(
        d.path(),
        |i| tone(100., i, 0.1) + tone(500., i, 0.07),
        |i| tone(2000., i, 0.15) + tone(4000., i, 0.1),
    );
    let mut join = tactic(TacticId::S02);
    join.lanes = vec![lane(0, StemLane::Other, 1.), lane(1, StemLane::Drums, 1.)];
    join.stem_assets.push(add_stem(
        d.path(),
        &mut sources,
        0,
        StemLane::Other,
        START,
        END,
        |i| tone(100., i, 0.1),
    ));
    join.stem_assets.push(add_stem(
        d.path(),
        &mut sources,
        1,
        StemLane::Drums,
        0,
        START,
        |i| tone(2000., i, 0.15),
    ));
    write_tactics(&mut plan, vec![join]).unwrap();
    let output = render_to(&plan, &sources, &d.path().join("stems.wav"), None);
    let region = &output[60000..84000];
    assert!((projection(region, 100.) - 0.05).abs() < 1e-5);
    assert!((projection(region, 2000.) - 0.075).abs() < 1e-5);
    assert!(projection(region, 500.) < 1e-6 && projection(region, 4000.) < 1e-6);
    for frame in [60001, 65003, 82009] {
        let expected = tone(100., frame, 0.1) + tone(2000., frame - START as usize, 0.15);
        assert!((output[frame][0] as f64 - expected).abs() < 1e-6);
    }
}

#[test]
fn m01_wav_holds_six_complete_bars_then_resolves_over_two_with_independent_vocals() {
    let d = tempfile::tempdir().unwrap();
    let (mut plan, mut sources) = fixture(d.path(), |_| 0., |_| 0.);
    // Four beats at 120 BPM: these are actual two-second bars, not shortened
    // fixture frames that merely happen to number eight intervals.
    let bar = 2 * u64::from(RATE);
    let start = bar;
    let hold_end = start + 6 * bar;
    let end = start + 8 * bar;
    let total = end + bar;
    let gain = 0.03;
    let frequencies = [[1000., 80., 400., 3200.], [2000., 160., 800., 4000.]];
    let names = [
        StemLane::Drums,
        StemLane::Bass,
        StemLane::Other,
        StemLane::Vocals,
    ];
    for (side, id) in ["a", "b"].iter().enumerate() {
        write_audio(&sources[*id], end, |i| {
            frequencies[side].iter().map(|hz| tone(*hz, i, gain)).sum()
        });
        let p = &mut plan.placements[side];
        p.audio_hash = WavReader::open(&sources[*id])
            .unwrap()
            .hashes(&Cancellation::default())
            .unwrap()
            .1;
        p.source_end_frame = end;
        p.time_map.anchors = vec![
            Anchor {
                source_frame: 0,
                output_frame: if side == 0 { 0 } else { start },
            },
            Anchor {
                source_frame: end,
                output_frame: if side == 0 { end } else { total },
            },
        ];
        p.automation = if side == 0 {
            vec![(0, 1.), (start, 1.), (end, 0.)]
        } else {
            vec![(start, 0.), (end, 1.), (total, 1.)]
        }
        .into_iter()
        .map(|(output_frame, gain_linear)| AutomationPoint {
            output_frame,
            gain_linear,
            bands_linear: [1.; 3],
            mute: false,
        })
        .collect();
    }
    plan.total_output_frames = total;
    plan.transitions[0].start_output_frame = start;
    plan.transitions[0].end_output_frame = end;
    let mut join = tactic(TacticId::M01);
    join.arrangement = TacticArrangement::Mashup;
    join.start_frame = start;
    join.hold_end_frame = hold_end;
    join.exit_start_frame = hold_end;
    join.end_frame = end;
    join.bar_frames = (0..=8).map(|i| start + i * bar).collect();
    join.lanes.clear();
    // Vocal ownership changes inside the unchanged instrumental mashup. A's
    // phrase lasts five bars; B enters after a real 300 ms silent vocal guard.
    let vocal_fade = u64::from(RATE) / 10;
    let a_vocal_end = start + 5 * bar;
    let b_vocal_start = a_vocal_end + join.vocal_guard_frames;
    for (side, freqs) in frequencies.iter().enumerate() {
        for (index, name) in names.iter().enumerate() {
            let envelope = match (side, name) {
                (0, StemLane::Drums) => vec![(start, 0.), (end, 0.)],
                (1, StemLane::Drums) => vec![(start, 1.), (end, 1.)],
                (0, StemLane::Vocals) => vec![
                    (start, 1.),
                    (a_vocal_end - vocal_fade, 1.),
                    (a_vocal_end, 0.),
                    (end, 0.),
                ],
                (1, StemLane::Vocals) => vec![
                    (start, 0.),
                    (b_vocal_start, 0.),
                    (b_vocal_start + vocal_fade, 1.),
                    (end, 1.),
                ],
                (0, _) => vec![(start, 1.), (hold_end, 1.), (end, 0.)],
                (1, _) => vec![(start, 0.), (hold_end, 0.), (end, 1.)],
                _ => unreachable!(),
            };
            join.lanes.push(LaneSchedule {
                placement: side,
                lane: *name,
                gain: envelope
                    .into_iter()
                    .map(|(output_frame, value)| ScalarPoint {
                        output_frame,
                        value,
                    })
                    .collect(),
                eq: vec![],
            });
            let (offset, source_end) = if side == 0 {
                (start, end)
            } else {
                (0, end - start)
            };
            join.stem_assets.push(add_stem(
                d.path(),
                &mut sources,
                side,
                *name,
                offset,
                source_end,
                |i| tone(freqs[index], i, gain),
            ));
        }
    }
    join.vocal_occupancy = vec![
        VocalOccupancy {
            placement: 0,
            start_frame: start,
            end_frame: a_vocal_end,
            tail_end_frame: a_vocal_end,
            supported_entry: true,
            supported_exit: true,
        },
        VocalOccupancy {
            placement: 1,
            start_frame: b_vocal_start,
            end_frame: end,
            tail_end_frame: end,
            supported_entry: true,
            supported_exit: true,
        },
    ];
    join.evidence = vec!["CC0 synthesis: unique lane identification tones with independent scheduled vocal occupancy".into()];
    join.selection_reason =
        "dense complementary six-bar mashup resolves over bars seven and eight".into();
    write_tactics(&mut plan, vec![join]).unwrap();
    let plan_path = d.path().join("m01-plan.json");
    std::fs::write(&plan_path, serde_json::to_vec(&plan).unwrap()).unwrap();
    let saved: MixPlan =
        validate_value(&serde_json::from_slice(&std::fs::read(plan_path).unwrap()).unwrap())
            .unwrap();
    let output = render_to(
        &saved,
        &sources,
        &d.path().join("m01-six-plus-two.wav"),
        None,
    );
    assert_eq!(output.len(), total as usize);

    let amplitudes = |center: u64| {
        let half = u64::from(RATE) / 8;
        let window = &output[(center - half) as usize..(center + half) as usize];
        frequencies.map(|row| row.map(|hz| projection(window, hz) * 2.))
    };
    // Every held bar, including the end of bar six, must preserve accompaniment
    // level and eliminate A's original drum foundation completely.
    for center in (0..6)
        .map(|i| start + i * bar + bar / 2)
        .chain([hold_end - u64::from(RATE) / 4])
    {
        let a = amplitudes(center);
        assert!(
            a[0][0] < 1e-6,
            "A drums leaked during the mashup at {center}: {}",
            a[0][0]
        );
        assert!(
            (a[1][0] - gain).abs() < 1e-6,
            "B drums moved during the hold at {center}"
        );
        for index in [1, 2] {
            assert!(
                (a[0][index] - gain).abs() < 1e-6,
                "A accompaniment faded before six bars at {center}"
            );
            assert!(
                a[1][index] < 1e-6,
                "B accompaniment entered before six bars at {center}"
            );
        }
    }
    // Both exit bars contain the planned fade, while B drums remain at their
    // original measured amplitude rather than following the main crossfade.
    let exit_without_drums = |frame: usize| {
        let t = (frame as u64 - hold_end) as f64 / (end - hold_end) as f64;
        let incoming = t * t * (3. - 2. * t);
        let b_frame = frame - start as usize;
        (tone(80., frame, gain) + tone(400., frame, gain)) * (1. - incoming)
            + (tone(160., b_frame, gain) + tone(800., b_frame, gain)) * incoming
            + tone(4000., b_frame, gain)
    };
    let mut previous_a = gain;
    let mut previous_b = 0.;
    for center in [hold_end + bar / 4, hold_end + bar, hold_end + bar * 7 / 4] {
        let a = amplitudes(center);
        // Amplitude ramps put accompaniment sidebands into finite Fourier
        // windows. Remove the known accompaniment before measuring drum gain;
        // do not mistake those predictable sidebands for the muted A drums.
        let half = u64::from(RATE) / 8;
        let isolated: Vec<_> = (center - half..center + half)
            .map(|frame| {
                let residual =
                    f64::from(output[frame as usize][0]) - exit_without_drums(frame as usize);
                [residual as f32; 2]
            })
            .collect();
        assert!(
            projection(&isolated, 1000.) * 2. < 1e-7,
            "A drums leaked into the two-bar exit"
        );
        assert!(
            (projection(&isolated, 2000.) * 2. - gain).abs() < 1e-7,
            "B drums were faded during the exit"
        );
        assert!(
            a[0][2] < previous_a && a[1][2] > previous_b,
            "exit did not progress across both bars"
        );
        assert!(
            (a[0][2] + a[1][2] - gain).abs() < 2e-5,
            "accompaniment ownership was not complementary"
        );
        previous_a = a[0][2];
        previous_b = a[1][2];
    }
    let early = amplitudes(start + bar);
    assert!((early[0][3] - gain).abs() < 1e-6 && early[1][3] < 1e-6);
    let guard = amplitudes(a_vocal_end + (b_vocal_start - a_vocal_end) / 2);
    assert!(
        guard[0][3] < 1e-6 && guard[1][3] < 1e-6,
        "vocal guard contains one of the vocal carriers"
    );
    let late_hold = amplitudes(hold_end - u64::from(RATE) / 4);
    assert!(
        late_hold[0][3] < 1e-6 && (late_hold[1][3] - gain).abs() < 1e-6,
        "independent vocal transfer incorrectly waited for the accompaniment fade"
    );
    // Inspect the last quarter-second right up to the exact sixth-bar endpoint,
    // including the final sample, so an early interpolation edge cannot hide
    // between the held-bar spectral windows.
    for (frame, sample) in output
        .iter()
        .enumerate()
        .take(hold_end as usize)
        .skip((hold_end - u64::from(RATE) / 4) as usize)
    {
        let expected = tone(2000., frame - start as usize, gain)
            + tone(80., frame, gain)
            + tone(400., frame, gain)
            + tone(4000., frame - start as usize, gain);
        assert!(
            (f64::from(sample[0]) - expected).abs() < 1e-7,
            "accompaniment moved before the exact sixth-bar endpoint at frame {frame}"
        );
    }
    for (frame, sample) in output
        .iter()
        .enumerate()
        .take((b_vocal_start + vocal_fade) as usize)
        .skip((a_vocal_end - vocal_fade) as usize)
    {
        let frame_u64 = frame as u64;
        let outgoing =
            ((a_vocal_end.saturating_sub(frame_u64)) as f64 / vocal_fade as f64).clamp(0., 1.);
        let incoming =
            ((frame_u64.saturating_sub(b_vocal_start)) as f64 / vocal_fade as f64).clamp(0., 1.);
        assert!(outgoing == 0. || incoming == 0.);
        let expected = tone(2000., frame - start as usize, gain)
            + tone(80., frame, gain)
            + tone(400., frame, gain)
            + tone(3200., frame, gain) * outgoing * outgoing * (3. - 2. * outgoing)
            + tone(4000., frame - start as usize, gain)
                * incoming
                * incoming
                * (3. - 2. * incoming);
        assert!(
            (f64::from(sample[0]) - expected).abs() < 1e-7,
            "independent vocal release, guard or entry differs at frame {frame}"
        );
    }

    // Compare actual PCM throughout the two-bar exit against its independently
    // specified time envelope. This catches a one-bar/early/late fade even if a
    // few spectral snapshots happen to look plausible.
    let mut worst = (0., 0usize);
    for (frame, sample) in output
        .iter()
        .enumerate()
        .take(end as usize)
        .skip(hold_end as usize)
    {
        let b_frame = frame - start as usize;
        let expected = tone(2000., b_frame, gain) + exit_without_drums(frame);
        let error = (f64::from(sample[0]) - expected).abs();
        if error > worst.0 {
            worst = (error, frame);
        }
    }
    assert!(
        worst.0 < 1e-7,
        "rendered two-bar fade differs by {} at frame {}",
        worst.0,
        worst.1
    );
    // Outside tactic ownership the real stereo source must return once, with
    // all B lanes present and none of the outgoing source left underneath it.
    let restored = amplitudes(end + bar / 2);
    assert!(restored[0].iter().all(|a| *a < 1e-6));
    assert!(restored[1].iter().all(|a| (*a - gain).abs() < 1e-6));
    for frame in [end as usize, end as usize + 1, end as usize + 137] {
        let expected = frequencies[1]
            .iter()
            .map(|hz| tone(*hz, frame - start as usize, gain))
            .sum::<f64>();
        assert!(
            (f64::from(output[frame][0]) - expected).abs() < 1e-7,
            "full-mix restoration changed at frame {frame}"
        );
    }
}

#[test]
fn changed_source_and_nonfinite_stem_are_rejected_without_publishing() {
    let d = tempfile::tempdir().unwrap();
    let (mut plan, mut sources) =
        fixture(d.path(), |i| tone(100., i, 0.1), |i| tone(300., i, 0.05));
    write_tactics(&mut plan, vec![tactic(TacticId::S02)]).unwrap();
    write_audio(&sources["a"], END, |i| tone(101., i, 0.1));
    let output = d.path().join("protected.wav");
    std::fs::write(&output, b"previous valid export").unwrap();
    let error = render(
        &plan,
        &sources,
        &output,
        true,
        None,
        &Cancellation::default(),
    )
    .unwrap_err();
    assert_eq!(error.code, "SOURCE_CHANGED");
    assert_eq!(std::fs::read(&output).unwrap(), b"previous valid export");

    plan.placements[0].audio_hash = WavReader::open(&sources["a"])
        .unwrap()
        .hashes(&Cancellation::default())
        .unwrap()
        .1;
    let path = d.path().join("bad-stem.wav");
    write_audio(&path, START, |i| if i == 100 { f64::NAN } else { 0. });
    let mut join = tactic(TacticId::S02);
    join.lanes[0] = lane(0, StemLane::Other, 1.);
    join.stem_assets.push(StemAsset {
        placement: 0,
        lane: StemLane::Other,
        audio: FileRef {
            path: "bad-stem.wav".into(),
            hash: hash_bytes(&std::fs::read(&path).unwrap()),
        },
        source_offset_frame: START,
        source_end_frame: END,
    });
    sources.insert("bad-stem.wav".into(), path);
    write_tactics(&mut plan, vec![join]).unwrap();
    let error = render(
        &plan,
        &sources,
        &output,
        true,
        None,
        &Cancellation::default(),
    )
    .unwrap_err();
    assert_eq!(error.code, "WAV_NON_FINITE");
    assert_eq!(std::fs::read(&output).unwrap(), b"previous valid export");
}

#[test]
fn a_hashed_but_truncated_stem_is_rejected_instead_of_padded_with_silence() {
    let d = tempfile::tempdir().unwrap();
    let (mut plan, mut sources) =
        fixture(d.path(), |i| tone(100., i, 0.1), |i| tone(300., i, 0.05));
    let mut join = tactic(TacticId::S02);
    join.lanes[0] = lane(0, StemLane::Other, 1.);
    let path = d.path().join("short-stem.wav");
    write_audio(&path, 100, |i| tone(100., i, 0.1));
    let hash = WavReader::open(&path)
        .unwrap()
        .hashes(&Cancellation::default())
        .unwrap()
        .0;
    join.stem_assets.push(StemAsset {
        placement: 0,
        lane: StemLane::Other,
        audio: FileRef {
            path: "short-stem.wav".into(),
            hash,
        },
        source_offset_frame: START,
        source_end_frame: END,
    });
    sources.insert("short-stem.wav".into(), path);
    write_tactics(&mut plan, vec![join]).unwrap();
    let output = d.path().join("never-published.wav");
    let error = render(
        &plan,
        &sources,
        &output,
        false,
        None,
        &Cancellation::default(),
    )
    .unwrap_err();
    assert_eq!(error.code, "TACTIC_STEM");
    assert!(!output.exists());
}

#[test]
fn loop_capture_larger_than_one_decoder_block_executes_with_preserved_pitch() {
    let d = tempfile::tempdir().unwrap();
    let (mut plan, sources) = fixture(d.path(), |i| tone(440., i, 0.1), |_| 0.);
    let mut join = tactic(TacticId::S12);
    join.source_edits.push(SourceEditSpec {
        placement: 0,
        lane: StemLane::Stereo,
        source_sample_rate_hz: RATE,
        source_start_frame: 0,
        source_end_frame: END,
        output_start_frame: START,
        output_end_frame: END,
        declick_frames: 48,
        transport: SourceTransport::ContinueUnderlying,
        kind: SourceEditKind::Loop { period_frames: END },
    });
    write_tactics(&mut plan, vec![join]).unwrap();
    let output = render_to(&plan, &sources, &d.path().join("large-loop.wav"), None);
    assert!(projection(&output[60000..84000], 440.) > 0.04);
    assert!(projection(&output[60000..84000], 880.) < 0.001);
}

#[test]
fn changed_stem_hash_is_rejected_without_overwriting_an_export() {
    let d = tempfile::tempdir().unwrap();
    let (mut plan, mut sources) = fixture(d.path(), |i| tone(100., i, 0.1), |_| 0.);
    let mut join = tactic(TacticId::S02);
    join.lanes[0] = lane(0, StemLane::Other, 1.);
    let asset = add_stem(
        d.path(),
        &mut sources,
        0,
        StemLane::Other,
        START,
        END,
        |i| tone(100., i, 0.1),
    );
    write_audio(&sources[&asset.audio.path], START, |i| tone(110., i, 0.1));
    join.stem_assets.push(asset);
    write_tactics(&mut plan, vec![join]).unwrap();
    let output = d.path().join("keep.wav");
    std::fs::write(&output, b"previous").unwrap();
    let error = render(
        &plan,
        &sources,
        &output,
        true,
        None,
        &Cancellation::default(),
    )
    .unwrap_err();
    assert_eq!(error.code, "TACTIC_STEM_CHANGED");
    assert_eq!(std::fs::read(output).unwrap(), b"previous");
}

#[test]
fn pcm24_preview_uses_the_same_dither_position_as_full_export() {
    let d = tempfile::tempdir().unwrap();
    let (mut plan, sources) = fixture(d.path(), |i| tone(100., i, 0.1), |_| 0.);
    write_tactics(&mut plan, vec![tactic(TacticId::S02)]).unwrap();
    plan.output.encoding = "pcm24".into();
    let full = render_to(&plan, &sources, &d.path().join("full24.wav"), None);
    let preview = render_to(
        &plan,
        &sources,
        &d.path().join("preview24.wav"),
        Some((START + 123, END + 345)),
    );
    let reference = &full[(START + 123) as usize..(END + 345) as usize];
    assert_eq!(preview.len(), reference.len());
    let mismatch = preview.iter().zip(reference).position(|(a, b)| a != b);
    assert!(
        mismatch.is_none(),
        "PCM24 preview differs at frame {mismatch:?}"
    );
}
