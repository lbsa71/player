#[path = "../../../tests/support.rs"]
mod support;
use mixlib_catalog::*;
use mixlib_types::*;
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
fn recorder(path: &Path) -> Value {
    support::wav(path, 48000, 2, 32, true, false, 96000, b"CC0", |i, _| {
        0.1 * (std::f64::consts::TAU * 220. * i as f64 / 48000.).sin()
    });
    let data = json!({"trackID":"spotify-id", "album":"Album", "artist":"Artist", "track":"Title", "file":path.file_name().unwrap().to_str().unwrap(), "sampleRate":48000, "frames":96000, "channels":2, "encoding":"IEEE Float32", "durationSeconds":2., "segmentFinalized":true, "unknown_future_field":{"keep":[1,2,3]}, "observedStartPositionSeconds":0.12345678901234566});
    fs::write(
        sidecars::sidecar_path(path),
        serde_json::to_vec_pretty(&data).unwrap(),
    )
    .unwrap();
    data
}
fn setup(legacy: bool) -> (tempfile::TempDir, Store, PathBuf, PathBuf, Value) {
    let d = tempfile::tempdir().unwrap();
    let music = d.path().join("music");
    fs::create_dir(&music).unwrap();
    let wav = music.join("Album - Artist - Title.wav");
    let metadata = recorder(&wav);
    let s = if legacy {
        Store::init_legacy(&d.path().join("library"))
    } else {
        Store::init(&d.path().join("library"))
    }
    .unwrap();
    let mut c: AnalysisConfig = s.read("config/analysis.json").unwrap();
    c.settling_ms = 0;
    s.write("config/analysis.json", &c).unwrap();
    (d, s, music, wav, metadata)
}
fn assert_recording(path: &Path, original: &Value) {
    let mut data: Value =
        serde_json::from_slice(&fs::read(sidecars::sidecar_path(path)).unwrap()).unwrap();
    data.as_object_mut().unwrap().remove("analyzer");
    assert_eq!(&data, original);
}
#[test]
fn one_sidecar_preserves_recorder_fields_through_analysis_corrections_and_rebuild() {
    let (_d, s, music, wav, metadata) = setup(false);
    let audio = fs::read(&wav).unwrap();
    scan(
        &s,
        std::slice::from_ref(&music),
        false,
        &Cancellation::default(),
    )
    .unwrap();
    mixlib_analysis::analyze(&s, true, true, &Cancellation::default()).unwrap();
    let t = s.tracks().unwrap().remove(0);
    let a = s.analysis(&t).unwrap();
    validate_analysis(&s, &a).unwrap();
    let mut corrections = s.overrides(&t).unwrap();
    corrections.operations.push(Correction::Unsuitable {
        start_frame: 0,
        end_frame: 48000,
    });
    s.set_overrides(&t.track_id, corrections).unwrap();
    assert_recording(&wav, &metadata);
    assert_eq!(fs::read(&wav).unwrap(), audio);
    let doc: Value =
        serde_json::from_slice(&fs::read(sidecars::sidecar_path(&wav)).unwrap()).unwrap();
    let records = doc["analyzer"]["records"].as_object().unwrap();
    assert!(records.keys().any(|k| k.ends_with("beats.json")));
    assert!(records.keys().any(|k| k.contains("/features/")));
    assert!(records.keys().any(|k| k.starts_with("jobs/")));
    assert!(records.keys().any(|k| k.starts_with("staging/")));
    assert!(!s
        .root
        .join(Store::track_path(&t.track_id).unwrap())
        .exists());
    assert!(!s.root.join(format!("jobs/{}.json", t.track_id)).exists());
    assert!(!s.root.join(format!("staging/{}", t.track_id)).exists());
    let modified = fs::metadata(sidecars::sidecar_path(&wav))
        .unwrap()
        .modified()
        .unwrap();
    let before = fs::read(sidecars::sidecar_path(&wav)).unwrap();
    scan(&s, &[], false, &Cancellation::default()).unwrap();
    assert_eq!(fs::read(sidecars::sidecar_path(&wav)).unwrap(), before);
    assert_eq!(
        fs::metadata(sidecars::sidecar_path(&wav))
            .unwrap()
            .modified()
            .unwrap(),
        modified
    );
    fs::remove_dir_all(s.root.join("catalog")).unwrap();
    s.directory("catalog").unwrap();
    rebuild_catalog(&s).unwrap();
    assert_eq!(s.track(&t.track_id).unwrap().override_revision, 1);
    validate_analysis(&s, &s.analysis(&s.track(&t.track_id).unwrap()).unwrap()).unwrap();
    assert_eq!(fs::read_dir(music).unwrap().count(), 2);
}
#[test]
fn migration_keeps_saved_plan_hashes_and_removes_separate_per_track_json() {
    let (d, s, music, wav, metadata) = setup(true);
    scan(&s, &[music], false, &Cancellation::default()).unwrap();
    mixlib_analysis::analyze(&s, true, true, &Cancellation::default()).unwrap();
    let t = s.tracks().unwrap().remove(0);
    let old = t.active_analysis.clone().unwrap();
    let p = mixlib_planner::plan(
        &s,
        &mixlib_planner::Request::default(),
        &MixingConfig::default(),
        &Cancellation::default(),
    )
    .unwrap();
    s.write(&format!("plans/{}.json", p.plan_id), &p).unwrap();
    assert_eq!(s.fuse_sidecars().unwrap().counts["fused"], 1);
    assert!(s.uses_sidecars().unwrap());
    assert_recording(&wav, &metadata);
    assert_eq!(
        s.track(&t.track_id).unwrap().active_analysis.unwrap().hash,
        old.hash
    );
    assert!(s.read_ref::<AnalysisManifest>(&old).is_ok());
    assert!(!s
        .root
        .join(Store::track_path(&t.track_id).unwrap())
        .exists());
    // Model a crash after switching layouts but before deleting a copied file.
    let key = Store::track_path(&t.track_id).unwrap();
    let leftover = s.path(&key).unwrap();
    fs::create_dir_all(leftover.parent().unwrap()).unwrap();
    fs::write(&leftover, s.record_bytes(&key).unwrap()).unwrap();
    assert_eq!(
        s.fuse_sidecars().unwrap().counts["removed_legacy_records"],
        1
    );
    assert!(!leftover.exists());
    let sources = verify_plan(&s, &p, &Cancellation::default()).unwrap();
    mixlib_audio::render(
        &p,
        &sources,
        &d.path().join("mix.wav"),
        false,
        None,
        &Cancellation::default(),
    )
    .unwrap();
}
#[test]
fn partial_and_unfinalized_recordings_and_invalid_sidecars_are_not_modified() {
    let (_d, s, music, _wav, _) = setup(false);
    let partial = music.join("copy.partial.WAV");
    recorder(&partial);
    let pending = music.join("unfinished.wav");
    let mut m = recorder(&pending);
    m["segmentFinalized"] = false.into();
    fs::write(
        sidecars::sidecar_path(&pending),
        serde_json::to_vec(&m).unwrap(),
    )
    .unwrap();
    let bad = music.join("bad.wav");
    recorder(&bad);
    fs::write(sidecars::sidecar_path(&bad), b"{unfinished").unwrap();
    let before: Vec<_> = [&partial, &pending, &bad]
        .iter()
        .map(|p| fs::read(sidecars::sidecar_path(p)).unwrap())
        .collect();
    let report = scan(&s, &[music], false, &Cancellation::default()).unwrap();
    assert_eq!(report.counts["deferred"], 2);
    assert_eq!(report.counts["failed"], 1);
    assert_eq!(s.tracks().unwrap().len(), 1);
    for (p, bytes) in [&partial, &pending, &bad].iter().zip(before) {
        assert_eq!(fs::read(sidecars::sidecar_path(p)).unwrap(), bytes);
    }
}
#[test]
fn default_command_uses_documents_audio_and_waits_for_recorder_publication() {
    let d = tempfile::tempdir().unwrap();
    let music = d.path().join("Documents/Audio");
    fs::create_dir_all(&music).unwrap();
    let wav = music.join("recording.wav");
    recorder(&wav);
    fs::remove_file(sidecars::sidecar_path(&wav)).unwrap();
    let run = || {
        let o = Command::new(env!("CARGO_BIN_EXE_mixlib"))
            .env("HOME", d.path())
            .env_remove("MIXLIB_LIBRARY")
            .args(["scan", "--json"])
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        serde_json::from_slice::<Value>(&o.stdout).unwrap()
    };
    assert_eq!(run()["counts"]["deferred"], 1);
    assert!(!sidecars::sidecar_path(&wav).exists());
    recorder(&wav);
    assert_eq!(run()["counts"]["new"], 1);
    let s = Store::open(&d.path().join("Library/Application Support/Player/Analyzer")).unwrap();
    assert!(s.uses_sidecars().unwrap());
    assert_eq!(
        s.read::<Library>("library.json").unwrap().source_roots[0].path,
        fs::canonicalize(&music).unwrap().to_str().unwrap()
    );
    assert_eq!(s.tracks().unwrap().len(), 1);
}
#[test]
fn renamed_wav_and_sidecar_reconnect_identity() {
    let (_d, s, music, wav, _) = setup(false);
    scan(
        &s,
        std::slice::from_ref(&music),
        false,
        &Cancellation::default(),
    )
    .unwrap();
    let id = s.tracks().unwrap()[0].track_id.clone();
    let moved = music.join("renamed.wav");
    fs::rename(&wav, &moved).unwrap();
    fs::rename(sidecars::sidecar_path(&wav), sidecars::sidecar_path(&moved)).unwrap();
    scan(&s, &[], true, &Cancellation::default()).unwrap();
    assert_eq!(s.tracks().unwrap()[0].track_id, id);
    assert_eq!(
        s.source(&s.track(&id).unwrap()).unwrap(),
        fs::canonicalize(moved).unwrap()
    );
}

