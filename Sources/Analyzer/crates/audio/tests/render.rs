#[path = "../../../tests/support.rs"]
mod support;
use mixlib_audio::*;
use mixlib_types::*;
use std::collections::BTreeMap;
fn plan(path: &std::path::Path, speed: f64, gain: f64) -> MixPlan {
    let mut wav = WavReader::open(path).unwrap();
    let audio = wav.hashes(&Cancellation::default()).unwrap().1;
    let frames = wav.format.frames;
    let out = (frames as f64 / speed).round() as u64;
    let policy = MixingConfig::default();
    MixPlan {
        header: Header::default(),
        plan_id: "test-plan".into(),
        planner_version: "test".into(),
        renderer_version: RENDERER.into(),
        policy_hash: canonical_hash(&policy).unwrap(),
        policy,
        seed: 42,
        objective: "order_all".into(),
        requested_duration_seconds: None,
        complete: true,
        output: OutputFormat {
            sample_rate_hz: 48000,
            channels: 2,
            encoding: "float32".into(),
            dither: "seeded_tpdf_1_lsb".into(),
        },
        drum_intro: None,
        prepared_patches: vec![],
        placements: vec![Placement {
            track_id: "track".into(),
            audio_hash: audio,
            audio_revision: 1,
            analysis: FileRef {
                path: "unused.json".into(),
                hash: hash_bytes(b""),
            },
            override_revision: 0,
            source_start_frame: 0,
            source_end_frame: frames,
            time_map: TimeMap {
                header: Header::default(),
                map_id: "map".into(),
                source_sample_rate_hz: 48000,
                output_sample_rate_hz: 48000,
                interpolation: "monotone_cubic".into(),
                anchors: vec![
                    Anchor {
                        source_frame: 0,
                        output_frame: 0,
                    },
                    Anchor {
                        source_frame: frames,
                        output_frame: out,
                    },
                ],
                speed_ratio_limits: [0.94, 1.06],
                max_speed_change_per_second: 0.02,
                preserve_pitch: true,
            },
            automation: vec![
                AutomationPoint {
                    output_frame: 0,
                    gain_linear: gain,
                    bands_linear: [1.; 3],
                    mute: false,
                },
                AutomationPoint {
                    output_frame: out,
                    gain_linear: gain,
                    bands_linear: [1.; 3],
                    mute: false,
                },
            ],
        }],
        transitions: vec![],
        total_output_frames: out,
        crossover_hz: [200., 2500.],
        crossover_design: "complementary_one_pole_v1".into(),
        automation_interpolation: "smoothstep_linear_amplitude".into(),
        diagnostics: vec![],
    }
}
#[test]
fn latency_compensation_places_transients_within_ten_ms() {
    let d = tempfile::tempdir().unwrap();
    let source = d.path().join("source.wav");
    support::wav(&source, 48000, 2, 32, true, false, 96000, b"CC0", |i, _| {
        let t = i as f64 / 48000.;
        [0.5, 1., 1.5]
            .iter()
            .map(|p| {
                0.3 * (-((t - p) / 0.002).powi(2)).exp() * (std::f64::consts::TAU * 1500. * t).cos()
            })
            .sum()
    });
    let p = plan(&source, 1.03, 1.);
    let out = d.path().join("mix.wav");
    render(
        &p,
        &BTreeMap::from([("track".into(), source)]),
        &out,
        false,
        None,
        &Cancellation::default(),
    )
    .unwrap();
    let mut wav = WavReader::open(&out).unwrap();
    let mut samples = vec![];
    loop {
        let block = wav.read_frames(65536).unwrap();
        if block.is_empty() {
            break;
        }
        samples.extend(block);
    }
    for t in [0.5, 1., 1.5] {
        let expected = (t / 1.03 * 48000.) as usize;
        let low = expected - 1500;
        let high = expected + 1500;
        let actual = low
            + samples[low..high]
                .iter()
                .enumerate()
                .max_by(|a, b| a.1[0].abs().total_cmp(&b.1[0].abs()))
                .unwrap()
                .0;
        assert!(
            actual.abs_diff(expected) <= 480,
            "transient scheduled at {actual}, expected {expected}"
        );
    }
}
#[test]
fn failed_or_cancelled_export_keeps_previous_output_and_pcm24_is_reproducible() {
    let d = tempfile::tempdir().unwrap();
    let source = d.path().join("source.wav");
    support::wav(&source, 48000, 2, 32, true, false, 24000, b"CC0", |i, _| {
        0.99 * (std::f64::consts::TAU * 440. * i as f64 / 48000.).sin()
    });
    let mut p = plan(&source, 1., 1.);
    let sources = BTreeMap::from([("track".into(), source)]);
    let out = d.path().join("mix.wav");
    std::fs::write(&out, b"previous committed output").unwrap();
    assert_eq!(
        render(&p, &sources, &out, true, None, &Cancellation::default())
            .unwrap_err()
            .code,
        "TRUE_PEAK"
    );
    assert_eq!(std::fs::read(&out).unwrap(), b"previous committed output");
    let cancel = Cancellation::default();
    cancel.cancel();
    assert_eq!(
        render(&p, &sources, &out, true, None, &cancel)
            .unwrap_err()
            .code,
        "CANCELLED"
    );
    assert_eq!(std::fs::read(&out).unwrap(), b"previous committed output");
    for x in &mut p.placements[0].automation {
        x.gain_linear = 0.2;
    }
    p.output.encoding = "pcm24".into();
    render(&p, &sources, &out, true, None, &Cancellation::default()).unwrap();
    let out2 = d.path().join("mix2.wav");
    render(&p, &sources, &out2, false, None, &Cancellation::default()).unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), std::fs::read(&out2).unwrap());
    assert_eq!(WavReader::open(&out).unwrap().format.container_bits, 24);
}

