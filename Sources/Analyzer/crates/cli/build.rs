use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, env, fs, path::Path, process::Command};
use toml::Value;

// These files generate requests or assemble whole sets. Pair measurement
// receives complete pinned plans and owns its own continuation generation.
const NON_ACOUSTIC_PLANNER: &[&str] = &[
    "baseline_cues.rs",
    "closing_bookends.rs",
    "cue_domain.rs",
    "event_cues.rs",
    "set_replays.rs",
    "song_pool.rs",
];
const ACOUSTIC_CRATES: &[&str] = &["planner", "audio", "analysis", "types", "catalog"];

fn add(hash: &mut Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value);
}

fn fingerprint(path: &Path, hash: &mut Sha256, excluded: &[&str]) {
    let name = path.file_name().unwrap().to_str().unwrap();
    if excluded.contains(&name) {
        return;
    }
    println!("cargo:rerun-if-changed={}", path.display());
    add(hash, name.as_bytes());
    if path.is_dir() {
        let mut entries: Vec<_> = fs::read_dir(path)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for entry in entries {
            fingerprint(&entry, hash, excluded);
        }
    } else {
        add(hash, &fs::read(path).unwrap());
    }
}

#[derive(Clone, Copy)]
struct Locked<'a> {
    name: &'a str,
    version: &'a str,
    source: &'a str,
    checksum: &'a str,
    dependencies: &'a [Value],
}

impl<'a> Locked<'a> {
    fn parse(package: &'a Value) -> Option<Self> {
        Some(Self {
            name: package.get("name")?.as_str()?,
            version: package.get("version")?.as_str()?,
            source: package.get("source").and_then(Value::as_str).unwrap_or(""),
            checksum: package
                .get("checksum")
                .and_then(Value::as_str)
                .unwrap_or(""),
            dependencies: match package.get("dependencies") {
                Some(v) => v.as_array()?.as_slice(),
                None => &[],
            },
        })
    }

    fn identity(self) -> (&'a str, &'a str, &'a str) {
        (self.name, self.version, self.source)
    }
}

