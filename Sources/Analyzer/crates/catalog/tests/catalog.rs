#[path = "../../../tests/support.rs"]
mod support;
use mixlib_catalog::*;
use mixlib_types::*;
use std::fs;
fn setup() -> (tempfile::TempDir, Store, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("music");
    fs::create_dir(&source).unwrap();
    let store = Store::init_legacy(&dir.path().join("library")).unwrap();
    let mut c: AnalysisConfig = store.read("config/analysis.json").unwrap();
    c.settling_ms = 0;
    store.write("config/analysis.json", &c).unwrap();
    (dir, store, source)
}
#[test]
fn identity_duplicates_moves_metadata_and_audio_changes() {
    let (_dir, s, music) = setup();
    let a = music.join("A.WAV");
    support::tone(&a, 1., 440.);
    scan(
        &s,
        std::slice::from_ref(&music),
        false,
        &Cancellation::default(),
    )
    .unwrap();
    let track = s.tracks().unwrap().remove(0);
    let id = track.track_id.clone();
    fs::rename(&a, music.join("moved.wav")).unwrap();
    let moved = scan(&s, &[], true, &Cancellation::default()).unwrap();
    assert_eq!(moved.counts["moved"], 1);
    assert_eq!(s.tracks().unwrap()[0].track_id, id);
    fs::copy(music.join("moved.wav"), music.join("duplicate.wav")).unwrap();
    scan(&s, &[], false, &Cancellation::default()).unwrap();
    assert_eq!(s.tracks().unwrap().len(), 1);
    assert_eq!(
        s.tracks().unwrap()[0]
            .source_locations
            .iter()
            .filter(|l| l.available)
            .count(),
        2
    );
    support::tone(&music.join("moved.wav"), 1., 880.);
    scan(&s, &[], true, &Cancellation::default()).unwrap();
    assert_eq!(s.tracks().unwrap().len(), 2);
    fs::remove_file(music.join("duplicate.wav")).unwrap();
    scan(&s, &[], false, &Cancellation::default()).unwrap();
    assert!(matches!(
        s.track(&id).unwrap().lifecycle,
        Lifecycle::Unavailable
    ));
}
#[test]
fn lock_inode_and_atomic_failure_preserve_records() {
    let (_dir, s, _) = setup();
    let original: AnalysisConfig = s.read("config/analysis.json").unwrap();
    let mut bad = original.clone();
    bad.low_crossover_hz = f64::NAN;
    assert!(s.write("config/analysis.json", &bad).is_err());
    assert_eq!(
        s.read::<AnalysisConfig>("config/analysis.json")
            .unwrap()
            .low_crossover_hz,
        original.low_crossover_hz
    );
    let guard = s.lock(false, &Cancellation::default()).unwrap();
    assert!(s.lock(false, &Cancellation::default()).is_err());
    assert!(s.read::<Library>("library.json").is_ok());
    drop(guard);
    assert!(s.lock(false, &Cancellation::default()).is_ok());
    assert!(s.path("../escape.json").is_err());
    assert!(s.path("/tmp/escape.json").is_err());
}
#[test]
fn corrupt_reference_is_detected_and_catalog_rebuilds() {
    let (_dir, s, music) = setup();
    support::tone(&music.join("a.wav"), 1., 440.);
    scan(&s, &[music], false, &Cancellation::default()).unwrap();
    let manifest = rebuild_catalog(&s).unwrap();
    let r = &manifest.shards[0];
    fs::write(s.path(&r.path).unwrap(), b"{}").unwrap();
    assert!(s.read_ref::<CatalogShard>(r).is_err());
    fs::remove_dir_all(s.path("catalog").unwrap()).unwrap();
    s.directory("catalog").unwrap();
    let m = rebuild_catalog(&s).unwrap();
    assert_eq!(
        s.read_ref::<CatalogShard>(&m.shards[0])
            .unwrap()
            .tracks
            .len(),
        1
    );
}
#[test]
fn malformed_wav_does_not_abort_scan_and_symlinks_are_skipped() {
    let (_dir, s, music) = setup();
    fs::write(music.join("bad.wav"), b"bad").unwrap();
    support::tone(&music.join("ok.wav"), 1., 440.);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&music, music.join("cycle")).unwrap();
    let report = scan(&s, &[music], false, &Cancellation::default()).unwrap();
    assert_eq!(report.counts["failed"], 1);
    assert_eq!(s.tracks().unwrap().len(), 1);
}

#[test]
fn verify_content_detects_replacement_with_preserved_size_and_timestamp() {
    let (_d, s, music) = setup();
    let p = music.join("a.wav");
    support::tone(&p, 1., 440.);
    scan(
        &s,
        std::slice::from_ref(&music),
        false,
        &Cancellation::default(),
    )
    .unwrap();
    let t = s.tracks().unwrap().remove(0);
    let m = fs::metadata(&p).unwrap();
    support::tone(&p, 1., 880.);
    fs::File::options()
        .write(true)
        .open(&p)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(m.modified().unwrap()))
        .unwrap();
    scan(&s, &[], false, &Cancellation::default()).unwrap();
    assert_eq!(s.track(&t.track_id).unwrap().audio_hash, t.audio_hash);
    scan(&s, &[], true, &Cancellation::default()).unwrap();
    let changed = s.track(&t.track_id).unwrap();
    assert_ne!(changed.audio_hash, t.audio_hash);
    assert_eq!(changed.audio_revision, t.audio_revision + 1);
}
#[test]
fn correction_publication_preserves_reader_snapshot_and_extensions() {
    let (_d, s, music) = setup();
    support::tone(&music.join("a.wav"), 1., 440.);
    scan(&s, &[music], false, &Cancellation::default()).unwrap();
    let old = s.tracks().unwrap().remove(0);
    let mut o = s.overrides(&old).unwrap();
    o.header
        .extensions
        .insert("future_field".into(), serde_json::json!({"preserve":true}));
    s.set_overrides(&old.track_id, o).unwrap();
    let first = s.track(&old.track_id).unwrap();
    let mut next = s.overrides(&first).unwrap();
    next.user_preferences
        .insert("energy".into(), serde_json::json!("low"));
    s.set_overrides(&first.track_id, next).unwrap();
    assert_eq!(s.overrides(&first).unwrap().revision, 1);
    let current = s.overrides(&s.track(&first.track_id).unwrap()).unwrap();
    assert_eq!(current.revision, 2);
    assert_eq!(current.header.extensions["future_field"]["preserve"], true);
}
