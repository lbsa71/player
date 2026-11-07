use clap::{Parser, Subcommand};
use mixlib_catalog::*;
use mixlib_types::*;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Parser)]
#[command(
    name = "mixlib",
    version,
    about = "Local WAV analysis, JSON catalogue and reproducible mixes"
)]
struct Args {
    #[arg(long, global = true, env = "MIXLIB_LIBRARY")]
    library: Option<PathBuf>,
    #[arg(long, global = true)]
    json: bool,
    #[arg(long, global = true)]
    wait_lock: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Init,
    Scan {
        #[arg(long)]
        source: Vec<PathBuf>,
        #[arg(long)]
        verify_content: bool,
    },
    Analyze {
        /// Analyze only these track IDs; repeat to select multiple recordings.
        #[arg(long)]
        track: Vec<String>,
        #[arg(long)]
        resume: bool,
        #[arg(
            long,
            help = "Explicitly disable neural rhythm; produce unknown-grid/fallback-only analysis"
        )]
        dsp_only: bool,
    },
    Inspect {
        #[arg(long)]
        track: Option<String>,
    },
    Plan {
        #[arg(long, conflicts_with = "duration")]
        all: bool,
        #[arg(long,value_parser=parse_duration)]
        duration: Option<f64>,
        #[arg(long, default_value_t = 42)]
        seed: u64,
        #[arg(long)]
        track: Vec<String>,
        #[arg(long)]
        start: Option<String>,
        #[arg(long)]
        end: Option<String>,
        #[arg(long,value_parser=["float32","pcm24"],default_value="float32")]
        encoding: String,
        #[arg(long)]
        no_fallback: bool,
    },
    Preview {
        #[arg(long)]
        plan: String,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long, default_value_t = 3.)]
        context_seconds: f64,
        #[arg(long)]
        overwrite: bool,
    },
    Render {
        #[arg(long)]
        plan: String,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        overwrite: bool,
    },
    /// Stretch two short excerpts to a shared tempo and render their beat-matched crossfade.
    Stitch {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long, default_value_t = 8)]
        overlap_beats: usize,
        #[arg(long, default_value_t = 8)]
        context_beats: usize,
        /// Maximum fractional speed change per recording (e.g. 0.10 for 10%).
        #[arg(long)]
        max_stretch: Option<f64>,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        overwrite: bool,
    },
    Play {
        #[arg(long)]
        plan: String,
    },
    Validate,
    RebuildCatalog,
    /// Fuse a previous separate-file catalogue into the recorder's WAV sidecars.
    FuseSidecars,
    /// Import frame-anchored corrections after review (see overrides schema).
    Correct {
        #[arg(long)]
        track: String,
        #[arg(long)]
        input: PathBuf,
    },
    /// Export Draft 2020-12 schemas and default configurations.
    Schemas {
        #[arg(long)]
        output: PathBuf,
    },
}
fn parse_duration(s: &str) -> std::result::Result<f64, String> {
    let (n, m) = if let Some(n) = s.strip_suffix('m') {
        (n, 60.)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3600.)
    } else {
        (s.strip_suffix('s').unwrap_or(s), 1.)
    };
    let n = n
        .parse::<f64>()
        .map_err(|_| "expected duration such as 60m, 1h or 30s".to_string())?
        * m;
    if n.is_finite() && n > 0. {
        Ok(n)
    } else {
        Err("duration must be positive and finite".into())
    }
}
fn main() {
    let args = Args::parse();
    let cancel = Cancellation::default();
    let signal = cancel.clone();
    ctrlc::set_handler(move || {
        signal.cancel();
        eprintln!("Cancelling at the next safe processing boundary…");
    })
    .expect("install SIGINT handler");
    match run(&args, &cancel) {
        Ok(value) => {
            if args.json {
                println!("{}", serde_json::to_string(&value).unwrap());
            } else {
                println!("{}", serde_json::to_string_pretty(&value).unwrap());
            }
            if value
                .get("counts")
                .and_then(|c| c.get("failed"))
                .and_then(|n| n.as_u64())
                .unwrap_or(0)
                > 0
            {
                std::process::exit(3);
            }
        }
        Err(e) => {
            if args.json {
                println!("{}", serde_json::json!({"error":e}));
            }
            eprintln!("{e}");
            std::process::exit(if e.code == "CANCELLED" { 130 } else { 2 });
        }
    }
}
fn run(args: &Args, cancel: &Cancellation) -> Result<serde_json::Value> {
    if let Command::Schemas { output } = &args.command {
        schemas(output)?;
        return Ok(serde_json::json!({"schemas":output}));
    }
    let root = args
        .library
        .clone()
        .map(Ok)
        .unwrap_or_else(default_library_directory)?;
    if matches!(args.command, Command::Init) {
        let store = Store::init(&root)?;
        eprintln!("library: {}", store.root.display());
        return Ok(serde_json::to_value(
            store.read::<Library>("library.json")?,
        )?);
    }
    let store = if args.library.is_none()
        && matches!(args.command, Command::Scan { .. })
        && !root.join("library.json").exists()
    {
        Store::init(&root)?
    } else {
        Store::open(&root)?
    };
    eprintln!("library: {}", store.root.display());
    let needs_writer = matches!(
        args.command,
        Command::Scan { .. }
            | Command::Analyze { .. }
            | Command::Plan { .. }
            | Command::Stitch { .. }
            | Command::RebuildCatalog
            | Command::FuseSidecars
            | Command::Correct { .. }
            | Command::Render { .. }
            | Command::Preview { .. }
            | Command::Play { .. }
    );
    let _writer = if needs_writer {
        Some(store.lock(args.wait_lock, cancel)?)
    } else {
        None
    };
    let result = match &args.command {
        Command::Scan {
            source,
            verify_content,
        } => {
            let source = if source.is_empty()
                && store
                    .read::<Library>("library.json")?
                    .source_roots
                    .is_empty()
            {
                vec![default_audio_directory()?]
            } else {
                source.clone()
            };
            serde_json::to_value(scan(&store, &source, *verify_content, cancel)?)?
        }
        Command::Analyze {
            resume,
            dsp_only,
            track,
        } => serde_json::to_value(mixlib_analysis::analyze_selected(
            &store, *resume, *dsp_only, track, cancel,
        )?)?,
        Command::Inspect { track } => {
            if let Some(id) = track {
                let t = store.track(id)?;
                let analysis = t
                    .active_analysis
                    .as_ref()
                    .map(|r| store.read_ref::<AnalysisManifest>(r))
                    .transpose()?;
                let regions = analysis
                    .as_ref()
                    .and_then(|a| a.artifacts.get("regions"))
                    .map(|r| store.read_ref::<Regions>(r))
                    .transpose()?;
                let overrides = store.overrides(&t)?;
                let recording = store.recording_metadata(&t)?;
                serde_json::json!({"track":t,"recording":recording,"analysis":analysis,"regions":regions,"overrides":overrides})
            } else {
                serde_json::to_value(store.tracks()?)?
            }
        }
        Command::Plan {
            duration,
            seed,
            track,
            start,
            end,
            encoding,
            no_fallback,
            ..
        } => {
            let mut policy: MixingConfig = store.read("config/mixing.json")?;
            if *no_fallback {
                policy.allow_fallback = false;
            }
            let mut p = mixlib_planner::plan(
                &store,
                &mixlib_planner::Request {
                    tracks: track.clone(),
                    seed: *seed,
                    target_seconds: *duration,
                    fixed_start: start.clone(),
                    fixed_end: end.clone(),
                },
                &policy,
                cancel,
            )?;
            p.output.encoding = encoding.clone();
            store.write(&format!("plans/{}.json", p.plan_id), &p)?;
            serde_json::to_value(p)?
        }
        Command::Render {
            plan,
            output,
            overwrite,
        } => {
            let p = load_plan(&store, plan)?;
            let sources = verify_plan(&store, &p, cancel)?;
            protect_output(&store, output)?;
            let r = mixlib_audio::render(&p, &sources, output, *overwrite, None, cancel)?;
            store.report(&r)?;
            serde_json::to_value(r)?
        }
        Command::Stitch {
            from,
            to,
            overlap_beats,
            context_beats,
            max_stretch,
            output,
            overwrite,
        } => {
            protect_output(&store, output)?;
            let mut policy: MixingConfig = store.read("config/mixing.json")?;
            if let Some(limit) = max_stretch {
                policy.max_speed_deviation = *limit;
            }
            let plan = mixlib_planner::stitch(
                &store,
                &mixlib_planner::StitchRequest {
                    from,
                    to,
                    overlap_beats: *overlap_beats,
                    context_beats: *context_beats,
                },
                &policy,
                cancel,
            )?;
            let sources = verify_plan(&store, &plan, cancel)?;
            let report = mixlib_audio::render(&plan, &sources, output, *overwrite, None, cancel)?;
            store.write(&format!("plans/{}.json", plan.plan_id), &plan)?;
            store.report(&report)?;
            serde_json::json!({"output": output, "plan": plan, "report": report})
        }
        Command::Preview {
            plan,
            output,
            context_seconds,
            overwrite,
        } => {
            let output = output
                .clone()
                .unwrap_or_else(|| store.root.join("previews").join(plan));
            require(
                context_seconds.is_finite() && (0.0..=60.).contains(context_seconds),
                "PREVIEW_CONTEXT",
                "preview context must be 0..60 seconds",
            )?;
            let p = load_plan(&store, plan)?;
            let sources = verify_plan(&store, &p, cancel)?;
            protect_output(&store, &output.join("preview.wav"))?;
            std::fs::create_dir_all(&output)?;
            let pad = (context_seconds * p.output.sample_rate_hz as f64) as u64;
            let mut reports = vec![];
            for (i, t) in p.transitions.iter().enumerate() {
                let first = t.start_output_frame.saturating_sub(pad);
                let last = (t.end_output_frame + pad)
                    .min(p.total_output_frames)
                    .max(first + 1);
                let r = mixlib_audio::render(
                    &p,
                    &sources,
                    &output.join(format!("transition-{:03}.wav", i + 1)),
                    *overwrite,
                    Some((first, last)),
                    cancel,
                )?;
                store.report(&r)?;
                reports.push(r);
            }
            serde_json::to_value(reports)?
        }
        Command::Play { plan } => {
            require(
                cfg!(target_os = "macos"),
                "PLAYBACK_PLATFORM",
                "CLI playback currently uses the macOS audio host",
            )?;
            let p = load_plan(&store, plan)?;
            let sources = verify_plan(&store, &p, cancel)?;
            let temp = tempfile::tempdir()?;
            let output = temp.path().join("playback.wav");
            let report = mixlib_audio::render(&p, &sources, &output, false, None, cancel)?;
            eprintln!(
                "Playback ready; Ctrl-C stops playback. Rendering uses the same plan as export."
            );
            let mut child = std::process::Command::new("/usr/bin/afplay")
                .arg(&output)
                .spawn()?;
            loop {
                if let Err(e) = cancel.check() {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(e);
                }
                if let Some(status) = child.try_wait()? {
                    require(
                        status.success(),
                        "PLAYBACK",
                        format!("audio host exited with {status}"),
                    )?;
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            serde_json::to_value(report)?
        }
        Command::Validate => serde_json::to_value(validate(&store, cancel)?)?,
        Command::RebuildCatalog => serde_json::to_value(rebuild_catalog(&store)?)?,
        Command::FuseSidecars => serde_json::to_value(store.fuse_sidecars()?)?,
        Command::Correct { track, input } => {
            let overrides: Overrides = read_record(input)?;
            let t = store.track(track)?;
            let analysis = store.analysis(&t)?;
            let mut beats: Beats = store.read_ref(&analysis.artifacts["beats"])?;
            let mut regions: Regions = store.read_ref(&analysis.artifacts["regions"])?;
            mixlib_analysis::apply_overrides(
                &t,
                t.active_analysis.as_ref().unwrap(),
                &mut beats,
                &mut regions,
                &overrides,
            )?;
            store.set_overrides(track, overrides)?;
            serde_json::to_value(store.overrides(&store.track(track)?)?)?
        }
        Command::Init | Command::Schemas { .. } => unreachable!(),
    };
    Ok(result)
}
fn load_plan(store: &Store, id: &str) -> Result<MixPlan> {
    valid_id(id)?;
    store.read(&format!("plans/{id}.json"))
}
fn protect_output(store: &Store, output: &Path) -> Result<()> {
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // Resolve the nearest existing ancestor before creating anything: even a
    // rejected output must not create directories in a read-only source root.
    let mut existing = std::path::absolute(parent)?;
    let mut tail = Vec::new();
    while !existing.exists() {
        tail.push(
            existing
                .file_name()
                .ok_or_else(|| Error::new("OUTPUT_PATH", "invalid output parent"))?
                .to_os_string(),
        );
        require(existing.pop(), "OUTPUT_PATH", "no existing output ancestor")?;
    }
    let mut parent = std::fs::canonicalize(existing)?;
    for component in tail.into_iter().rev() {
        parent.push(component);
    }
    let mut lib: Library = store.read("library.json")?;
    require(
        !lib.source_roots.iter().any(|r| parent.starts_with(&r.path)),
        "OUTPUT_IN_SOURCE",
        "choose a render directory outside all source roots",
    )?;
    std::fs::create_dir_all(&parent)?;
    if !parent.starts_with(&store.root)
        && !lib
            .excluded_directories
            .iter()
            .any(|p| Path::new(p) == parent)
    {
        lib.excluded_directories
            .push(parent.to_string_lossy().into());
        store.write("library.json", &lib)?;
    }
    Ok(())
}
fn schemas(output: &Path) -> Result<()> {
    std::fs::create_dir_all(output)?;
    macro_rules! emit {
        ($name:literal,$t:ty) => {
            std::fs::write(
                output.join(concat!($name, ".schema.json")),
                serde_json::to_vec_pretty(&schema::<$t>())?,
            )?;
        };
    }
    emit!("library", Library);
    emit!("sidecar-analyzer", SidecarBundle);
    emit!("sidecar-index", SidecarIndex);
    emit!("track", Track);
    emit!("analysis", AnalysisManifest);
    emit!("beats", Beats);
    emit!("harmony", Harmony);
    emit!("regions", Regions);
    emit!("features", FeatureChunk);
    emit!("overrides", Overrides);
    emit!("plan", MixPlan);
    emit!("time-map", TimeMap);
    emit!("job", Job);
    emit!("report", Report);
    emit!("catalog", CatalogManifest);
    emit!("catalog-shard", CatalogShard);
    emit!("analysis-config", AnalysisConfig);
    emit!("mixing-config", MixingConfig);
    emit!("models", ModelManifest);
    std::fs::write(
        output.join("analysis.example.json"),
        encode(&AnalysisConfig::default())?,
    )?;
    std::fs::write(
        output.join("mixing.example.json"),
        encode(&MixingConfig::default())?,
    )?;
    Ok(())
}
