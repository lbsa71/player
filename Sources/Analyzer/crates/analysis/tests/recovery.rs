#[path = "../../../tests/support.rs"]
mod support;
use mixlib_analysis::analyze;
use mixlib_catalog::*;
use mixlib_types::*;
#[test]
fn resumes_valid_stage_and_invalidates_only_dependencies() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::init_legacy(&dir.path().join("library")).unwrap();
    let music = dir.path().join("music");
    std::fs::create_dir(&music).unwrap();
    support::tone(&music.join("a.wav"), 2., 220.);
    let mut config: AnalysisConfig = s.read("config/analysis.json").unwrap();
    config.settling_ms = 0;
    s.write("config/analysis.json", &config).unwrap();
    scan(&s, &[music], false, &Cancellation::default()).unwrap();
    analyze(&s, true, true, &Cancellation::default()).unwrap();
    let t = s.tracks().unwrap().remove(0);
    let old = s.analysis(&t).unwrap();
    let old_ref = t.active_analysis.clone().unwrap();
    config.low_crossover_hz = 250.;
    s.write("config/analysis.json", &config).unwrap();
    let report = analyze(&s, true, true, &Cancellation::default()).unwrap();
    assert_eq!(report.counts["reused_rhythm_stages"], 1);
    let t = s.track(&t.track_id).unwrap();
    let current = s.analysis(&t).unwrap();
    assert_eq!(old.artifacts["beats"].hash, current.artifacts["beats"].hash);
    assert_ne!(
        old.artifacts["harmony"].hash,
        current.artifacts["harmony"].hash
    );
    assert!(s.read_ref::<AnalysisManifest>(&old_ref).is_ok());
    let harmony: Harmony = s.read_ref(&current.artifacts["harmony"]).unwrap();
    std::fs::write(s.path(&harmony.feature_chunks[0].path).unwrap(), b"corrupt").unwrap();
    let resumed = analyze(&s, true, true, &Cancellation::default()).unwrap();
    assert_eq!(resumed.counts["completed"], 1);
    let t = s.track(&t.track_id).unwrap();
    validate_analysis(&s, &s.analysis(&t).unwrap()).unwrap();
    // Simulate interruption after checkpoint publication, before committing track.json.
    let staged: AnalysisManifest = s
        .read(&format!("staging/{}/checkpoint.json", t.track_id))
        .unwrap();
    let mut reset = t.clone();
    reset.active_analysis = None;
    s.save_track(&reset).unwrap();
    let mut job: Job = s.read(&format!("jobs/{}.json", t.track_id)).unwrap();
    job.state = JobState::Running;
    s.save_job(&job).unwrap();
    let report = analyze(&s, true, true, &Cancellation::default()).unwrap();
    assert_eq!(report.counts["reused_feature_stages"], 1);
    assert_eq!(report.counts["reused_rhythm_stages"], 1);
    let final_analysis = s.analysis(&s.track(&t.track_id).unwrap()).unwrap();
    assert_eq!(
        staged.artifacts["beats"].hash,
        final_analysis.artifacts["beats"].hash
    );
}
#[test]
fn unavailable_models_are_explicit_and_do_not_publish_analysis() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::init_legacy(&dir.path().join("library")).unwrap();
    let music = dir.path().join("music");
    std::fs::create_dir(&music).unwrap();
    support::tone(&music.join("a.wav"), 0.2, 220.);
    scan(&s, &[music], false, &Cancellation::default()).unwrap();
    let report = analyze(&s, true, false, &Cancellation::default()).unwrap();
    assert_eq!(report.counts["failed"], 1);
    assert_eq!(report.diagnostics[0].code, "MODEL_MISSING");
    assert!(s.tracks().unwrap()[0].active_analysis.is_none());
}
