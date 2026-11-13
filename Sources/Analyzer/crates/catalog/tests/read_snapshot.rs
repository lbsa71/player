#[path = "../../../tests/support.rs"]
mod support;
use mixlib_catalog::*;
use mixlib_types::*;

fn setup() -> (tempfile::TempDir, Store, Track) {
    let dir = tempfile::tempdir().unwrap();
    let music = dir.path().join("music");
    std::fs::create_dir(&music).unwrap();
    support::tone(&music.join("fixture.wav"), 0.1, 440.);
    let store = Store::init(&dir.path().join("library")).unwrap();
    let mut config: AnalysisConfig = store.read("config/analysis.json").unwrap();
    config.settling_ms = 0;
    store.write("config/analysis.json", &config).unwrap();
    scan(&store, &[music], false, &Cancellation::default()).unwrap();
    let track = store.tracks().unwrap().remove(0);
    (dir, store, track)
}

#[test]
fn one_sidecar_decode_serves_many_hashed_logical_records() {
    let (_dir, store, track) = setup();
    let refs: Vec<_> = (0..8)
        .map(|i| {
            store
                .write(
                    &format!("staging/{}/record-{i}.json", track.track_id),
                    &Report::new("fixture"),
                )
                .unwrap()
        })
        .collect();
    let snapshot = store.read_snapshot();
    assert!(snapshot.shares_read_snapshot(&snapshot.clone()));
    assert!(!snapshot.shares_read_snapshot(&store));
    assert!(!snapshot.shares_read_snapshot(&store.read_snapshot()));
    for _ in 0..3 {
        for reference in &refs {
            snapshot.read_ref::<Report>(reference).unwrap();
        }
    }
    let stats = snapshot.read_snapshot_stats().unwrap().unwrap();
    assert_eq!(stats.sidecar_bundles, 1);
    assert_eq!(stats.physical_files, 3); // library, sidecar index, one fused document
    assert_eq!(stats.logical_records, 10);
    snapshot
        .verify_read_snapshot(&Cancellation::default())
        .unwrap();
    let mut incorrect = refs[0].clone();
    incorrect.hash = hash_bytes(b"wrong");
    assert_eq!(
        snapshot.read_ref::<Report>(&incorrect).unwrap_err().code,
        "CHECKSUM"
    );
    assert_eq!(
        snapshot
            .write("reports/forbidden.json", &Report::new("fixture"))
            .unwrap_err()
            .code,
        "READ_SNAPSHOT_WRITE"
    );
    assert_eq!(
        snapshot.fuse_sidecars().unwrap_err().code,
        "READ_SNAPSHOT_WRITE"
    );
}

#[test]
fn song_identity_and_analysis_share_one_pinned_sidecar() {
    let (_dir, store, track) = setup();
    let path = store.sidecar_for_track(&track.track_id).unwrap();
    let mut document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    document["trackID"] = "original-song".into();
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();

    let snapshot = store.read_snapshot();
    assert_eq!(snapshot.tracks().unwrap()[0].track_id, track.track_id);
    let metadata = snapshot.recording_metadata(&track).unwrap();
    assert_eq!(metadata["trackID"], "original-song");
    assert!(metadata.get("analyzer").is_none());
    assert_eq!(
        snapshot
            .read_snapshot_stats()
            .unwrap()
            .unwrap()
            .sidecar_bundles,
        1
    );

    document["trackID"] = "changed-song".into();
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    assert_eq!(snapshot.recording_metadata(&track).unwrap(), metadata);
    assert_eq!(
        snapshot
            .verify_read_snapshot(&Cancellation::default())
            .unwrap_err()
            .code,
        "READ_SNAPSHOT_CHANGED"
    );
    assert_eq!(
        store.read_snapshot().recording_metadata(&track).unwrap()["trackID"],
        "changed-song"
    );
}

#[test]
fn corrections_changed_during_search_are_rejected_and_a_new_scope_is_fresh() {
    let (_dir, store, mut track) = setup();
    let snapshot = store.read_snapshot();
    assert_eq!(
        snapshot.track(&track.track_id).unwrap().override_revision,
        0
    );
    track.override_revision = 1;
    store.save_track(&track).unwrap();
    assert_eq!(
        snapshot.track(&track.track_id).unwrap().override_revision,
        0
    );
    assert_eq!(
        snapshot
            .verify_read_snapshot(&Cancellation::default())
            .unwrap_err()
            .code,
        "READ_SNAPSHOT_CHANGED"
    );
    drop(snapshot);
    assert_eq!(store.track(&track.track_id).unwrap().override_revision, 1);
    assert_eq!(
        store
            .read_snapshot()
            .track(&track.track_id)
            .unwrap()
            .override_revision,
        1
    );
}

#[test]
fn changed_or_corrupt_logical_records_never_escape_revalidation() {
    let (_dir, store, track) = setup();
    let key = format!("staging/{}/record.json", track.track_id);
    let reference = store.write(&key, &Report::new("original")).unwrap();
    let snapshot = store.read_snapshot();
    snapshot.read_ref::<Report>(&reference).unwrap();
    store.write(&key, &Report::new("changed")).unwrap();
    assert_eq!(
        snapshot
            .verify_read_snapshot(&Cancellation::default())
            .unwrap_err()
            .code,
        "READ_SNAPSHOT_CHANGED"
    );
    assert_eq!(
        store
            .read_snapshot()
            .read_ref::<Report>(&reference)
            .unwrap_err()
            .code,
        "CHECKSUM"
    );
    let fresh = store.read_snapshot();
    fresh.track(&track.track_id).unwrap();
    let path = store.sidecar_for_track(&track.track_id).unwrap();
    let mut corrupted = std::fs::read(&path).unwrap();
    corrupted[0] = b'!'; // Same length; byte verification must still reject it.
    std::fs::write(&path, corrupted).unwrap();
    assert_eq!(
        fresh
            .verify_read_snapshot(&Cancellation::default())
            .unwrap_err()
            .code,
        "READ_SNAPSHOT_CHANGED"
    );
    assert!(store.read_snapshot().track(&track.track_id).is_err());
}

