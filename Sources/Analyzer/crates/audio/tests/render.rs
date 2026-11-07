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