fn resolve(packages: &[Locked<'_>], dependency: &str) -> Option<usize> {
    let mut fields = dependency.splitn(3, ' ');
    let name = fields.next()?;
    let version = fields.next();
    let source = match fields.next() {
        Some(s) => Some(s.strip_prefix('(')?.strip_suffix(')')?),
        None => None,
    };
    let mut candidates = packages.iter().enumerate().filter(|(_, p)| {
        p.name == name
            && version.is_none_or(|v| v == p.version)
            && source.is_none_or(|s| s == p.source)
    });
    let (index, _) = candidates.next()?;
    candidates.next().is_none().then_some(index)
}

fn locked_acoustic_graph(lock: &str, workspace: &str) -> Option<Sha256> {
    let lock: Value = lock.parse().ok()?;
    let workspace: Value = workspace.parse().ok()?;
    let packages = lock
        .get("package")?
        .as_array()?
        .iter()
        .map(Locked::parse)
        .collect::<Option<Vec<_>>>()?;
    let mut todo = ACOUSTIC_CRATES
        .iter()
        .map(|name| resolve(&packages, &format!("mixlib-{name}")))
        .collect::<Option<Vec<_>>>()?;
    let mut reachable = BTreeSet::new();
    while let Some(i) = todo.pop() {
        if reachable.insert(i) {
            for dependency in packages[i].dependencies {
                todo.push(resolve(&packages, dependency.as_str()?)?);
            }
        }
    }

    let mut graph: Vec<_> = reachable.into_iter().map(|i| packages[i]).collect();
    graph.sort_by_key(|p| p.identity());
    let mut hash = Sha256::new();
    let workspace_deps = workspace
        .get("workspace")?
        .get("dependencies")?
        .as_table()?;
    for package in graph {
        for value in [
            package.name,
            package.version,
            package.source,
            package.checksum,
        ] {
            add(&mut hash, value.as_bytes());
        }
        let mut dependencies = package
            .dependencies
            .iter()
            .map(|dep| Some(packages[resolve(&packages, dep.as_str()?)?].identity()))
            .collect::<Option<Vec<_>>>()?;
        dependencies.sort_unstable();
        add(&mut hash, &(dependencies.len() as u64).to_le_bytes());
        for (name, version, source) in dependencies {
            for value in [name, version, source] {
                add(&mut hash, value.as_bytes());
            }
        }
        if let Some(settings) = workspace_deps.get(package.name) {
            add(&mut hash, settings.to_string().as_bytes());
        }
    }
    if let Some(profile) = workspace.get("profile") {
        add(&mut hash, profile.to_string().as_bytes());
    }
    Some(hash)
}

fn audited_baseline_toolchain() -> bool {
    for key in ["CC", "CXX", "SDKROOT", "LIBCLANG_PATH", "DEVELOPER_DIR"] {
        println!("cargo:rerun-if-env-changed={key}");
        if env::var_os(key).is_some() {
            return false;
        }
    }
    let output = |program: &str, args: &[&str]| {
        let result = Command::new(program).args(args).output().ok()?;
        result
            .status
            .success()
            .then_some(String::from_utf8(result.stdout).ok()?)
    };
    output("clang", &["--version"])
        .as_deref()
        .and_then(|s| s.lines().next())
        == Some("Apple clang version 21.0.0 (clang-2100.3.34.2)")
        && output("xcrun", &["--sdk", "macosx", "--show-sdk-version"])
            .as_deref()
            .map(str::trim)
            == Some("27.0")
}

fn main() {
    let here = std::path::PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let lock = here.join("../../Cargo.lock");
    let workspace = here.join("../../Cargo.toml");
    let mut admission = Sha256::new();
    let mut proposals = Sha256::new();
    let mut state = Sha256::new();
    add(&mut admission, b"pair-admission/2");

    for name in ACOUSTIC_CRATES {
        let crate_path = here.join("..").join(name);
        let src = crate_path.join("src");
        fingerprint(
            &src,
            &mut admission,
            if *name == "planner" {
                NON_ACOUSTIC_PLANNER
            } else {
                &[]
            },
        );
        fingerprint(&crate_path.join("Cargo.toml"), &mut admission, &[]);
        let build = crate_path.join("build.rs");
        if build.exists() {
            fingerprint(&build, &mut admission, &[]);
        }
    }
    for name in ["src/tactics.rs", "src/duration_measurement.rs"] {
        fingerprint(&here.join(name), &mut admission, &[]);
    }
    for path in [&lock, &workspace] {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    if let Some(graph) = locked_acoustic_graph(
        &fs::read_to_string(&lock).unwrap(),
        &fs::read_to_string(&workspace).unwrap(),
    ) {
        add(&mut admission, &graph.finalize());
    } else {
        // New lock formats or ambiguous identities fall back to full invalidation.
        fingerprint(&lock, &mut admission, &[]);
        fingerprint(&workspace, &mut admission, &[]);
    }
    let compiler = Command::new(env::var("RUSTC").unwrap())
        .arg("--version")
        .output()
        .unwrap()
        .stdout;
    for input in [&compiler[..], env::var("TARGET").unwrap().as_bytes()] {
        add(&mut admission, input);
    }
    for key in ["CARGO_ENCODED_RUSTFLAGS", "CFLAGS", "CXXFLAGS"] {
        println!("cargo:rerun-if-env-changed={key}");
        add(&mut admission, env::var(key).unwrap_or_default().as_bytes());
    }
    let admission = format!("{:x}", admission.finalize());
    println!("cargo:rustc-env=MIXLIB_ADMISSION_RULES_HASH={admission}");
    // The 73-track benchmark used 180c80d with Rust 1.89 on arm64 macOS.
    // Pair measurement and its window key were moved unchanged. The only
    // planner changes were proposal/closing entry points. The audited native
    // stretch builds have identical bindings and machine-code sections.
    let compatible_admission = match admission.as_str() {
        "d95f8625a64783f494e29355c6bc583d17439792a16e96fda85fdc8033af18f2" => {
            Some("ef19234f6b6e8d488972a177e9c377f1bf4ac0544bde7d8415f7fe075713266e")
        }
        // Compared with this predecessor, the only hashed source change is
        // a read-only TrackPacing runway query and its unit test. Existing
        // pair measurements and their keys have the same semantics.
        "2bac187d539e65b7afb62cc047c8a8d9f7fc5d5e961f54d5cf577400b1396947" => {
            Some("53050d9ff9e4770e11a6baf400a35e576e84643e32845cae6a286cfcfbdeea8c")
        }
        // Selector reviews add diagnostics to the same ranked variants and
        // retain their existing admission tests, scores and window keys.
        "6f4e538f49594b0d9e66eb4d7a7010bacf4736154d86bc81ce9508b538527a28" => Some(concat!(
            "2bac187d539e65b7afb62cc047c8a8d9f7fc5d5e961f54d5cf577400b1396947,",
            "53050d9ff9e4770e11a6baf400a35e576e84643e32845cae6a286cfcfbdeea8c"
        )),
        // Earlier schedules retain their original admission rules. Their
        // receipts lack the additional exits and remain eligible for refresh.
        "fbe600346e9f3839d50ab948fa551040eb472cc51f2a2f0d999857629ba3ad23" => Some(concat!(
            "6f4e538f49594b0d9e66eb4d7a7010bacf4736154d86bc81ce9508b538527a28,",
            "2bac187d539e65b7afb62cc047c8a8d9f7fc5d5e961f54d5cf577400b1396947,",
            "53050d9ff9e4770e11a6baf400a35e576e84643e32845cae6a286cfcfbdeea8c"
        )),
        // Additional accompaniment exit frames use the existing admission
        // gates. The older schedules and source pins remain valid.
        "b336db7aedf26fc9713e26c5732ec8a078b3e11196c3eb8141433fc6a01de8dc" => Some(concat!(
            "fbe600346e9f3839d50ab948fa551040eb472cc51f2a2f0d999857629ba3ad23,",
            "6f4e538f49594b0d9e66eb4d7a7010bacf4736154d86bc81ce9508b538527a28,",
            "2bac187d539e65b7afb62cc047c8a8d9f7fc5d5e961f54d5cf577400b1396947,",
            "53050d9ff9e4770e11a6baf400a35e576e84643e32845cae6a286cfcfbdeea8c"
        )),
        // Classification binds existing failed gates to measured evidence.
        // Earlier admitted schedules remain reusable under the same pins.
        "3ee01dfc471f5b8ac67eb825b9df73b9fd58226423a2f007d6338528c46099c8" => Some(concat!(
            "b336db7aedf26fc9713e26c5732ec8a078b3e11196c3eb8141433fc6a01de8dc,",
            "fbe600346e9f3839d50ab948fa551040eb472cc51f2a2f0d999857629ba3ad23,",
            "6f4e538f49594b0d9e66eb4d7a7010bacf4736154d86bc81ce9508b538527a28,",
            "2bac187d539e65b7afb62cc047c8a8d9f7fc5d5e961f54d5cf577400b1396947,",
            "53050d9ff9e4770e11a6baf400a35e576e84643e32845cae6a286cfcfbdeea8c"
        )),
        // Detector completion adds negative evidence for unchanged schedules.
        "585f676723b736d0c4ee5cda591272a1c182991d9f93153905e215169d6000e6" => Some(concat!(
            "3ee01dfc471f5b8ac67eb825b9df73b9fd58226423a2f007d6338528c46099c8,",
            "b336db7aedf26fc9713e26c5732ec8a078b3e11196c3eb8141433fc6a01de8dc,",
            "fbe600346e9f3839d50ab948fa551040eb472cc51f2a2f0d999857629ba3ad23,",
            "6f4e538f49594b0d9e66eb4d7a7010bacf4736154d86bc81ce9508b538527a28,",
            "2bac187d539e65b7afb62cc047c8a8d9f7fc5d5e961f54d5cf577400b1396947,",
            "53050d9ff9e4770e11a6baf400a35e576e84643e32845cae6a286cfcfbdeea8c"
        )),
        // Existing eligibility gates now certify additional negative rows.
        // Candidate schedules and positive admission use the same predicates.
        "34a1ffd3dbd7961499df9a1ff4b51909df27c622afb51dde02f8d356e7c32568" => Some(concat!(
            "585f676723b736d0c4ee5cda591272a1c182991d9f93153905e215169d6000e6,",
            "3ee01dfc471f5b8ac67eb825b9df73b9fd58226423a2f007d6338528c46099c8,",
            "b336db7aedf26fc9713e26c5732ec8a078b3e11196c3eb8141433fc6a01de8dc,",
            "fbe600346e9f3839d50ab948fa551040eb472cc51f2a2f0d999857629ba3ad23,",
            "6f4e538f49594b0d9e66eb4d7a7010bacf4736154d86bc81ce9508b538527a28,",
            "2bac187d539e65b7afb62cc047c8a8d9f7fc5d5e961f54d5cf577400b1396947,",
            "53050d9ff9e4770e11a6baf400a35e576e84643e32845cae6a286cfcfbdeea8c"
        )),
        // Skip profiling outside the window geometry and skip cut matching
        // when the selected schedule has no headroom estimate. Both gates
        // already prevent positive admission in the predecessor profiles.
        "808c9cd09bd49e4dbf38efc61bb2d2bd4b46c9e359d0e1743900b462a824ef99"
        // Snapshot reuse changes metadata loading and pins recorder identity
        // to the same bytes as analysis. Logical record bytes and checksums
        // remain the same, and the original admission namespace is reusable.
        | "fbedddc2ae430fdbe0af8135f09481afbfa6efc9e6105155d1be061d1de16143" => Some(concat!(
            "808c9cd09bd49e4dbf38efc61bb2d2bd4b46c9e359d0e1743900b462a824ef99,",
            "34a1ffd3dbd7961499df9a1ff4b51909df27c622afb51dde02f8d356e7c32568,",
            "585f676723b736d0c4ee5cda591272a1c182991d9f93153905e215169d6000e6,",
            "3ee01dfc471f5b8ac67eb825b9df73b9fd58226423a2f007d6338528c46099c8,",
            "b336db7aedf26fc9713e26c5732ec8a078b3e11196c3eb8141433fc6a01de8dc,",
            "fbe600346e9f3839d50ab948fa551040eb472cc51f2a2f0d999857629ba3ad23,",
            "6f4e538f49594b0d9e66eb4d7a7010bacf4736154d86bc81ce9508b538527a28,",
            "2bac187d539e65b7afb62cc047c8a8d9f7fc5d5e961f54d5cf577400b1396947,",
            "53050d9ff9e4770e11a6baf400a35e576e84643e32845cae6a286cfcfbdeea8c"
        )),
        _ => None,
    };
    if let Some(previous) = compatible_admission.filter(|_| audited_baseline_toolchain()) {
        println!("cargo:rustc-env=MIXLIB_COMPATIBLE_ADMISSION_HASH={previous}");
    }

    add(&mut proposals, admission.as_bytes());
    for name in NON_ACOUSTIC_PLANNER {
        if !matches!(*name, "closing_bookends.rs" | "set_replays.rs") {
            fingerprint(&here.join("../planner/src").join(name), &mut proposals, &[]);
        }
    }
    fingerprint(&here.join("src/duration_proposals.rs"), &mut proposals, &[]);
    let proposals = format!("{:x}", proposals.finalize());
    println!("cargo:rustc-env=MIXLIB_PROPOSAL_RULES_HASH={proposals}");

    add(&mut state, proposals.as_bytes());
    for source in [
        "src/chain.rs",
        "src/composition.rs",
        "src/complete_refinement.rs",
        "src/cp_bar_coverage.rs",
        "src/cp_archive.rs",
        "src/cp_closing.rs",
        "src/cp_cue_scans.rs",
        "src/cp_manifest.rs",
        "src/cp_search.rs",
        "src/cp_startup.rs",
        "src/discovery_schedule.rs",
        "src/duration_continuations.rs",
        "src/duration_oracle.rs",
        "src/duration_search.rs",
        "src/historical_seeds.rs",
        "src/search_cache.rs",
        "src/set_config.rs",
        "src/refinement_branches.rs",
        "src/refinement_layout.rs",
        "src/required_pairs.rs",
        "src/set_replays.rs",
        "../planner/src/closing_bookends.rs",
        "../planner/src/set_replays.rs",
        "../set-search/src",
        "../../Cargo.lock",
    ] {
        fingerprint(&here.join(source), &mut state, &[]);
    }
    println!(
        "cargo:rustc-env=MIXLIB_MUSICAL_RULES_HASH={:x}",
        state.finalize()
    );
}
