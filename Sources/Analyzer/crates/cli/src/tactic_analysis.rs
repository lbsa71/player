//! Local full-track analysis, immutable stem publication and fused-record cache.
use super::*;
use mixlib_analysis::apply_overrides;
use mixlib_audio::WavReader;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
};

/// The CLI caller holds the same writer lock used by scan/analyze/stitch.
pub(super) fn ensure_tactic_analysis(
    store: &Store,
    track_ids: &[String],
    python: &Path,
    cancel: &Cancellation,
) -> Result<Report> {
    cancel.check()?;
    let manifest: ModelManifest = store.read("models/manifest.json")?;
    let mut models = BTreeMap::new();
    for wanted in ["stem_htdemucs", "vocal_phrase_whisper"] {
        let matching: Vec<_> = manifest
            .models
            .iter()
            .filter(|m| m.model_id == wanted)
            .collect();
        require(
            matching.len() == 1,
            "TACTIC_MODEL_MISSING",
            format!("install one pinned {wanted} model"),
        )?;
        let model = (*matching[0]).clone();
        let path = store.path(&model.file.path)?;
        require(
            file_hash(&path, cancel)? == model.file.hash,
            "MODEL_CHECKSUM",
            format!("{wanted} model changed"),
        )?;
        models.insert(wanted.to_string(), model);
    }
    let methods = script_sources();
    let implementation_hash = canonical_hash(
        &methods
            .iter()
            .map(|(name, text)| (*name, hash_bytes(text.as_bytes())))
            .collect::<BTreeMap<_, _>>(),
    )?;
    let targets: BTreeSet<_> = if track_ids.is_empty() {
        mixlib_planner::unique_song_pool(
            store,
            store
                .tracks()?
                .into_iter()
                .filter(|t| matches!(t.lifecycle, Lifecycle::Available))
                .collect(),
        )?
        .into_iter()
        .map(|t| t.track_id)
        .collect()
    } else {
        track_ids.iter().cloned().collect()
    };
    let mut report = Report::new("tactic_analysis");
    report
        .header
        .extensions
        .insert("target_songs".into(), serde_json::json!(targets.len()));
    let targets: Vec<_> = targets.into_iter().collect();
    // Each separator owns four CPU threads. Keep at most two independent jobs
    // on machines with eight logical CPUs; publication remains serialized.
    let workers = std::thread::available_parallelism().map_or(1, |n| (n.get() / 4).clamp(1, 2));
    for batch in targets.chunks(workers) {
        cancel.check()?;
        let results = std::thread::scope(|scope| {
            let jobs: Vec<_> = batch
                .iter()
                .map(|track_id| {
                    let models = &models;
                    let implementation_hash = &implementation_hash;
                    let methods = &methods;
                    scope.spawn(move || {
                        eprintln!("tactic analysis: inspecting {track_id}");
                        (
                            track_id,
                            analyze_one(
                                store,
                                track_id,
                                python,
                                models,
                                implementation_hash,
                                methods,
                                cancel,
                            ),
                        )
                    })
                })
                .collect();
            jobs.into_iter()
                .map(|j| {
                    j.join()
                        .map_err(|_| Error::new("TACTIC_WORKER", "analysis worker failed"))
                })
                .collect::<Result<Vec<_>>>()
        })?;
        for (track_id, result) in results {
            match result {
                Ok(cached) => report.count(if cached { "cached" } else { "analyzed" }),
                Err(error) if error.code == "CANCELLED" => {
                    report
                        .header
                        .extensions
                        .insert("status".into(), serde_json::json!("cancelled"));
                    store.report(&report)?;
                    return Err(error);
                }
                Err(error) => {
                    eprintln!("tactic analysis: failed {track_id}: {error}");
                    report.count("failed");
                    report.error(track_id, &error);
                }
            }
        }
        report
            .header
            .extensions
            .insert("status".into(), serde_json::json!("running"));
        store.report(&report)?;
        eprintln!(
            "tactic analysis: {} prepared/cached, {} failed, {} targets",
            report.counts.get("cached").unwrap_or(&0) + report.counts.get("analyzed").unwrap_or(&0),
            report.counts.get("failed").unwrap_or(&0),
            targets.len()
        );
    }
    report.header.extensions.insert(
        "status".into(),
        serde_json::json!(if report.counts.get("failed").copied().unwrap_or(0) == 0 {
            "complete"
        } else {
            "incomplete"
        }),
    );
    store.report(&report)?;
    Ok(report)
}

