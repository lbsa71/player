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
