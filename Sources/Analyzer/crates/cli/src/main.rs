mod chain;
mod measured_beam;
mod tactic_analysis;
mod tactics;
mod timetable;

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
    /// Cache full-track stems, vocal phrases and signal evidence for tactic selection.
    AnalyzeTactics {
        /// Automatically prepare N new tracks with connections into the prepared pool.
        #[arg(long, conflicts_with="track", value_parser=clap::value_parser!(u16).range(1..=128))]
        expand_pool: Option<u16>,
        #[arg(long)]
        track: Vec<String>,
        #[arg(long, env = "MIXLIB_SEPARATOR_PYTHON")]
        separator_python: PathBuf,
    },
    Inspect {
        #[arg(long)]
        track: Option<String>,
        /// Include one- to four-bar texture and timing descriptions.
        #[arg(long, requires = "track")]
        bars: bool,
        /// Previous incoming transition end, in source time, for a pacing audit.
        #[arg(long, requires_all=["track","pacing_exit"], value_parser=parse_duration)]
        pacing_after: Option<f64>,
        /// Proposed next transition start, in source time.
        #[arg(long, requires_all=["track","pacing_after"], value_parser=parse_duration)]
        pacing_exit: Option<f64>,
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
    /// Export the saved timetable, measured matching evidence and full-set comparisons.
    Timetable {
        #[arg(long)]
        plan: String,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        overwrite: bool,
    },
    /// Inspect measured eligibility for all mixing tactics, or save a selected schedule.
    Tactics {
        #[arg(long)]
        plan: String,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        allow_expressive: bool,
        #[arg(long, value_parser=parse_tactic, requires="apply")]
        tactic: Option<TacticId>,
    },
    /// Choose cues and mixing tactics for two to eight tracks, then render a combined WAV.
    Stitch {
        /// Choose and order four tracks from the analyzed library.
        #[arg(long, conflicts_with_all = ["from", "to", "then", "overlap_beats", "beat_return_near", "drum_lead_bars"])]
        pick_four: bool,
        /// Choose and order eight tracks from the analyzed library.
        #[arg(long, conflicts_with_all = ["pick_four", "from", "to", "then", "overlap_beats", "beat_return_near", "drum_lead_bars"])]
        pick_eight: bool,
        #[arg(long, required_unless_present_any = ["pick_four", "pick_eight"])]
        from: Option<String>,
        #[arg(long, required_unless_present_any = ["pick_four", "pick_eight"])]
        to: Option<String>,
        /// Add another track after --to. Repeat for up to eight tracks.
        #[arg(long, conflicts_with_all = ["overlap_beats", "beat_return_near", "drum_lead_bars"])]
        then: Vec<String>,
        /// Require a three-band exchange at this numbered join. Repeat to select more joins.
        #[arg(long, requires = "then")]
        band_join: Vec<usize>,
        #[arg(long)]
        overlap_beats: Option<usize>,
        #[arg(long)]
        context_beats: Option<usize>,
        /// Maximum fractional speed change per recording (e.g. 0.10 for 10%).
        #[arg(long)]
        max_stretch: Option<f64>,
        /// Permit expressive transitions when their measured constraints pass.
        #[arg(long)]
        allow_expressive: bool,
        /// Lead with this many bars of separated drums during a quiet outgoing passage.
        #[arg(long, requires = "separator_python")]
        drum_lead_bars: Option<usize>,
        /// Find a returning beat near this outgoing source time, then cut at the next bar.
        #[arg(long, requires = "separator_python", conflicts_with = "drum_lead_bars")]
        beat_return_near: Option<f64>,
        /// Use an incoming downbeat near this source time for the full-mix cut.
        #[arg(long, requires = "beat_return_near")]
        incoming_cut_near: Option<f64>,
        /// Keep outgoing non-drum layers through this source time, then fade over one beat.
        #[arg(long, requires = "beat_return_near")]
        vocal_tail_until: Option<f64>,
        /// Stage the accompaniment over four bars and detect a gap for the vocal handoff.
        #[arg(
            long,
            requires = "beat_return_near",
            conflicts_with = "vocal_tail_until"
        )]
        auto_vocal_handoff: bool,
        /// Python environment for local stem, vocal-phrase and tactic analysis.
        #[arg(long, env = "MIXLIB_SEPARATOR_PYTHON")]
        separator_python: Option<PathBuf>,
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
fn parse_tactic(s: &str) -> std::result::Result<TacticId, String> {
    serde_json::from_value(serde_json::Value::String(s.to_ascii_uppercase()))
        .map_err(|_| "unknown tactic ID; see MIXING_TACTICS.md".into())
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
            | Command::AnalyzeTactics { .. }
            | Command::Plan { .. }
            | Command::Stitch { .. }
            | Command::Tactics { apply: true, .. }
            | Command::RebuildCatalog
            | Command::FuseSidecars
            | Command::Correct { .. }
            | Command::Render { .. }
            | Command::Timetable { .. }
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
        Command::AnalyzeTactics {
            expand_pool,
            track,
            separator_python,
        } => {
            if let Some(count) = expand_pool {
                tactic_analysis::expand_pool(&store, usize::from(*count), separator_python, cancel)?
            } else {
                serde_json::to_value(tactic_analysis::ensure_tactic_analysis(
                    &store,
                    track,
                    separator_python,
                    cancel,
                )?)?
            }
        }
        Command::Inspect {
            track,
            bars,
            pacing_after,
            pacing_exit,
        } => {
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
                let mut result = serde_json::json!({"track":t,"recording":recording,"analysis":analysis,"regions":regions,"overrides":overrides});
                if let (Some(after), Some(exit)) = (pacing_after, pacing_exit) {
                    let rate = f64::from(t.format.sample_rate_hz);
                    require(
                        *exit <= t.format.frames as f64 / rate && after <= exit,
                        "PACING_WINDOW",
                        "pacing interval must lie inside the recording",
                    )?;
                    let pacing = mixlib_planner::read_track_pacing(&store, id, cancel)?;
                    let entry_end = (after * rate).round() as u64;
                    let decision = pacing.assess(entry_end, (exit * rate).round() as u64);
                    result["pacing"] = serde_json::json!({"admitted":decision.is_some(),"decision":decision,
                        "earliest_possible_exit_seconds":pacing.earliest_exit(entry_end) as f64/rate,
                        "scope":"Pacing only; concrete tactic vocal, buildup, layer and overlap admission remains required"});
                }
                if *bars {
                    result["segments"] = analysis
                        .as_ref()
                        .and_then(|a| a.artifacts.get("segments"))
                        .map(|reference| store.read_ref::<SegmentAnalysis>(reference))
                        .transpose()?
                        .map(serde_json::to_value)
                        .transpose()?
                        .unwrap_or(serde_json::Value::Null);
                    result["bar_tiles"] = serde_json::to_value(
                        mixlib_planner::describe_bar_tiles(&store, t, cancel)?,
                    )?;
                }
                result
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
            tactics::automatic(&store, &mut p, cancel)?;
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
            let table_path = timetable::sidecar(output);
            timetable::preflight(&store, &table_path, *overwrite)?;
            let (evidence, markdown) = timetable::build(&store, &p, cancel)?;
            let mut r = mixlib_audio::render(&p, &sources, output, *overwrite, None, cancel)?;
            timetable::write(&table_path, &markdown, *overwrite)?;
            r.header.extensions.insert("timetable".into(), evidence);
            store.report(&r)?;
            serde_json::to_value(r)?
        }
        Command::Timetable {
            plan,
            output,
            overwrite,
        } => {
            timetable::preflight(&store, output, *overwrite)?;
            let p = load_plan(&store, plan)?;
            let (data, markdown) = timetable::build(&store, &p, cancel)?;
            timetable::write(output, &markdown, *overwrite)?;
            serde_json::json!({"output":output,"timetable":data})
        }
        Command::Tactics {
            plan,
            apply,
            allow_expressive,
            tactic,
        } => {
            let mut proposed = load_plan(&store, plan)?;
            if *apply {
                proposed.plan_id = id();
            }
            let result = tactics::select(
                &store,
                &mut proposed,
                *apply,
                *allow_expressive,
                *tactic,
                cancel,
            )?;
            if *apply {
                store.write(&format!("plans/{}.json", proposed.plan_id), &proposed)?;
            }
            serde_json::json!({"plan_id":proposed.plan_id,"result":result})
        }
        Command::Stitch {
            pick_four,
            pick_eight,
            from,
            to,
            then,
            band_join,
            overlap_beats,
            context_beats,
            max_stretch,
            allow_expressive,
            drum_lead_bars,
            beat_return_near,
            incoming_cut_near,
            vocal_tail_until,
            auto_vocal_handoff,
            separator_python,
            output,
            overwrite,
        } => {
            protect_output(&store, output)?;
            let mut policy: MixingConfig = store.read("config/mixing.json")?;
            if let Some(limit) = max_stretch {
                policy.max_speed_deviation = *limit;
            }
            if *allow_expressive {
                policy
                    .header
                    .extensions
                    .insert("allow_expressive_tactics".into(), serde_json::json!(true));
            }
            if *pick_four || *pick_eight {
                let count = if *pick_eight { 8 } else { 4 };
                let plan = chain::pick_chain(
                    &store,
                    &policy,
                    context_beats.unwrap_or(12),
                    count,
                    separator_python.as_deref(),
                    cancel,
                )?;
                return render_stitch(&store, plan, output, *overwrite, cancel);
            }
            let from = from.as_ref().expect("from required for explicit stitch");
            let to = to.as_ref().expect("to required for explicit stitch");
            if !then.is_empty() {
                let tracks: Vec<_> = [from.as_str(), to.as_str()]
                    .into_iter()
                    .chain(then.iter().map(String::as_str))
                    .collect();
                let plan = chain::stitch_chain(
                    &store,
                    &tracks,
                    &policy,
                    context_beats.unwrap_or(12),
                    separator_python.as_deref(),
                    max_stretch.is_none(),
                    band_join,
                    cancel,
                )?;
                return render_stitch(&store, plan, output, *overwrite, cancel);
            }
            let automatic_python = separator_python.as_deref().filter(|_| {
                beat_return_near.is_none() && drum_lead_bars.is_none() && overlap_beats.is_none()
            });
            let request = mixlib_planner::StitchRequest {
                from,
                to,
                overlap_beats: overlap_beats.unwrap_or(8),
                context_beats: context_beats.unwrap_or(if automatic_python.is_some() {
                    12
                } else {
                    8
                }),
                outgoing_after_frame: 0,
            };
            let (mut plan, handoff) = match (beat_return_near, drum_lead_bars) {
                (Some(near), _) => {
                    let (plan, handoff) = mixlib_planner::beat_return_stitch(
                        &store,
                        &request,
                        &policy,
                        mixlib_planner::BeatReturnCue {
                            outgoing_near: *near,
                            incoming_cut_near: *incoming_cut_near,
                            vocal_tail_until: *vocal_tail_until,
                            auto_vocal_handoff: *auto_vocal_handoff,
                        },
                        cancel,
                    )?;
                    (plan, Some(handoff))
                }
                (None, Some(bars)) => {
                    let (plan, handoff) =
                        mixlib_planner::drum_stitch(&store, &request, &policy, *bars, cancel)?;
                    (plan, Some(handoff))
                }
                (None, None) if automatic_python.is_some() => {
                    let python = automatic_python.unwrap();
                    let analysis = tactic_analysis::ensure_tactic_analysis(
                        &store,
                        &[from.clone(), to.clone()],
                        python,
                        cancel,
                    )?;
                    require(
                        analysis.counts.get("failed").copied().unwrap_or(0) == 0,
                        "TACTIC_ANALYSIS",
                        "tactic analysis failed; inspect its saved report",
                    )?;
                    let choice = tactics::choose_pair(&store, &request, &policy, None, 0, cancel)?;
                    let mut plan = if let Some(plan) = choice.selected {
                        plan
                    } else {
                        match chain::try_quiet_linger(
                            &store, &request, None, &policy, python, cancel,
                        )? {
                            Some(plan) => plan,
                            None => automatic_stitch(
                                &store,
                                &request,
                                &policy,
                                max_stretch.is_none(),
                                python,
                                cancel,
                            )?,
                        }
                    };
                    plan.header.extensions.insert(
                        "joint_tactic_search".into(),
                        serde_json::json!(choice.examined),
                    );
                    (plan, None)
                }
                (None, None) if overlap_beats.is_none() => {
                    let choice = tactics::choose_pair(&store, &request, &policy, None, 0, cancel)?;
                    (
                        match choice.selected {
                            Some(plan) => plan,
                            None => return Err(Error::new("TACTIC_ANALYSIS_REQUIRED",
                                "automatic stitching requires full tactic analysis; run analyze-tactics or configure MIXLIB_SEPARATOR_PYTHON")),
                        },
                        None,
                    )
                }
                (None, None) => (
                    mixlib_planner::stitch(&store, &request, &policy, cancel)?,
                    None,
                ),
            };
            let mut sources = verify_plan(&store, &plan, cancel)?;
            if let (Some(handoff), Some(python)) = (handoff, separator_python) {
                let swap = beat_return_near.is_some();
                let intro = if let Some(bars) = handoff.vocal_bars {
                    prepare_vocal_handoff(
                        &store,
                        &mut plan,
                        [&sources[from], &sources[to]],
                        &bars,
                        python,
                        cancel,
                    )?
                } else {
                    let incoming = prepare_drum_clip(
                        &store,
                        &plan,
                        &sources[to],
                        if vocal_tail_until.is_some() {
                            "fade"
                        } else if swap {
                            "cut"
                        } else {
                            "bands"
                        },
                        handoff.source_frame,
                        python,
                        cancel,
                    )?;
                    let outgoing = if swap {
                        Some(prepare_drum_clip(
                            &store,
                            &plan,
                            &sources[from],
                            "remove",
                            plan.placements[0].time_map.anchors[1].source_frame,
                            python,
                            cancel,
                        )?)
                    } else {
                        None
                    };
                    DrumIntro {
                        audio: incoming.audio,
                        source_offset: incoming.source_offset,
                        accompaniment_start_frame: handoff.output_frame,
                        outgoing,
                    }
                };
                for (key, _, audio, _) in intro.assets() {
                    sources.insert(key.into(), store.path(&audio.path)?);
                }
                plan.drum_intro = Some(intro);
                plan.renderer_version = if swap {
                    mixlib_audio::DRUM_SWAP_RENDERER
                } else {
                    mixlib_audio::DRUM_INTRO_RENDERER
                }
                .into();
            }
            render_stitch(&store, plan, output, *overwrite, cancel)?
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
fn render_stitch(
    store: &Store,
    mut plan: MixPlan,
    output: &Path,
    overwrite: bool,
    cancel: &Cancellation,
) -> Result<serde_json::Value> {
    tactics::automatic(store, &mut plan, cancel)?;
    let sources = verify_plan(store, &plan, cancel)?;
    let table_path = timetable::sidecar(output);
    timetable::preflight(store, &table_path, overwrite)?;
    let (evidence, markdown) = timetable::build(store, &plan, cancel)?;
    plan.header.extensions.insert("timetable".into(), evidence);
    let report = mixlib_audio::render(&plan, &sources, output, overwrite, None, cancel)?;
    timetable::write(&table_path, &markdown, overwrite)?;
    store.write(&format!("plans/{}.json", plan.plan_id), &plan)?;
    store.report(&report)?;
    Ok(
        serde_json::json!({"output": output, "timetable":table_path, "plan": plan, "report": report}),
    )
}

fn prepare_drum_clip(
    store: &Store,
    plan: &MixPlan,
    source: &Path,
    mode: &str,
    handoff_frame: u64,
    python: &Path,
    cancel: &Cancellation,
) -> Result<DrumRemoval> {
    let outgoing = mode == "remove";
    let incoming = &plan.placements[usize::from(!outgoing)];
    let bounds = ClipBounds::new(source, incoming)?;
    let ClipBounds { offset, end, .. } = bounds;
    let crossfade_end = incoming.time_map.anchors[1].source_frame;
    let directory = store.directory("stems")?;
    let temp = tempfile::tempdir_in(directory)?;
    let prepared = temp.path().join("prepared.wav");
    let mut command = std::process::Command::new(python);
    command
        .arg("-c")
        .arg(include_str!("../../../scripts/prepare-drum-intro.py"))
        .arg(source)
        .arg(&prepared)
        .arg(mode)
        .args([offset, end, handoff_frame, crossfade_end].map(|v| v.to_string()));
    run_separator(&mut command, cancel)?;
    publish_drum_clip(store, plan, &prepared, bounds, outgoing, cancel)
}

fn run_separator(command: &mut std::process::Command, cancel: &Cancellation) -> Result<()> {
    let mut child = command
        .stdout(std::process::Stdio::null())
        .spawn()
        .map_err(|e| Error::new("DRUM_SEPARATOR", e.to_string()))?;
    loop {
        if let Err(error) = cancel.check() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        if let Some(status) = child.try_wait()? {
            require(
                status.success(),
                "DRUM_SEPARATOR",
                format!("local separation exited with {status}"),
            )?;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct ClipBounds {
    offset: u64,
    end: u64,
    rate: u32,
}

impl ClipBounds {
    fn new(source: &Path, placement: &Placement) -> Result<Self> {
        let wav = mixlib_audio::WavReader::open(source)?;
        let rate = wav.format.sample_rate_hz;
        let padding = u64::from(rate) * 2;
        Ok(Self {
            offset: placement.source_start_frame.saturating_sub(padding),
            end: (placement.source_end_frame + padding).min(wav.format.frames),
            rate,
        })
    }
}

fn publish_drum_clip(
    store: &Store,
    plan: &MixPlan,
    prepared: &Path,
    bounds: ClipBounds,
    outgoing: bool,
    cancel: &Cancellation,
) -> Result<DrumRemoval> {
    let ClipBounds { offset, end, rate } = bounds;
    let mut wav = mixlib_audio::WavReader::open(prepared)?;
    require(
        wav.format.sample_rate_hz == rate
            && wav.format.channels == 2
            && wav.format.frames == end - offset,
        "DRUM_INTRO_FORMAT",
        "prepared excerpt must preserve the source timebase and length",
    )?;
    let suffix = if outgoing { "-outgoing" } else { "" };
    let key = format!("stems/{}{suffix}.wav", plan.plan_id);
    let hash = wav.hashes(cancel)?.0;
    let destination = store.path(&key)?;
    std::fs::File::open(prepared)?.sync_all()?;
    std::fs::rename(prepared, &destination)?;
    std::fs::File::open(destination.parent().unwrap())?.sync_all()?;
    Ok(DrumRemoval {
        audio: FileRef { path: key, hash },
        source_offset: offset,
    })
}

fn automatic_stitch(
    store: &Store,
    request: &mixlib_planner::StitchRequest<'_>,
    policy: &MixingConfig,
    use_default_stretch: bool,
    python: &Path,
    cancel: &Cancellation,
) -> Result<MixPlan> {
    let mut stem_policy = policy.clone();
    if use_default_stretch {
        stem_policy.max_speed_deviation = stem_policy.max_speed_deviation.max(0.10);
    }
    let candidates = mixlib_planner::automatic_cues(store, request, &stem_policy, cancel)?;
    let sources = [
        store.source(&store.track(request.from)?)?,
        store.source(&store.track(request.to)?)?,
    ];
    let mut rejected = 0;
    for cue in candidates {
        let (mut plan, handoff) =
            mixlib_planner::beat_return_stitch(store, request, &stem_policy, cue, cancel)?;
        match prepare_vocal_handoff(
            store,
            &mut plan,
            [&sources[0], &sources[1]],
            handoff
                .vocal_bars
                .as_deref()
                .expect("automatic cue has a vocal window"),
            python,
            cancel,
        ) {
            Ok(intro) => {
                plan.drum_intro = Some(intro);
                plan.renderer_version = mixlib_audio::DRUM_SWAP_RENDERER.into();
                plan.diagnostics.push(format!(
                    "automatic selection: quiet break and returning drums, stable incoming intro, safe vocal gap; {rejected} candidates rejected"
                ));
                return Ok(plan);
            }
            Err(error) if error.code == "STITCH_VOCAL_GAP" => rejected += 1,
            Err(error) => return Err(error),
        }
    }
    require(
        policy.allow_fallback,
        "STITCH_AUTO_INFEASIBLE",
        "no automatic vocal handoff and fallback is disabled",
    )?;
    let mut plan = mixlib_planner::stitch(store, request, policy, cancel)?;
    plan.diagnostics.push(format!(
        "automatic selection: beat-matched crossfade; no eligible drum and vocal handoff ({rejected} vocal gaps rejected)"
    ));
    Ok(plan)
}

fn prepare_vocal_handoff(
    store: &Store,
    plan: &mut MixPlan,
    sources: [&Path; 2],
    bars: &[u64],
    python: &Path,
    cancel: &Cancellation,
) -> Result<DrumIntro> {
    let transition = &plan.transitions[0];
    let swap = transition.start_output_frame;
    let rate = f64::from(plan.output.sample_rate_hz);
    let mut mashup = plan.header.extensions.get("mashup").cloned();
    let first_vocal_search_bar = mashup
        .as_ref()
        .and_then(|metadata| metadata.get("hold_bars"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(4) as usize;
    require(
        bars.len() >= 7
            && first_vocal_search_bar >= 4
            && first_vocal_search_bar < bars.len() - 1
            && bars[0] >= swap
            && bars.windows(2).all(|pair| pair[0] < pair[1]),
        "STITCH_VOCAL_WINDOW",
        "invalid vocal handoff bar boundaries",
    )?;
    let mut drum_lead_bars = match plan.objective.as_str() {
        "drum_mashup" => first_vocal_search_bar,
        "quiet_drum_linger" => 2,
        _ => 0,
    };
    let relative_bars: Vec<_> = bars
        .iter()
        .map(|frame| (frame - swap) as f64 / rate)
        .collect();
    let overlap_seconds = *relative_bars.last().unwrap();
    let exit_seconds =
        relative_bars[first_vocal_search_bar + 1] - relative_bars[first_vocal_search_bar];
    let bounds = [
        ClipBounds::new(sources[0], &plan.placements[0])?,
        ClipBounds::new(sources[1], &plan.placements[1])?,
    ];
    let temp = tempfile::tempdir_in(store.directory("stems")?)?;
    let vocal_provider = if let Some(metadata) = mashup.as_mut() {
        let manifest: ModelManifest = store.read("models/manifest.json")?;
        if let Some(model) = manifest
            .models
            .iter()
            .find(|model| model.model_id == "vocal_phrase_whisper")
        {
            let model_path = store.path(&model.file.path)?;
            require(
                std::fs::metadata(&model_path)?.len() <= 512 * 1024 * 1024,
                "MODEL_SIZE",
                "vocal phrase model exceeds 512 MiB",
            )?;
            require(
                hash_bytes(&std::fs::read(&model_path)?) == model.file.hash,
                "MODEL_CHECKSUM",
                "vocal phrase model changed",
            )?;
            let script = temp.path().join("vocal-phrase-evidence.py");
            std::fs::write(
                &script,
                include_str!("../../../scripts/vocal-phrase-evidence.py"),
            )?;
            metadata["exit_policy"] = serde_json::json!("adaptive");
            Some(serde_json::json!({
                "script": script, "model_path": model_path,
                "model_sha256": model.file.hash.digest
            }))
        } else {
            None
        }
    } else {
        None
    };
    let prepared = [temp.path().join("out.wav"), temp.path().join("in.wav")];
    let decision = temp.path().join("decision.json");
    let clips: Vec<_> = (0..2)
        .map(|i| {
            let anchors = &plan.placements[i].time_map.anchors;
            let from = anchors.iter().find(|a| a.output_frame == swap).unwrap();
            let end = anchors
                .iter()
                .find(|a| a.output_frame == transition.end_output_frame)
                .unwrap();
            let warp = plan.header.extensions.contains_key("bar_bpm").then(|| {
                let map = &plan.placements[i].time_map;
                (swap..end.output_frame)
                    .step_by(128)
                    .chain(std::iter::once(end.output_frame))
                    .map(|output| [map.source_at(output as f64), (output - swap) as f64 / rate])
                    .collect::<Vec<_>>()
            });
            serde_json::json!({
                "source": sources[i], "prepared": prepared[i], "offset": bounds[i].offset,
                "end": bounds[i].end, "swap": from.source_frame, "overlap_end": end.source_frame,
                "warp": warp
            })
        })
        .collect();
    let input = temp.path().join("request.json");
    std::fs::write(
        &input,
        serde_json::to_vec(&serde_json::json!({
            "bars": relative_bars, "clips": clips, "decision": decision,
            "drum_lead_bars": drum_lead_bars, "first_vocal_search_bar": first_vocal_search_bar,
            "mashup": mashup, "vocal_phrase_provider": vocal_provider
        }))?,
    )?;
    run_separator(
        std::process::Command::new(python)
            .arg("-c")
            .arg(include_str!("../../../scripts/prepare-drum-intro.py"))
            .arg("vocal-handoff")
            .arg(input),
        cancel,
    )?;
    #[derive(serde::Deserialize)]
    struct MashupDecision {
        hold_until: f64,
        exit_end: f64,
        #[serde(default)]
        exit_policy: String,
    }
    #[derive(serde::Deserialize)]
    #[serde(tag = "status", rename_all = "snake_case")]
    enum Decision {
        Found {
            outgoing_vocals_off: f64,
            incoming_vocals_on: f64,
            mashup: Option<MashupDecision>,
            #[serde(default)]
            evidence: serde_json::Value,
            #[serde(default)]
            tactic_id: Option<String>,
        },
        NoGap {
            reason: String,
        },
    }
    let result: Decision = serde_json::from_slice(&std::fs::read(&decision)?)?;
    let (off_seconds, on_seconds, mashup_decision, evidence, tactic_id) = match result {
        Decision::Found {
            outgoing_vocals_off,
            incoming_vocals_on,
            mashup,
            evidence,
            tactic_id,
        } => (
            outgoing_vocals_off,
            incoming_vocals_on,
            mashup,
            evidence,
            tactic_id,
        ),
        Decision::NoGap { reason } => return Err(Error::new("STITCH_VOCAL_GAP", reason)),
    };
    require(
        off_seconds >= relative_bars[first_vocal_search_bar] + 0.05
            && on_seconds >= off_seconds + 0.29
            && on_seconds + 0.08 <= overlap_seconds,
        "STITCH_VOCAL_WINDOW",
        "separator returned an invalid vocal handoff",
    )?;
    let mut accompaniment_start = bars[drum_lead_bars];
    if let Some(decision) = mashup_decision {
        let fixed_two_bar_exit = decision.exit_policy == "six_plus_two";
        let valid_exit = if fixed_two_bar_exit {
            relative_bars.len() == 9
                && (decision.hold_until - relative_bars[6]).abs() < 1e-6
                && (decision.exit_end - relative_bars[8]).abs() < 1e-6
                && tactic_id.as_deref() == Some("M01")
                && evidence.is_object()
        } else {
            (decision.hold_until - off_seconds).abs() < 1e-6
                && decision.exit_end - decision.hold_until <= exit_seconds + 1e-6
        };
        require(
            mashup.is_some()
                && valid_exit
                && decision.exit_end > decision.hold_until
                && decision.exit_end <= overlap_seconds,
            "STITCH_MASHUP_WINDOW",
            "separator returned an invalid mashup hold or exit",
        )?;
        let metadata = plan.header.extensions.get_mut("mashup").unwrap();
        metadata["hold_end_output_frame"] =
            serde_json::json!(swap + (decision.hold_until * rate).round() as u64);
        metadata["exit_end_output_frame"] =
            serde_json::json!(swap + (decision.exit_end * rate).round() as u64);
        metadata["exit_policy"] = serde_json::json!(decision.exit_policy);
        metadata["exit_bars"] = serde_json::json!(if fixed_two_bar_exit { 2 } else { 1 });
        accompaniment_start = swap + (decision.hold_until * rate).round() as u64;
        plan.diagnostics.push(format!(
            "{first_vocal_search_bar}-bar mashup hold followed by a {}-bar layer exchange with independent vocal gates",
            if fixed_two_bar_exit { 2 } else { 1 }
        ));
    } else if mashup.is_some() {
        plan.objective = "quiet_drum_linger".into();
        plan.header.extensions.remove("mashup");
        drum_lead_bars = 2;
        accompaniment_start = bars[drum_lead_bars];
        plan.diagnostics.push(
            "vocal pocket leaves insufficient room for a brisk mashup exit; use the gradual handoff".into(),
        );
    }
    let off = swap + (off_seconds * rate).round() as u64;
    let on = swap + (on_seconds * rate).round() as u64;
    if !evidence.is_null() {
        plan.header
            .extensions
            .insert("stem_evidence".into(), evidence);
    }
    if let Some(tactic_id) = tactic_id {
        plan.header
            .extensions
            .insert("prepared_tactic_id".into(), serde_json::json!(tactic_id));
    }
    let outgoing = publish_drum_clip(store, plan, &prepared[0], bounds[0], true, cancel)?;
    let incoming = publish_drum_clip(store, plan, &prepared[1], bounds[1], false, cancel)?;
    plan.header.extensions.insert(
        "vocal_handoff".into(),
        serde_json::json!({
            "bar_frames": bars, "outgoing_vocals_off": off, "incoming_vocals_on": on,
            "accompaniment_start_frame": accompaniment_start
        }),
    );
    plan.diagnostics.push(format!(
        "estimated outgoing vocals end at {:.3}s output; incoming vocal gate opens at {:.3}s output",
        off as f64 / rate, on as f64 / rate
    ));
    Ok(DrumIntro {
        audio: incoming.audio,
        source_offset: incoming.source_offset,
        accompaniment_start_frame: accompaniment_start,
        outgoing: Some(outgoing),
    })
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
    emit!("segments", SegmentAnalysis);
    emit!("mixing-tactic", TacticPlan);
    emit!("tactic-analysis", TacticTrackAnalysis);
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