/// Shortlist using basic measurements, then publish through the normal fused
/// sidecar analysis path. Failed candidates do not count toward pool expansion.
pub(super) fn expand_pool(
    store: &Store,
    count: usize,
    python: &Path,
    cancel: &Cancellation,
) -> Result<serde_json::Value> {
    let before = measured_beam::prepared_pool(store)?.len();
    let policy: MixingConfig = store.read("config/mixing.json")?;
    eprintln!(
        "tactic analysis: ranking new tracks by two-sided connections to {before} prepared tracks"
    );
    let snapshot = store.read_snapshot();
    let shortlist = mixlib_planner::shortlist_preparation(&snapshot, &policy, 12, 8, cancel)?;
    snapshot.verify_read_snapshot(cancel)?;
    let analyzed_recordings = store
        .tracks()?
        .iter()
        .filter(|t| t.active_analysis.is_some() && matches!(t.lifecycle, Lifecycle::Available))
        .count();
    let mut report = Report::new("tactic_pool_expansion");
    report.header.extensions.insert("selection".into(), serde_json::json!({
        "requested":count,"added":0,"prepared_before":before,"prepared_after":before,
        "basic_analyzed_tracks":shortlist.analyzed_tracks,"basic_analyzed_recordings":analyzed_recordings,
        "identity_policy":"distinct recorder trackID when known, plus distinct audio hashes; prepared captures preferred",
        "shortlist":shortlist.preparation_candidates,
        "attempts":[],"complete":false,"status":"running"
    }));
    store.report(&report)?;
    let mut prepared = measured_beam::prepared_pool(store)?;
    for candidate in &shortlist.preparation_candidates {
        if prepared.len() >= before + count {
            break;
        }
        cancel.check()?;
        let id = candidate["track_id"].as_str().unwrap();
        let track = store.track(id)?;
        if prepared.iter().any(|p| p.audio_hash == track.audio_hash) {
            continue;
        }
        let source = store.source(&track)?;
        eprintln!("tactic analysis: pool candidate {}", source.display());
        let attempt = serde_json::json!({"candidate":candidate,"source":source,"status":"running"});
        report.header.extensions.get_mut("selection").unwrap()["attempts"]
            .as_array_mut()
            .unwrap()
            .push(attempt);
        store.report(&report)?;
        let analyzed = match ensure_tactic_analysis(store, &[id.to_owned()], python, cancel) {
            Ok(report) => report,
            Err(error) => {
                let status = if error.code == "CANCELLED" {
                    "cancelled"
                } else {
                    "failed"
                };
                let selection = report.header.extensions.get_mut("selection").unwrap();
                selection["status"] = serde_json::json!(status);
                let attempt = selection["attempts"]
                    .as_array_mut()
                    .unwrap()
                    .last_mut()
                    .unwrap();
                attempt["status"] = serde_json::json!(status);
                attempt["error"] = serde_json::json!({"code":error.code,"message":error.message});
                store.report(&report)?;
                return Err(error);
            }
        };
        prepared = measured_beam::prepared_pool(store)?;
        let succeeded = prepared.iter().any(|p| p.audio_hash == track.audio_hash);
        let selection = report.header.extensions.get_mut("selection").unwrap();
        let attempt = selection["attempts"]
            .as_array_mut()
            .unwrap()
            .last_mut()
            .unwrap();
        attempt["status"] = serde_json::json!(if succeeded { "published" } else { "failed" });
        attempt["report"] = serde_json::to_value(analyzed)?;
        selection["added"] = serde_json::json!(prepared.len().saturating_sub(before));
        selection["prepared_after"] = serde_json::json!(prepared.len());
        store.report(&report)?;
    }
    let selection = report.header.extensions.get_mut("selection").unwrap();
    selection["complete"] = serde_json::json!(prepared.len() >= before + count);
    selection["status"] = serde_json::json!(if prepared.len() >= before + count {
        "complete"
    } else {
        "shortlist_exhausted"
    });
    store.report(&report)?;
    Ok(serde_json::to_value(report)?)
}

