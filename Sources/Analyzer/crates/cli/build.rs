use sha2::{Digest, Sha256};
use std::{env, fs, path::Path, process::Command};

fn fingerprint(path: &Path, hash: &mut Sha256) {
    println!("cargo:rerun-if-changed={}", path.display());
    if path.is_dir() {
        let mut entries: Vec<_> = fs::read_dir(path)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for entry in entries {
            fingerprint(&entry, hash);
        }
    } else {
        hash.update(path.file_name().unwrap().as_encoded_bytes());
        hash.update(fs::read(path).unwrap());
    }
}
fn main() {
    let here = std::path::PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let mut hash = Sha256::new();
    let mut admission = Sha256::new();
    // Search ordering, logging and budgets are absent. Everything that can
    // authorize a cue/tactic, score a full set or alter its audio is included.
    for source in [
        "../planner/src",
        "../audio/src",
        "../analysis/src",
        "../types/src",
        "../catalog/src",
        "src/tactics.rs",
        "src/chain.rs",
        "src/composition.rs",
        "src/set_replays.rs",
        "src/required_pairs.rs",
        "../../Cargo.lock",
    ] {
        fingerprint(&here.join(source), &mut hash);
        if source != "src/composition.rs" {
            fingerprint(&here.join(source), &mut admission);
        }
    }
    let compiler = Command::new(env::var("RUSTC").unwrap())
        .arg("--version")
        .output()
        .unwrap()
        .stdout;
    hash.update(&compiler);
    admission.update(&compiler);
    let target = env::var("TARGET").unwrap();
    hash.update(&target);
    admission.update(&target);
    println!(
        "cargo:rustc-env=MIXLIB_MUSICAL_RULES_HASH={:x}",
        hash.finalize()
    );
    println!(
        "cargo:rustc-env=MIXLIB_ADMISSION_RULES_HASH={:x}",
        admission.finalize()
    );
}
