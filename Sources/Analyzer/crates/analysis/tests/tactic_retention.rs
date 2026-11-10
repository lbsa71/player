#[path = "../../../tests/support.rs"]
mod support;
use mixlib_analysis::analyze;
use mixlib_catalog::*;
use mixlib_types::*;
use std::collections::BTreeMap;

fn fixture() -> (tempfile::TempDir, Store, Track, AnalysisConfig) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init_legacy(&dir.path().join("library")).unwrap();
    let music = dir.path().join("music");
    std::fs::create_dir(&music).unwrap();
    support::tone(&music.join("a.wav"), 2., 220.);
    let mut config: AnalysisConfig = store.read("config/analysis.json").unwrap();
    config.settling_ms = 0;
    store.write("config/analysis.json", &config).unwrap();
    scan(&store, &[music], false, &Cancellation::default()).unwrap();
    analyze(&store, true, true, &Cancellation::default()).unwrap();
    let track = store.tracks().unwrap().remove(0);
    (dir, store, track, config)
}

fn attach(store: &Store, mut track: Track, wrong_beats: bool, wrong_override: bool) -> FileRef {
    let mut analysis = store.analysis(&track).unwrap();
    let reference = track.active_analysis.clone().unwrap();
    let mut beats = analysis.artifacts["beats"].clone();
    if wrong_beats {
        beats.hash = hash_bytes(b"different beat artifact");
    }
    let models = ["stem_htdemucs", "vocal_phrase_whisper"]
        .into_iter()
        .map(|name| {
            (
                name.into(),
                ModelAsset {
                    model_id: name.into(),
                    file: reference.clone(),
                    upstream_url: "fixture://local".into(),
                    revision: "fixture".into(),
                    code_license: "fixture".into(),
                    weight_license: "fixture".into(),
                    attribution: "fixture".into(),
                    redistribution: "fixture".into(),
                },
            )
        })
        .collect();
    let record = TacticTrackAnalysis {
        header: Header::default(),
        tactic_analysis_id: id(),
        track_id: track.track_id.clone(),
        audio_hash: track.audio_hash.clone(),
        audio_revision: track.audio_revision,
        source_sample_rate_hz: track.format.sample_rate_hz,
        source_frames: track.format.frames,
        base_analysis: reference.clone(),
        beat_artifact: beats,
        override_revision: track.override_revision + u64::from(wrong_override),
        method: TACTIC_ANALYSIS_METHOD.into(),
        implementation_hash: hash_bytes(b"fixture"),
        component_hashes: BTreeMap::new(),
        cache_key: hash_bytes(b"fixture"),
        models,
        stems: [
            StemLane::Drums,
            StemLane::Bass,
            StemLane::Other,
            StemLane::Vocals,
        ]
        .into_iter()
        .map(|lane| (lane, reference.clone()))
        .collect(),
        vocal_evidence: serde_json::json!({"method":"fixture/1","reference":"fixture:unknown","track":{"instrumental":false}}),
        signal_evidence: serde_json::json!({"method":"fixture/1","sample_rate_hz":track.format.sample_rate_hz,"source_start_frame":0,"source_end_frame":track.format.frames}),
        limitations: vec!["metadata retention fixture; no positive audio evidence".into()],
        created_at: now(),
    };
    let prefix = format!(
        "tracks/{}/{}/analysis/{}",
        shard(&track.track_id),
        track.track_id,
        id()
    );
    let supplemental = store
        .write(&format!("{prefix}/tactic-analysis.json"), &record)
        .unwrap();
    analysis
        .artifacts
        .insert("tactic_analysis".into(), supplemental.clone());
    analysis.stages.insert(
        "tactic_analysis".into(),
        Stage {
            implementation: TACTIC_ANALYSIS_METHOD.into(),
            input_hash: record.cache_key,
            artifacts: BTreeMap::from([("tactic_analysis".into(), supplemental.clone())]),
            dependencies: vec!["beats".into()],
        },
    );
    analysis.analysis_id = id();
    track.active_analysis = Some(
        store
            .write(&format!("{prefix}/manifest.json"), &analysis)
            .unwrap(),
    );
    store.save_track(&track).unwrap();
    supplemental
}

#[test]
fn matching_expensive_supplement_survives_feature_refresh_without_recursive_relinking() {
    let (_temp, store, track, mut config) = fixture();
    let original = track.active_analysis.clone().unwrap();
    let supplement = attach(&store, track.clone(), false, false);
    config.low_crossover_hz = 250.;
    store.write("config/analysis.json", &config).unwrap();
    let report = analyze(&store, true, true, &Cancellation::default()).unwrap();
    assert_eq!(report.counts.get("retained_tactic_analysis"), Some(&1));
    let current = store
        .analysis(&store.track(&track.track_id).unwrap())
        .unwrap();
    assert_eq!(current.artifacts["tactic_analysis"].hash, supplement.hash);
    assert_eq!(
        current.stages["tactic_analysis"].artifacts["tactic_analysis"].hash,
        supplement.hash
    );
    let unchanged: TacticTrackAnalysis = store.read_ref(&supplement).unwrap();
    assert_eq!(unchanged.base_analysis.hash, original.hash);
}

#[test]
fn changed_beat_or_override_generation_drops_supplement_from_new_manifest() {
    for (wrong_beats, wrong_override) in [(true, false), (false, true)] {
        let (_temp, store, track, mut config) = fixture();
        let supplement = attach(&store, track.clone(), wrong_beats, wrong_override);
        config.low_crossover_hz = 250.;
        store.write("config/analysis.json", &config).unwrap();
        let report = analyze(&store, true, true, &Cancellation::default()).unwrap();
        assert_eq!(report.counts.get("completed"), Some(&1));
        assert!(!report.counts.contains_key("retained_tactic_analysis"));
        let current = store
            .analysis(&store.track(&track.track_id).unwrap())
            .unwrap();
        assert!(!current.artifacts.contains_key("tactic_analysis"));
        assert!(!current.stages.contains_key("tactic_analysis"));
        assert!(
            store.read_ref::<TacticTrackAnalysis>(&supplement).is_ok(),
            "immutable old evidence remains readable"
        );
    }
}