fn script_sources() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        (
            "analyze-tactics.py",
            include_str!("../../../scripts/analyze-tactics.py"),
        ),
        (
            "prepare-drum-intro.py",
            include_str!("../../../scripts/prepare-drum-intro.py"),
        ),
        (
            "vocal-phrase-evidence.py",
            include_str!("../../../scripts/vocal-phrase-evidence.py"),
        ),
        (
            "tactic-signal-evidence.py",
            include_str!("../../../scripts/tactic-signal-evidence.py"),
        ),
    ])
}

fn component_hashes(methods: &BTreeMap<&str, &str>) -> BTreeMap<String, ContentHash> {
    let mut values: BTreeMap<_, _> = methods
        .iter()
        .map(|(name, source)| (name.to_string(), hash_bytes(source.as_bytes())))
        .collect();
    // Bump these when orchestration changes the meaning or coordinates of the
    // corresponding expensive component, independently of signal descriptors.
    values.insert(
        "stem_pipeline".into(),
        hash_bytes(b"source-aligned-stereo/1"),
    );
    values.insert(
        "vocal_pipeline".into(),
        hash_bytes(b"source-seconds-full-track/1"),
    );
    values
}

fn previous_components(record: &TacticTrackAnalysis) -> BTreeMap<String, ContentHash> {
    if !record.component_hashes.is_empty() {
        return record.component_hashes.clone();
    }
    // Exact first-release digest, reconstructed from that binary's four embedded
    // scripts and checked against its persisted record. No heuristic migration.
    if record.implementation_hash.digest
        != "79fa10c921dc42fac3ed646c45dd28d077dc2eddcb40502654ed77ffa8040caa"
    {
        return BTreeMap::new();
    }
    let mut values: BTreeMap<_, _> = [
        (
            "analyze-tactics.py",
            "400a21ab39ae2053397b3e30dac9a9673853e0e2156307c6778668503a001821",
        ),
        (
            "prepare-drum-intro.py",
            "717daee6941bd058b59d7169152ed3ccb539d16986edaf6459e49cd02252161d",
        ),
        (
            "vocal-phrase-evidence.py",
            "b5ce678e3b11fb5e9007ce2a20cd0a6f2496614a3926608993cf450db8d8ca59",
        ),
        (
            "tactic-signal-evidence.py",
            "945a82f08b442d176a603d569021c9e737932c49cd41f646770178eb873a3370",
        ),
    ]
    .into_iter()
    .map(|(name, digest)| {
        (
            name.into(),
            ContentHash {
                algorithm: HashAlgorithm::Sha256,
                digest: digest.into(),
            },
        )
    })
    .collect();
    values.insert(
        "stem_pipeline".into(),
        hash_bytes(b"source-aligned-stereo/1"),
    );
    values.insert(
        "vocal_pipeline".into(),
        hash_bytes(b"source-seconds-full-track/1"),
    );
    values
}

fn reusable_components(
    cached: &TacticTrackAnalysis,
    components: &BTreeMap<String, ContentHash>,
    models: &BTreeMap<String, ModelAsset>,
) -> (bool, bool) {
    let previous = previous_components(cached);
    reusable_hashes(&previous, components, &cached.models, models)
}

fn reusable_hashes(
    previous: &BTreeMap<String, ContentHash>,
    components: &BTreeMap<String, ContentHash>,
    previous_models: &BTreeMap<String, ModelAsset>,
    models: &BTreeMap<String, ModelAsset>,
) -> (bool, bool) {
    let same = |key: &str| {
        previous
            .get(key)
            .is_some_and(|h| components.get(key) == Some(h))
    };
    let model_same = |key: &str| {
        previous_models
            .get(key)
            .zip(models.get(key))
            .is_some_and(|(a, b)| a.file.hash == b.file.hash)
    };
    let stems =
        same("prepare-drum-intro.py") && same("stem_pipeline") && model_same("stem_htdemucs");
    let voice = stems
        && same("vocal-phrase-evidence.py")
        && same("vocal_pipeline")
        && model_same("vocal_phrase_whisper");
    (stems, voice)
}

fn read_staged_result(path: &Path) -> Result<Vec<u8>> {
    // Staging JSON includes duplicated provider/signal data before compaction.
    // Persisted logical records remain subject to the ordinary MAX_JSON bound.
    const MAX_STAGED: u64 = 64 * 1024 * 1024;
    let file = File::open(path)?;
    require(
        file.metadata()?.len() <= MAX_STAGED,
        "TACTIC_RESULT_TOO_LARGE",
        path.display().to_string(),
    )?;
    let mut bytes = Vec::new();
    file.take(MAX_STAGED + 1).read_to_end(&mut bytes)?;
    require(
        bytes.len() as u64 <= MAX_STAGED,
        "TACTIC_RESULT_TOO_LARGE",
        "staged result grew while reading",
    )?;
    Ok(bytes)
}