#[test]
fn duplicate_sidecar_shares_analysis_and_can_change_audio_without_losing_history() {
    let (_d, s, music, wav, metadata) = setup(false);
    scan(
        &s,
        std::slice::from_ref(&music),
        false,
        &Cancellation::default(),
    )
    .unwrap();
    let original = s.tracks().unwrap().remove(0);
    let duplicate = music.join("duplicate.wav");
    recorder(&duplicate);
    let r = scan(&s, &[], false, &Cancellation::default()).unwrap();
    assert_eq!(r.counts["duplicate"], 1);
    let doc: Value =
        serde_json::from_slice(&fs::read(sidecars::sidecar_path(&duplicate)).unwrap()).unwrap();
    assert!(doc["analyzer"]["shared_from"].is_object());
    assert!(doc["analyzer"]["records"].as_object().unwrap().is_empty());
    support::wav(
        &duplicate,
        48000,
        2,
        32,
        true,
        false,
        96000,
        b"CC0",
        |i, _| 0.1 * (std::f64::consts::TAU * 440. * i as f64 / 48000.).sin(),
    );
    scan(&s, &[], true, &Cancellation::default()).unwrap();
    assert_eq!(s.tracks().unwrap().len(), 2);
    assert_eq!(
        s.track(&original.track_id).unwrap().audio_hash,
        original.audio_hash
    );
    assert_recording(&wav, &metadata);
    assert_eq!(fs::read_dir(music).unwrap().count(), 4);
}

