#[path = "../../../tests/support.rs"]
mod support;
use mixlib_catalog::*;
use mixlib_types::*;
use std::{fs, path::Path, process::Command};
fn cli(lib: &Path, args: &[&str]) -> serde_json::Value {
    let result = Command::new(env!("CARGO_BIN_EXE_mixlib"))
        .arg("--library")
        .arg(lib)
        .arg("--json")
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{:?}: {} {}",
        args,
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&result.stdout).unwrap()
}
#[test]
fn cli_scan_analyze_plan_preview_render_incremental_and_stale() {
    let dir = tempfile::tempdir().unwrap();
    let music = dir.path().join("music");
    fs::create_dir(&music).unwrap();
    let lib = dir.path().join("library");
    for (name, freq) in [("a.wav", 220.), ("b.wav", 330.)] {
        support::wav(
            &music.join(name),
            48000,
            2,
            32,
            true,
            false,
            96000,
            b"CC0",
            |i, _| 0.1 * (std::f64::consts::TAU * freq * i as f64 / 48000.).sin(),
        );
    }
    cli(&lib, &["init"]);
    let s = Store::open(&lib).unwrap();
    let mut a: AnalysisConfig = s.read("config/analysis.json").unwrap();
    a.settling_ms = 0;
    s.write("config/analysis.json", &a).unwrap();
    let mut m: MixingConfig = s.read("config/mixing.json").unwrap();
    m.min_played_seconds = 0.25;
    m.overlap_seconds = 0.25;
    s.write("config/mixing.json", &m).unwrap();
    let scan = cli(&lib, &["scan", "--source", music.to_str().unwrap()]);
    assert_eq!(scan["counts"]["new"], 2);
    let analysis = cli(&lib, &["analyze", "--resume", "--dsp-only"]);
    assert_eq!(analysis["counts"]["completed"], 2);
    let refs: Vec<_> = s
        .tracks()
        .unwrap()
        .iter()
        .map(|t| t.active_analysis.as_ref().unwrap().hash.clone())
        .collect();
    let again = cli(&lib, &["scan"]);
    assert_eq!(again["counts"]["unchanged"], 2);
    let analyze = cli(&lib, &["analyze", "--resume", "--dsp-only"]);
    assert_eq!(analyze["counts"]["unchanged"], 2);
    assert_eq!(
        refs,
        s.tracks()
            .unwrap()
            .iter()
            .map(|t| t.active_analysis.as_ref().unwrap().hash.clone())
            .collect::<Vec<_>>()
    );
    let p = cli(&lib, &["plan", "--all", "--seed", "42"]);
    let plan = p["plan_id"].as_str().unwrap();
    assert_eq!(p["placements"].as_array().unwrap().len(), 2);
    assert!(p["transitions"][0]["fallback_reason"].is_string());
    let output = dir.path().join("renders/mix.wav");
    let r = cli(
        &lib,
        &[
            "render",
            "--plan",
            plan,
            "--output",
            output.to_str().unwrap(),
        ],
    );
    assert_eq!(r["counts"]["output_frames"], p["total_output_frames"]);
    let forbidden = music.join("new-render-directory/mix.wav");
    let rejected = Command::new(env!("CARGO_BIN_EXE_mixlib"))
        .args([
            "--library",
            lib.to_str().unwrap(),
            "--json",
            "render",
            "--plan",
            plan,
            "--output",
            forbidden.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stdout).contains("OUTPUT_IN_SOURCE"));
    assert!(!music.join("new-render-directory").exists());
    let original = fs::read(&output).unwrap();
    let duplicate = Command::new(env!("CARGO_BIN_EXE_mixlib"))
        .args([
            "--library",
            lib.to_str().unwrap(),
            "--json",
            "render",
            "--plan",
            plan,
            "--output",
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!duplicate.status.success());
    assert_eq!(fs::read(&output).unwrap(), original);
    let previews = dir.path().join("previews");
    cli(
        &lib,
        &[
            "preview",
            "--plan",
            plan,
            "--output",
            previews.to_str().unwrap(),
            "--context-seconds",
            "0.1",
        ],
    );
    let planrecord: MixPlan = serde_json::from_value(p).unwrap();
    let mut preview = mixlib_audio::WavReader::open(&previews.join("transition-001.wav")).unwrap();
    let first = planrecord.transitions[0].start_output_frame - 4800;
    let mut full = mixlib_audio::WavReader::open(&output).unwrap();
    full.seek_frame(first).unwrap();
    assert_eq!(
        preview.read_frames(8192).unwrap(),
        full.read_frames(8192).unwrap()
    );
    cli(&lib, &["validate"]);
    // Same-size metadata rewrite preserves the analysis; verified audio replacement invalidates it.
    let apath = music.join("a.wav");
    let mut bytes = fs::read(&apath).unwrap();
    let pos = bytes.windows(3).position(|w| w == b"CC0").unwrap();
    bytes[pos..pos + 3].copy_from_slice(b"NEW");
    fs::write(&apath, &bytes).unwrap();
    cli(&lib, &["scan", "--verify-content"]);
    assert_eq!(
        refs,
        s.tracks()
            .unwrap()
            .iter()
            .map(|t| t.active_analysis.as_ref().unwrap().hash.clone())
            .collect::<Vec<_>>()
    );
    let last = bytes.len() - 4;
    bytes[last..].copy_from_slice(&0.4f32.to_le_bytes());
    fs::write(&apath, &bytes).unwrap();
    let stale = Command::new(env!("CARGO_BIN_EXE_mixlib"))
        .args([
            "--library",
            lib.to_str().unwrap(),
            "--json",
            "render",
            "--plan",
            planrecord.plan_id.as_str(),
            "--output",
            dir.path().join("stale.wav").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!stale.status.success());
    assert!(String::from_utf8_lossy(&stale.stdout).contains("SOURCE_CHANGED"));
    assert!(!dir.path().join("stale.wav").exists());
}

#[test]
fn cli_writer_conflict_and_parseable_error() {
    let d = tempfile::tempdir().unwrap();
    let s = Store::init(d.path()).unwrap();
    let _guard = s.lock(false, &Cancellation::default()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_mixlib"))
        .args([
            "--library",
            d.path().to_str().unwrap(),
            "--json",
            "rebuild-catalog",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["error"]["code"], "WRITER_BUSY");
    assert!(s.read::<Library>("library.json").is_ok());
}

#[test]
fn checked_in_schemas_match_contracts() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_mixlib"))
        .args(["schemas", "--output", dir.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
    let checked = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas");
    for entry in fs::read_dir(dir.path()).unwrap() {
        let entry = entry.unwrap();
        let generated: serde_json::Value =
            serde_json::from_slice(&fs::read(entry.path()).unwrap()).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(checked.join(entry.file_name())).unwrap()).unwrap();
        assert_eq!(
            generated,
            saved,
            "stale schema/example: {:?}",
            entry.file_name()
        );
    }
}