fn file_hash(path: &Path, cancel: &Cancellation) -> Result<ContentHash> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        cancel.check()?;
        let length = file.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        hash.update(&buffer[..length]);
    }
    Ok(ContentHash {
        algorithm: HashAlgorithm::Sha256,
        digest: format!("{:x}", hash.finalize()),
    })
}

fn cache_key(
    track: &Track,
    beats: &FileRef,
    models: &BTreeMap<String, ModelAsset>,
    implementation: &ContentHash,
) -> Result<ContentHash> {
    canonical_hash(&serde_json::json!({
        "method":TACTIC_ANALYSIS_METHOD,"implementation":implementation,
        "audio_hash":track.audio_hash,"audio_revision":track.audio_revision,
        "format":track.format,"beats":beats.hash,"override_revision":track.override_revision,
        "models":models.iter().map(|(id,m)|(id,&m.file.hash)).collect::<BTreeMap<_,_>>(),
        "demucs_version":"4.0.1","whisper_version":"20250625",
    }))
}

fn verify_stems(
    store: &Store,
    analysis: &TacticTrackAnalysis,
    cancel: &Cancellation,
) -> Result<()> {
    for asset in analysis.stems.values() {
        cancel.check()?;
        let mut wav = WavReader::open(&store.path(&asset.path)?)?;
        require(
            wav.format.sample_rate_hz == analysis.source_sample_rate_hz
                && wav.format.frames == analysis.source_frames
                && wav.format.channels == 2,
            "TACTIC_STEM_FORMAT",
            "cached full-track stem changed its source timebase",
        )?;
        require(
            wav.hashes(cancel)?.0 == asset.hash,
            "TACTIC_STEM_CHECKSUM",
            "cached stem changed",
        )?;
    }
    Ok(())
}