#[test]
fn long_track_evidence_round_trips_without_relaxing_control_record_limits() {
    use std::collections::BTreeMap;
    let (_dir, store, track) = setup();
    let reference = FileRef {
        path: "models/fixture.bin".into(),
        hash: hash_bytes(b"fixture"),
    };
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
    let payload = "x".repeat(9 * 1024 * 1024);
    let mut record = TacticTrackAnalysis {
        header: Header::default(),
        tactic_analysis_id: id(),
        track_id: track.track_id.clone(),
        audio_hash: track.audio_hash.clone(),
        audio_revision: track.audio_revision,
        source_sample_rate_hz: track.format.sample_rate_hz,
        source_frames: track.format.frames,
        base_analysis: reference.clone(),
        beat_artifact: reference.clone(),
        override_revision: 0,
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
        vocal_evidence: serde_json::json!({"method":"fixture/1"}),
        signal_evidence: serde_json::json!({"method":"fixture/1","sample_rate_hz":track.format.sample_rate_hz,
            "source_start_frame":0,"source_end_frame":track.format.frames,"payload":payload}),
        limitations: vec![],
        created_at: now(),
    };
    let path = format!("staging/{}/tactic-analysis.json", track.track_id);
    let saved = store.write(&path, &record).unwrap();
    let snapshot = store.read_snapshot();
    assert_eq!(
        store
            .read_ref::<TacticTrackAnalysis>(&saved)
            .unwrap()
            .signal_evidence,
        record.signal_evidence
    );
    assert_eq!(
        snapshot
            .read_ref::<TacticTrackAnalysis>(&saved)
            .unwrap()
            .signal_evidence,
        record.signal_evidence
    );
    snapshot
        .verify_read_snapshot(&Cancellation::default())
        .unwrap();
    let mut ordinary = Report::new("fixture");
    ordinary
        .header
        .extensions
        .insert("payload".into(), record.signal_evidence["payload"].clone());
    assert_eq!(encode(&ordinary).unwrap_err().code, "RECORD_TOO_LARGE");
    record.signal_evidence["payload"] = serde_json::json!("x".repeat(33 * 1024 * 1024));
    assert_eq!(encode(&record).unwrap_err().code, "RECORD_TOO_LARGE");
}

#[test]
fn analysis_validation_cache_is_manifest_scoped_and_requires_final_byte_verification() {
    let (_dir, store, _track) = setup();
    let beats: Beats = serde_json::from_str(include_str!("../../../examples/beats.json")).unwrap();
    let harmony = Harmony {
        header: Header::default(),
        harmony_id: "fixture".into(),
        source_sample_rate_hz: 44100,
        tonic_encoding: "pitch_class".into(),
        mode_vocabulary: vec![],
        score_semantics: "fixture".into(),
        tuning_cents: None,
        tuning_reason: None,
        tuning_reference_hz: 440.,
        feature_chunks: vec![],
        windows: vec![],
    };
    let regions = Regions {
        header: Header::default(),
        regions_id: "fixture".into(),
        source_frames: 4410,
        regions: vec![],
    };
    let mut analysis: AnalysisManifest =
        serde_json::from_str(include_str!("../../../examples/analysis.json")).unwrap();
    analysis.stages.clear();
    analysis.artifacts = std::collections::BTreeMap::from([
        (
            "beats".into(),
            store.write("reports/beats.json", &beats).unwrap(),
        ),
        (
            "harmony".into(),
            store.write("reports/harmony.json", &harmony).unwrap(),
        ),
        (
            "regions".into(),
            store.write("reports/regions.json", &regions).unwrap(),
        ),
    ]);
    let snapshot = store.read_snapshot();
    for _ in 0..3 {
        validate_analysis(&snapshot, &analysis).unwrap();
    }
    assert_eq!(
        snapshot
            .read_snapshot_stats()
            .unwrap()
            .unwrap()
            .validated_analyses,
        1
    );
    let mut changed = analysis.clone();
    changed.artifacts.get_mut("beats").unwrap().hash = hash_bytes(b"incorrect");
    assert_eq!(
        validate_analysis(&snapshot, &changed).unwrap_err().code,
        "CHECKSUM"
    );
    assert_eq!(
        snapshot
            .read_snapshot_stats()
            .unwrap()
            .unwrap()
            .validated_analyses,
        1,
        "failures never enter the cache"
    );
    let mut changed_harmony = harmony;
    changed_harmony.tuning_reference_hz = 442.;
    store
        .write("reports/harmony.json", &changed_harmony)
        .unwrap();
    validate_analysis(&snapshot, &analysis).unwrap();
    assert_eq!(
        snapshot
            .verify_read_snapshot(&Cancellation::default())
            .unwrap_err()
            .code,
        "READ_SNAPSHOT_CHANGED"
    );
    assert_eq!(
        validate_analysis(&store.read_snapshot(), &analysis)
            .unwrap_err()
            .code,
        "CHECKSUM"
    );
    assert_eq!(
        validate_analysis(&store, &analysis).unwrap_err().code,
        "CHECKSUM"
    );
}