#[test]
fn foreign_library_cannot_replace_owned_sidecar() {
    let (d, s, music, wav, _) = setup(false);
    scan(
        &s,
        std::slice::from_ref(&music),
        false,
        &Cancellation::default(),
    )
    .unwrap();
    let before = fs::read(sidecars::sidecar_path(&wav)).unwrap();
    let other = Store::init(&d.path().join("other")).unwrap();
    assert_eq!(
        scan(&other, &[music], false, &Cancellation::default())
            .unwrap_err()
            .code,
        "SIDECAR_OWNER"
    );
    assert_eq!(fs::read(sidecars::sidecar_path(&wav)).unwrap(), before);
}

#[test]
fn cli_analyzes_only_selected_tracks_and_rejects_unknown_ids_before_writing() {
    let (_d, s, music, wav, _) = setup(false);
    let other = music.join("other.wav");
    support::wav(&other, 48000, 2, 32, true, false, 96000, b"CC0", |i, _| {
        0.1 * (std::f64::consts::TAU * 440. * i as f64 / 48000.).sin()
    });
    scan(&s, &[music], false, &Cancellation::default()).unwrap();
    let track = s
        .tracks()
        .unwrap()
        .into_iter()
        .find(|t| s.source(t).unwrap() == fs::canonicalize(&wav).unwrap())
        .unwrap();
    let untouched = fs::read(sidecars::sidecar_path(&other)).unwrap();
    let run = |ids: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mixlib"));
        command
            .arg("--library")
            .arg(&s.root)
            .args(["--json", "analyze", "--resume", "--dsp-only"]);
        for id in ids {
            command.args(["--track", id]);
        }
        command.output().unwrap()
    };
    let rejected = run(&[&track.track_id, "unknown-id"]);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stdout).contains("TRACK_NOT_FOUND"));
    assert!(s
        .tracks()
        .unwrap()
        .iter()
        .all(|t| t.active_analysis.is_none()));
    let result = run(&[&track.track_id, &track.track_id]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap()["counts"]["completed"],
        1
    );
    assert!(s.track(&track.track_id).unwrap().active_analysis.is_some());
    assert_eq!(fs::read(sidecars::sidecar_path(&other)).unwrap(), untouched);
}