fn analyze_one(
    store: &Store,
    track_id: &str,
    python: &Path,
    models: &BTreeMap<String, ModelAsset>,
    implementation: &ContentHash,
    methods: &BTreeMap<&str, &str>,
    cancel: &Cancellation,
) -> Result<bool> {
    let mut track = store.track(track_id)?;
    let source = store.source(&track)?;
    let base_ref = track
        .active_analysis
        .clone()
        .ok_or_else(|| Error::new("ANALYSIS_MISSING", track_id))?;
    let mut base: AnalysisManifest = store.read_ref(&base_ref)?;
    require(
        base.audio_hash == track.audio_hash,
        "ANALYSIS_STALE",
        track_id,
    )?;
    validate_analysis(store, &base)?;
    let beat_ref = base
        .artifacts
        .get("beats")
        .ok_or_else(|| Error::new("ANALYSIS_ARTIFACT", "missing beats"))?
        .clone();
    let key = cache_key(&track, &beat_ref, models, implementation)?;
    let mut reader = WavReader::open(&source)?;
    let (source_file_hash, audio_hash) = reader.hashes(cancel)?;
    require(
        audio_hash == track.audio_hash,
        "SOURCE_CHANGED",
        "source changed before full-track tactic analysis",
    )?;
    let components = component_hashes(methods);
    let cached = base
        .artifacts
        .get("tactic_analysis")
        .and_then(|reference| store.read_ref::<TacticTrackAnalysis>(reference).ok())
        .filter(|cached| {
            cached.audio_hash == track.audio_hash
                && cached.audio_revision == track.audio_revision
                && cached.track_id == track.track_id
                && cached.source_sample_rate_hz == track.format.sample_rate_hz
                && cached.source_frames == track.format.frames
                && cached.beat_artifact.hash == beat_ref.hash
                && cached.override_revision == track.override_revision
        });
    let reusable = cached
        .as_ref()
        .map(|c| reusable_components(c, &components, models))
        .unwrap_or((false, false));
    let reusable = if reusable.0 {
        match verify_stems(store, cached.as_ref().unwrap(), cancel) {
            Ok(()) => reusable,
            Err(error) if error.code == "CANCELLED" => return Err(error),
            Err(_) => (false, false),
        }
    } else {
        reusable
    };
    if cached.as_ref().is_some_and(|c| c.cache_key == key) && reusable.0 && reusable.1 {
        return Ok(true);
    }
    cancel.check()?;
    let mut beats: Beats = store.read_ref(&beat_ref)?;
    let mut regions: Regions = store.read_ref(&base.artifacts["regions"])?;
    let overrides = store.overrides(&track)?;
    apply_overrides(&track, &base_ref, &mut beats, &mut regions, &overrides)?;
    let generation_key = canonical_hash(&serde_json::json!({
        "audio":track.audio_hash,"revision":track.audio_revision,"format":track.format,
        "beats":beat_ref.hash,"overrides":track.override_revision,
        "separator":components["prepare-drum-intro.py"],"pipeline":components["stem_pipeline"],
        "model":models["stem_htdemucs"].file.hash,
    }))?;
    // Recovery files are temporary, never public metadata. Retaining this bounded
    // per-generation directory on failure avoids repeating verified model work.
    let temporary = store.directory(&format!(
        "stems/staging/{track_id}/{}",
        generation_key.digest
    ))?;
    for (name, text) in methods {
        std::fs::write(temporary.as_path().join(name), text)?;
    }
    let model_config = |id: &str| -> Result<serde_json::Value> {
        let model = &models[id];
        Ok(
            serde_json::json!({"model_path":store.path(&model.file.path)?,"model_sha256":model.file.hash.digest,
                             "signature":"955717e8"}),
        )
    };
    let stems: BTreeMap<_, _> = ["drums", "bass", "other", "vocals"]
        .into_iter()
        .map(|lane| (lane, temporary.as_path().join(format!("{lane}.wav"))))
        .collect();
    let downbeat_ids: BTreeSet<_> = beats.downbeat_ids.iter().collect();
    let result = temporary.as_path().join("result.json");
    let mut request = serde_json::json!({
        "source":source,"source_file_sha256":source_file_hash.digest,
        "sample_rate_hz":track.format.sample_rate_hz,"source_frames":track.format.frames,
        "beat_frames":beats.events.iter().map(|b|b.source_frame).collect::<Vec<_>>(),
        "bar_frames":beats.events.iter().filter(|b|downbeat_ids.contains(&b.event_id)).map(|b|b.source_frame).collect::<Vec<_>>(),
        "separator_script":temporary.as_path().join("prepare-drum-intro.py"),
        "signal_script":temporary.as_path().join("tactic-signal-evidence.py"),
        "vocal_provider_script":temporary.as_path().join("vocal-phrase-evidence.py"),
        "separator_model":model_config("stem_htdemucs")?,"vocal_model":model_config("vocal_phrase_whisper")?,
        "stems":stems,"result":result,
        "checkpoint":temporary.join("inputs.checkpoint.json"),"generation_key":generation_key,
    });
    if reusable.0 {
        let previous = cached.as_ref().unwrap();
        let assets: BTreeMap<_, _> = previous
            .stems
            .iter()
            .map(|(lane, asset)| {
                Ok((
                    lane,
                    serde_json::json!({"path":store.path(&asset.path)?,"sha256":asset.hash.digest}),
                ))
            })
            .collect::<Result<_>>()?;
        request["reuse"] = serde_json::json!({"stems":assets});
        if reusable.1 {
            request["reuse"]["vocal_evidence"] = previous.vocal_evidence.clone();
        }
    }
    let request_path = temporary.as_path().join("request.json");
    std::fs::write(&request_path, serde_json::to_vec(&request)?)?;
    run_separator(
        std::process::Command::new(python)
            .arg(temporary.as_path().join("analyze-tactics.py"))
            .arg(&request_path),
        cancel,
    )?;
    #[derive(serde::Deserialize)]
    struct Prepared {
        method: String,
        stem_assets: BTreeMap<String, PreparedStem>,
        sample_rate_hz: u32,
        source_frames: u64,
        source_file_sha256: String,
        vocal_evidence: serde_json::Value,
        signal_evidence: serde_json::Value,
        limitations: Vec<String>,
    }
    #[derive(serde::Deserialize)]
    struct PreparedStem {
        path: PathBuf,
        sha256: String,
    }
    let prepared: Prepared = serde_json::from_slice(&read_staged_result(&result)?)?;
    require(
        prepared.method == TACTIC_ANALYSIS_METHOD
            && prepared.sample_rate_hz == track.format.sample_rate_hz
            && prepared.source_frames == track.format.frames
            && prepared.source_file_sha256 == source_file_hash.digest,
        "TACTIC_ANALYSIS_RESULT",
        "prepared analysis changed the requested source identity",
    )?;
    require(
        file_hash(&source, cancel)? == source_file_hash,
        "SOURCE_CHANGED",
        "source changed during full-track analysis",
    )?;
    let latest = store.track(track_id)?;
    require(
        latest.audio_hash == track.audio_hash
            && latest.audio_revision == track.audio_revision
            && latest.override_revision == track.override_revision
            && latest
                .active_analysis
                .as_ref()
                .is_some_and(|r| r.hash == base_ref.hash),
        "ANALYSIS_CHANGED",
        "track metadata changed before tactic publication",
    )?;
    let asset_prefix = format!("stems/tracks/{track_id}/{}", key.digest);
    let destination = store.directory(&asset_prefix)?;
    let mut assets = if reusable.0 {
        cached.as_ref().unwrap().stems.clone()
    } else {
        BTreeMap::new()
    };
    for (name, lane) in [
        ("drums", StemLane::Drums),
        ("bass", StemLane::Bass),
        ("other", StemLane::Other),
        ("vocals", StemLane::Vocals),
    ] {
        cancel.check()?;
        let prepared_stem = prepared
            .stem_assets
            .get(name)
            .ok_or_else(|| Error::new("TACTIC_STEM_MISSING", name))?;
        let staged = &prepared_stem.path;
        let expected = if reusable.0 {
            store.path(&assets[&lane].path)?
        } else {
            stems[name].clone()
        };
        require(
            staged == &expected,
            "TACTIC_STEM_PATH",
            "prepared stem escaped its requested staging or immutable path",
        )?;
        let mut wav = WavReader::open(staged)?;
        require(
            wav.format.sample_rate_hz == track.format.sample_rate_hz
                && wav.format.frames == track.format.frames
                && wav.format.channels == 2,
            "TACTIC_STEM_FORMAT",
            "prepared stem must retain source frame zero, rate and full length",
        )?;
        let hash = wav.hashes(cancel)?.0;
        require(
            hash.digest == prepared_stem.sha256,
            "TACTIC_STEM_CHECKSUM",
            "staged stem checksum differs from measured samples",
        )?;
        if reusable.0 {
            require(
                hash == assets[&lane].hash,
                "TACTIC_STEM_CHECKSUM",
                "reused stem changed during signal measurement",
            )?;
            continue;
        }
        File::open(staged)?.sync_all()?;
        let target = destination.join(format!("{name}.wav"));
        // Keep the staging link until metadata commits. A downstream record-size
        // or publication error can then resume without discarding model work.
        if target.exists() {
            require(
                !std::fs::symlink_metadata(&target)?.file_type().is_symlink()
                    && file_hash(&target, cancel)? == hash,
                "TACTIC_ASSET_COLLISION",
                "immutable stem destination differs from verified staged audio",
            )?;
        } else {
            std::fs::hard_link(staged, &target)?;
        }
        assets.insert(
            lane,
            FileRef {
                path: format!("{asset_prefix}/{name}.wav"),
                hash,
            },
        );
    }
    File::open(&destination)?.sync_all()?;
    let record = TacticTrackAnalysis {
        header: Header::default(),
        tactic_analysis_id: id(),
        track_id: track_id.into(),
        audio_hash: track.audio_hash.clone(),
        audio_revision: track.audio_revision,
        source_sample_rate_hz: track.format.sample_rate_hz,
        source_frames: track.format.frames,
        base_analysis: cached
            .as_ref()
            .map(|c| c.base_analysis.clone())
            .unwrap_or_else(|| base_ref.clone()),
        beat_artifact: beat_ref,
        override_revision: track.override_revision,
        method: TACTIC_ANALYSIS_METHOD.into(),
        implementation_hash: implementation.clone(),
        component_hashes: components,
        cache_key: key.clone(),
        models: models.clone(),
        stems: assets,
        vocal_evidence: prepared.vocal_evidence,
        signal_evidence: prepared.signal_evidence,
        limitations: prepared.limitations,
        created_at: now(),
    };
    record.validate()?;
    let generation = id();
    let prefix = format!(
        "tracks/{}/{track_id}/analysis/{generation}",
        shard(track_id)
    );
    // Fused sidecars in one source directory share an advisory directory lock.
    // Keep all publication writes for a generation together across workers.
    static PUBLICATION: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _publication = PUBLICATION
        .lock()
        .map_err(|_| Error::new("TACTIC_PUBLICATION", "publication lock poisoned"))?;
    let reference = store.write(&format!("{prefix}/tactic-analysis.json"), &record)?;
    base.analysis_id = generation;
    base.artifacts
        .insert("tactic_analysis".into(), reference.clone());
    base.stages.insert(
        "tactic_analysis".into(),
        Stage {
            implementation: TACTIC_ANALYSIS_METHOD.into(),
            input_hash: key,
            artifacts: BTreeMap::from([("tactic_analysis".into(), reference)]),
            dependencies: vec![
                "beats".into(),
                "corrections".into(),
                "stem_htdemucs".into(),
                "vocal_phrase_whisper".into(),
            ],
        },
    );
    base.created_at = now();
    track.active_analysis = Some(store.write(&format!("{prefix}/manifest.json"), &base)?);
    store.save_track(&track)?;
    std::fs::remove_dir_all(temporary)?;
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn models() -> BTreeMap<String, ModelAsset> {
        ["stem_htdemucs", "vocal_phrase_whisper"]
            .into_iter()
            .map(|name| {
                (
                    name.into(),
                    ModelAsset {
                        model_id: name.into(),
                        file: FileRef {
                            path: format!("models/{name}"),
                            hash: hash_bytes(name.as_bytes()),
                        },
                        upstream_url: "fixture://local".into(),
                        revision: "fixture".into(),
                        code_license: "fixture".into(),
                        weight_license: "fixture".into(),
                        attribution: "fixture".into(),
                        redistribution: "fixture".into(),
                    },
                )
            })
            .collect()
    }
    #[test]
    fn expensive_components_have_independent_dependencies() {
        let previous = component_hashes(&script_sources());
        let old_models = models();
        let mut current = previous.clone();
        current.insert(
            "tactic-signal-evidence.py".into(),
            hash_bytes(b"changed signal measurement"),
        );
        assert_eq!(
            reusable_hashes(&previous, &current, &old_models, &old_models),
            (true, true)
        );
        current.insert(
            "vocal-phrase-evidence.py".into(),
            hash_bytes(b"changed local absence support"),
        );
        assert_eq!(
            reusable_hashes(&previous, &current, &old_models, &old_models),
            (true, false)
        );
        current.insert(
            "prepare-drum-intro.py".into(),
            hash_bytes(b"changed separation"),
        );
        assert_eq!(
            reusable_hashes(&previous, &current, &old_models, &old_models),
            (false, false)
        );
    }
    #[test]
    fn pre_dc_restoration_assets_are_not_reused_as_new_separator_output() {
        let current = component_hashes(&script_sources());
        let mut previous = current.clone();
        previous.insert(
            "prepare-drum-intro.py".into(),
            ContentHash {
                algorithm: HashAlgorithm::Sha256,
                digest: "717daee6941bd058b59d7169152ed3ccb539d16986edaf6459e49cd02252161d".into(),
            },
        );
        assert_ne!(
            previous["prepare-drum-intro.py"],
            current["prepare-drum-intro.py"]
        );
        let pinned = models();
        assert_eq!(
            reusable_hashes(&previous, &current, &pinned, &pinned),
            (false, false)
        );
        // The old generation stays immutable/readable; only new-analysis reuse
        // is refused because its stem and dependent vocal inputs differ.
    }

    #[test]
    fn model_or_unknown_provenance_never_reuses_incompatible_components() {
        let components = component_hashes(&script_sources());
        let previous = models();
        let mut current = previous.clone();
        current.get_mut("vocal_phrase_whisper").unwrap().file.hash = hash_bytes(b"new whisper");
        assert_eq!(
            reusable_hashes(&components, &components, &previous, &current),
            (true, false)
        );
        current.get_mut("stem_htdemucs").unwrap().file.hash = hash_bytes(b"new separator");
        assert_eq!(
            reusable_hashes(&components, &components, &previous, &current),
            (false, false)
        );
        assert_eq!(
            reusable_hashes(&BTreeMap::new(), &components, &previous, &previous),
            (false, false)
        );
    }
}
