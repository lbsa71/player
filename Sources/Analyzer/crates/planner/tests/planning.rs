#![allow(clippy::field_reassign_with_default)]
#[path = "../../../tests/support.rs"]
mod support;
use mixlib_catalog::*;
use mixlib_planner::*;
use mixlib_types::*;
use std::{collections::BTreeMap, fs};
fn library(seconds: f64) -> (tempfile::TempDir, Store, Vec<Track>) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::init_legacy(&d.path().join("library")).unwrap();
    let music = d.path().join("music");
    fs::create_dir(&music).unwrap();
    for (name, freq) in [("a.wav", 440.), ("b.wav", 440.1)] {
        support::wav(
            &music.join(name),
            48000,
            2,
            32,
            true,
            false,
            (seconds * 48000.) as usize,
            b"CC0",
            |i, _| 0.15 * (std::f64::consts::TAU * freq * i as f64 / 48000.).sin(),
        );
    }
    let mut c: AnalysisConfig = s.read("config/analysis.json").unwrap();
    c.settling_ms = 0;
    s.write("config/analysis.json", &c).unwrap();
    scan(&s, &[music], false, &Cancellation::default()).unwrap();
    mixlib_analysis::analyze(&s, true, true, &Cancellation::default()).unwrap();
    let tracks = s.tracks().unwrap();
    (d, s, tracks)
}
#[test]
fn deterministic_coverage_duration_fixed_end_and_infeasibility() {
    let (_d, s, tracks) = library(2.);
    let mut c = MixingConfig::default();
    c.min_played_seconds = 0.25;
    c.overlap_seconds = 0.25;
    let req = Request {
        seed: 17,
        ..Request::default()
    };
    let p = plan(&s, &req, &c, &Cancellation::default()).unwrap();
    let again = plan(&s, &req, &c, &Cancellation::default()).unwrap();
    assert_eq!(
        serde_json::to_value(&p.placements).unwrap(),
        serde_json::to_value(&again.placements).unwrap()
    );
    assert_eq!(p.placements.len(), 2);
    assert!(p.transitions[0].fallback_reason.is_some());
    let req = Request {
        fixed_start: Some(tracks[0].track_id.clone()),
        fixed_end: Some(tracks[1].track_id.clone()),
        ..req
    };
    let p = plan(&s, &req, &c, &Cancellation::default()).unwrap();
    assert_eq!(p.placements[0].track_id, tracks[0].track_id);
    c.allow_fallback = false;
    assert_eq!(
        plan(&s, &req, &c, &Cancellation::default())
            .unwrap_err()
            .code,
        "PLAN_INFEASIBLE"
    );
    c.allow_fallback = true;
    let req = Request {
        target_seconds: Some(1.9),
        ..Request::default()
    };
    let p = plan(&s, &req, &c, &Cancellation::default()).unwrap();
    assert_eq!(p.placements.len(), 1);
    assert_eq!(p.total_output_frames, 96000);
}
#[test]
fn variable_grid_blends_pitch_preservation_and_preview_coherence() {
    let (d, s, mut tracks) = library(24.);
    for (idx, t) in tracks.iter_mut().enumerate() {
        let mut a = s.analysis(t).unwrap();
        let mut beats = vec![];
        let mut time = 0.25;
        for n in 0..47 {
            let frame = (time * 48000.) as u64;
            beats.push(Beat {
                event_id: format!("beat-{n}"),
                source_frame: frame,
                score: 1.,
                local_bpm: Some(120.),
            });
            time += if idx == 0 {
                0.5
            } else {
                0.505 + 0.001 * (n as f64 * 0.15).sin()
            };
        }
        let b = Beats {
            header: Header::default(),
            beats_id: id(),
            source_sample_rate_hz: 48000,
            source_frames: t.format.frames,
            downbeat_ids: beats
                .iter()
                .step_by(4)
                .map(|b| b.event_id.clone())
                .collect(),
            events: beats.clone(),
            meter_segments: vec![Meter {
                start_frame: 0,
                end_frame: t.format.frames,
                pulses_per_bar: 4,
                pulse_unit: "test_annotated_pulse".into(),
            }],
            unreliable_intervals: vec![],
            score_semantics: "synthetic ground truth for DSP tests, not model output".into(),
        };
        let analysis_id = id();
        let prefix = format!(
            "tracks/{}/{}/analysis/{analysis_id}",
            shard(&t.track_id),
            t.track_id
        );
        a.analysis_id = analysis_id;
        a.artifacts.insert(
            "beats".into(),
            s.write(&format!("{prefix}/beats.json"), &b).unwrap(),
        );
        let regions = Regions {
            header: Header::default(),
            regions_id: id(),
            source_frames: t.format.frames,
            regions: [RegionRole::Entry, RegionRole::Exit]
                .into_iter()
                .enumerate()
                .map(|(i, role)| Region {
                    region_id: format!("r{i}"),
                    role,
                    start_frame: 0,
                    end_frame: t.format.frames,
                    beat_ids: beats.iter().map(|b| b.event_id.clone()).collect(),
                    tempo_range_bpm: Some([118., 120.]),
                    chroma: [1. / 12.; 12],
                    energy: 0.1,
                    available_overlap_frames: t.format.frames,
                    reliable: true,
                    exclusion_reasons: vec![],
                })
                .collect(),
        };
        a.artifacts.insert(
            "regions".into(),
            s.write(&format!("{prefix}/regions.json"), &regions)
                .unwrap(),
        );
        a.stages = BTreeMap::new();
        t.active_analysis = Some(s.write(&format!("{prefix}/manifest.json"), &a).unwrap());
        s.save_track(t).unwrap();
    }
    let mut c = MixingConfig::default();
    c.overlap_seconds = 9.;
    c.min_played_seconds = 1.;
    c.allow_fallback = false;
    let req = Request {
        fixed_start: Some(tracks[0].track_id.clone()),
        ..Request::default()
    };
    let p = plan(&s, &req, &c, &Cancellation::default()).unwrap();
    assert_eq!(p.transitions[0].template, Template::BassHandoff);
    assert!(p.placements[1].time_map.anchors.len() > 10);
    assert!(p.placements[1]
        .time_map
        .slopes()
        .iter()
        .any(|x| (x - 1.).abs() > 0.005));
    let sources = verify_plan(&s, &p, &Cancellation::default()).unwrap();
    let start = p.transitions[0].end_output_frame + 4800;
    let end = start + 48000;
    let out = d.path().join("stretch.wav");
    mixlib_audio::render(
        &p,
        &sources,
        &out,
        false,
        Some((start, end)),
        &Cancellation::default(),
    )
    .unwrap();
    let mut reader = mixlib_audio::WavReader::open(&out).unwrap();
    let samples = reader.read_frames(48000).unwrap();
    let max_difference = samples
        .iter()
        .map(|s| (s[0] - s[1]).abs())
        .fold(0f32, f32::max); // Signalsmith links channel phases; Float32 FFT arithmetic permits a small residual.
    assert!(max_difference < 5e-5);
    let crossings = samples
        .windows(2)
        .filter(|s| s[0][0] < 0. && s[1][0] >= 0.)
        .count();
    assert!(
        (435..=445).contains(&crossings),
        "pitch changed: {crossings} Hz"
    );
    assert!(samples.iter().map(|s| s[0].abs()).fold(0f32, f32::max) > 0.05);
    c.template = Template::ThreeBandExchange;
    let p = plan(&s, &req, &c, &Cancellation::default()).unwrap();
    assert_eq!(p.transitions[0].template, Template::ThreeBandExchange);

    let request = StitchRequest {
        from: &tracks[0].track_id,
        to: &tracks[1].track_id,
        overlap_beats: 8,
        context_beats: 4,
    };
    let p = stitch(&s, &request, &c, &Cancellation::default()).unwrap();
    assert_eq!(p.objective, "beat_stitch");
    assert!(p.placements[0].time_map.slopes().iter().all(|s| *s < 1.));
    assert!(p.placements[1].time_map.slopes().iter().all(|s| *s > 1.));
    let transition = &p.transitions[0];
    assert_eq!(
        p.placements[0].time_map.anchors[1].output_frame,
        transition.start_output_frame
    );
    assert_eq!(
        p.placements[1].time_map.anchors[1].output_frame,
        transition.end_output_frame
    );
    assert!((7 * 48000..9 * 48000).contains(&p.total_output_frames));
    c.max_speed_deviation = 0.001;
    assert_eq!(
        stitch(&s, &request, &c, &Cancellation::default())
            .unwrap_err()
            .code,
        "STITCH_INFEASIBLE"
    );
}