#[test]
fn set_replay_uses_processed_capture_resumes_exactly_and_previews_match() {
    let d = tempfile::tempdir().unwrap();
    let source = d.path().join("phrase.wav");
    support::wav(
        &source,
        48000,
        2,
        32,
        true,
        false,
        20 * 48000,
        b"CC0",
        |i, _| {
            let t = i as f64 / 48000.;
            0.15 * (std::f64::consts::TAU * (180. * t + 2. * t * t)).sin()
        },
    );
    let sources = BTreeMap::from([("track".into(), source.clone())]);
    let cancel = Cancellation::default();
    for speed in [1., 1.02] {
        let mut p = plan(&source, speed, 0.7);
        p.output.encoding = "pcm24".into();
        let base = d.path().join(format!("base-{speed}.wav"));
        render(&p, &sources, &base, false, None, &cancel).unwrap();
        let r = PassageReplay {
            placement: 0,
            capture_start: 2 * 48000,
            capture_end: 3 * 48000,
            insert_before: 8 * 48000,
            repetitions: 2,
            bars: 4,
            declick_frames: 240,
            evidence: serde_json::json!({"fixture":true}),
        };
        let a = SetArrangement {
            version: 1,
            base_output_frames: p.total_output_frames,
            replays: vec![r.clone()],
        };
        p.total_output_frames += a.added_frames();
        p.renderer_version = ARRANGEMENT_RENDERER.into();
        p.header.extensions.insert(
            ARRANGEMENT_EXTENSION.into(),
            serde_json::to_value(&a).unwrap(),
        );
        let out = d.path().join(format!("replay-{speed}.wav"));
        render(&p, &sources, &out, false, None, &cancel).unwrap();
        let mut original = WavReader::open(&base).unwrap();
        let mut expanded = WavReader::open(&out).unwrap();
        assert_eq!(expanded.format.frames, p.total_output_frames);
        for repetition in 0..2 {
            original.seek_frame(r.capture_start + 12000).unwrap();
            expanded
                .seek_frame(r.insert_before + repetition * 48000 + 12000)
                .unwrap();
            for (x, y) in original
                .read_frames(2000)
                .unwrap()
                .iter()
                .zip(expanded.read_frames(2000).unwrap())
            {
                assert!(
                    (x[0] - y[0]).abs() < 4. / 8388608.,
                    "repeat must preserve the stretched capture"
                );
            }
        }
        original.seek_frame(r.insert_before + 100).unwrap();
        expanded
            .seek_frame(r.insert_before + r.added_frames() + 100)
            .unwrap();
        for (x, y) in original
            .read_frames(2000)
            .unwrap()
            .iter()
            .zip(expanded.read_frames(2000).unwrap())
        {
            assert!(
                (x[0] - y[0]).abs() < 4. / 8388608.,
                "original timeline must resume without skipping source"
            );
        }
        let range = (
            r.insert_before + 72000,
            r.insert_before + r.added_frames() + 4567,
        );
        let preview = d.path().join(format!("preview-{speed}.wav"));
        render(&p, &sources, &preview, false, Some(range), &cancel).unwrap();
        let mut slice = WavReader::open(&preview).unwrap();
        expanded.seek_frame(range.0).unwrap();
        assert_eq!(
            slice.read_frames((range.1 - range.0) as usize).unwrap(),
            expanded.read_frames((range.1 - range.0) as usize).unwrap()
        );
        let mut bad = p.clone();
        let mut invalid = a.clone();
        invalid.replays[0].insert_before = 2 * 48000;
        bad.header.extensions.insert(
            ARRANGEMENT_EXTENSION.into(),
            serde_json::to_value(invalid).unwrap(),
        );
        assert!(
            bad.validate().is_err(),
            "a capture cannot be replayed before it exists"
        );
        bad = p.clone();
        bad.placements[0].automation[1].gain_linear = 0.5;
        assert!(
            bad.validate().is_err(),
            "changing gain invalidates a measured splice"
        );
    }
}
