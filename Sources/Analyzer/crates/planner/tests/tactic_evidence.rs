#[path = "../../../tests/support.rs"]
mod support;
use mixlib_catalog::*;
use mixlib_planner::*;
use mixlib_types::*;
use std::fs;

fn fixture() -> (tempfile::TempDir, Store, MixPlan) {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::init_legacy(&temp.path().join("library")).unwrap();
    let music = temp.path().join("music");
    fs::create_dir(&music).unwrap();
    for (name, frequency) in [("a.wav", 220.), ("b.wav", 330.)] {
        support::wav(
            &music.join(name),
            48000,
            2,
            32,
            true,
            false,
            16 * 48000,
            b"CC0",
            |i, _| 0.08 * (std::f64::consts::TAU * frequency * i as f64 / 48000.).sin(),
        );
    }
    let mut config: AnalysisConfig = store.read("config/analysis.json").unwrap();
    config.settling_ms = 0;
    store.write("config/analysis.json", &config).unwrap();
    scan(&store, &[music], false, &Cancellation::default()).unwrap();
    mixlib_analysis::analyze(&store, true, true, &Cancellation::default()).unwrap();
    let mut tracks = store.tracks().unwrap();
    for track in &mut tracks {
        let mut analysis = store.analysis(track).unwrap();
        let beats = Beats {
            header: Header::default(),
            beats_id: id(),
            source_sample_rate_hz: 48000,
            source_frames: track.format.frames,
            events: (0..=30)
                .map(|i| Beat {
                    event_id: format!("b{i}"),
                    source_frame: i * 24000,
                    score: 0.95,
                    local_bpm: Some(120.),
                })
                .collect(),
            downbeat_ids: (0..=30).step_by(3).map(|i| format!("b{i}")).collect(),
            meter_segments: vec![Meter {
                start_frame: 0,
                end_frame: track.format.frames,
                pulses_per_bar: 3,
                pulse_unit: "annotated fixture pulse".into(),
            }],
            unreliable_intervals: vec![],
            score_semantics: "fixture".into(),
        };
        let prefix = format!(
            "tracks/{}/{}/analysis/{}",
            shard(&track.track_id),
            track.track_id,
            id()
        );
        analysis.artifacts.insert(
            "beats".into(),
            store
                .write(&format!("{prefix}/beats.json"), &beats)
                .unwrap(),
        );
        // Exercise the on-demand path for older generations without segments.
        analysis.artifacts.remove("segments");
        analysis.stages.clear();
        analysis.analysis_id = id();
        track.active_analysis = Some(
            store
                .write(&format!("{prefix}/manifest.json"), &analysis)
                .unwrap(),
        );
        store.save_track(track).unwrap();
    }
    let plan = stitch(
        &store,
        &StitchRequest {
            from: &tracks[0].track_id,
            to: &tracks[1].track_id,
            overlap_beats: 12,
            context_beats: 3,
            outgoing_after_frame: 0,
        },
        &MixingConfig::default(),
        &Cancellation::default(),
    )
    .unwrap();
    (temp, store, plan)
}

#[test]
fn actual_saved_analysis_drives_three_beat_bars_without_metadata_writes() {
    let (_temp, store, plan) = fixture();
    let before = store
        .read_ref::<AnalysisManifest>(&plan.placements[0].analysis)
        .unwrap();
    let facts = candidate_facts(&store, &plan, 0, &Cancellation::default()).unwrap();
    assert_eq!(facts.bar_frames.len(), 5);
    assert!(facts
        .a
        .bars
        .iter()
        .chain(&facts.b.bars)
        .all(|b| b.beats_per_bar == Some(3)));
    assert!(facts.a.vocal_phrases.is_none() && facts.b.vocal_phrases.is_none());
    assert!(facts.a.stems.is_none() && facts.a.stem_assets.is_empty());
    assert!(facts.pair.lead_conflict.is_none() && facts.pair.pulse_ratio.is_none());
    assert!(facts.pair.selected_composite_density.is_none());
    assert!(facts.pair.beat_skew_ms.unwrap() < 0.01);
    assert!(facts.pair.harmonic_distance.unwrap() > 0.0);
    let after = store
        .read_ref::<AnalysisManifest>(&plan.placements[0].analysis)
        .unwrap();
    assert_eq!(
        canonical_hash(&before).unwrap(),
        canonical_hash(&after).unwrap()
    );
    assert!(!after.artifacts.contains_key("segments"));

    // A different current analysis pointer cannot replace the plan's generation.
    let mut track = store.track(&plan.placements[0].track_id).unwrap();
    let mut current = before;
    let mut beats: Beats = store.read_ref(&current.artifacts["beats"]).unwrap();
    beats.unreliable_intervals.push(Interval {
        start_frame: 0,
        end_frame: track.format.frames,
        reason: "new unreliable generation".into(),
    });
    let prefix = format!(
        "tracks/{}/{}/analysis/{}",
        shard(&track.track_id),
        track.track_id,
        id()
    );
    current.artifacts.insert(
        "beats".into(),
        store
            .write(&format!("{prefix}/beats.json"), &beats)
            .unwrap(),
    );
    current.analysis_id = id();
    track.active_analysis = Some(
        store
            .write(&format!("{prefix}/manifest.json"), &current)
            .unwrap(),
    );
    store.save_track(&track).unwrap();
    let still_saved = candidate_facts(&store, &plan, 0, &Cancellation::default()).unwrap();
    assert_eq!(still_saved.bar_frames, facts.bar_frames);
}

#[test]
fn changed_corrections_are_rejected_and_applied_when_revision_matches() {
    let (_temp, store, mut plan) = fixture();
    let track = store.track(&plan.placements[0].track_id).unwrap();
    let mut corrections = store.overrides(&track).unwrap();
    corrections.operations.push(Correction::Meter {
        start_frame: 0,
        end_frame: track.format.frames,
        pulses_per_bar: 4,
    });
    store.set_overrides(&track.track_id, corrections).unwrap();
    assert_eq!(
        candidate_facts(&store, &plan, 0, &Cancellation::default())
            .unwrap_err()
            .code,
        "OVERRIDE_CHANGED"
    );
    plan.placements[0].override_revision = 1;
    let structural = candidate_facts(&store, &plan, 0, &Cancellation::default()).unwrap();
    assert_eq!(structural.timing_basis, TacticTimingBasis::Structural);
    assert!(structural.a.bars.is_empty() && structural.b.bars.is_empty());
    assert_eq!(
        structural.bar_frames,
        vec![
            plan.transitions[0].start_output_frame,
            plan.transitions[0].end_output_frame
        ]
    );
    assert!(structural.pair.beat_skew_ms.is_none());
    assert!(!structural.pair.tempo_maps_validated);
    assert!(structural
        .a
        .evidence
        .iter()
        .any(|e| e.contains("no inferred bars")));
    assert!(structural.a.vocal_phrases.is_none());
    assert!(structural.pair.predicted_peak_dbfs.is_some());
}

#[test]
fn cancellation_is_observed_before_source_reads() {
    let (_temp, store, plan) = fixture();
    let cancel = Cancellation::default();
    cancel.cancel();
    assert_eq!(
        candidate_facts(&store, &plan, 0, &cancel).unwrap_err().code,
        "CANCELLED"
    );
}

fn publish_supplement(
    store: &Store,
    p: &Placement,
    provider: serde_json::Value,
    amplitude: f64,
    beat_matches: bool,
) {
    let mut track = store.track(&p.track_id).unwrap();
    let mut manifest: AnalysisManifest = store.read_ref(&p.analysis).unwrap();
    let folder = store
        .directory(&format!("stems/test/{}", track.track_id))
        .unwrap();
    let path = folder.join("vocals.wav");
    support::wav(
        &path,
        48000,
        2,
        32,
        true,
        false,
        track.format.frames as usize,
        b"CC0",
        |i, _| amplitude * (std::f64::consts::TAU * 220. * i as f64 / 48000.).sin(),
    );
    let mut reader = mixlib_audio::WavReader::open(&path).unwrap();
    let stem = FileRef {
        path: format!("stems/test/{}/vocals.wav", track.track_id),
        hash: reader.hashes(&Cancellation::default()).unwrap().0,
    };
    let mut beats = manifest.artifacts["beats"].clone();
    if !beat_matches {
        beats.hash = hash_bytes(b"other beat generation");
    }
    let models = ["stem_htdemucs", "vocal_phrase_whisper"]
        .into_iter()
        .map(|key| {
            (
                key.into(),
                ModelAsset {
                    model_id: key.into(),
                    file: stem.clone(),
                    upstream_url: "fixture://local".into(),
                    revision: "fixture".into(),
                    code_license: "CC0".into(),
                    weight_license: "CC0".into(),
                    attribution: "test".into(),
                    redistribution: "test".into(),
                },
            )
        })
        .collect();
    let analysis = TacticTrackAnalysis {
        header: Header::default(),
        tactic_analysis_id: id(),
        track_id: track.track_id.clone(),
        audio_hash: track.audio_hash.clone(),
        audio_revision: track.audio_revision,
        source_sample_rate_hz: 48000,
        source_frames: track.format.frames,
        base_analysis: p.analysis.clone(),
        beat_artifact: beats,
        override_revision: track.override_revision,
        method: TACTIC_ANALYSIS_METHOD.into(),
        implementation_hash: hash_bytes(b"fixture"),
        component_hashes: Default::default(),
        cache_key: hash_bytes(b"fixture"),
        models,
        stems: [
            StemLane::Drums,
            StemLane::Bass,
            StemLane::Other,
            StemLane::Vocals,
        ]
        .into_iter()
        .map(|l| (l, stem.clone()))
        .collect(),
        vocal_evidence: provider,
        signal_evidence: serde_json::json!({"method":"fixture/1","sample_rate_hz":48000,"source_start_frame":0,"source_end_frame":track.format.frames,"bars":[]}),
        limitations: vec!["fixture evidence".into()],
        created_at: now(),
    };
    let generation = id();
    let prefix = format!(
        "tracks/{}/{}/analysis/{generation}",
        shard(&track.track_id),
        track.track_id
    );
    manifest.artifacts.insert(
        "tactic_analysis".into(),
        store
            .write(&format!("{prefix}/tactic-analysis.json"), &analysis)
            .unwrap(),
    );
    manifest.analysis_id = generation;
    track.active_analysis = Some(
        store
            .write(&format!("{prefix}/manifest.json"), &manifest)
            .unwrap(),
    );
    store.save_track(&track).unwrap();
}

fn absence_provider() -> serde_json::Value {
    serde_json::json!({"method":"local_whisper_fixture/2","reference":"fixture:pinned","coordinate_system":"source_seconds",
    "track":{"instrumental":false,"phrases":[],"words":[],"verified_absence_spans":[
        {"start_seconds":0.,"end_seconds":16.,"verification_method":"separated_vocal_rms_and_peak_negligible/1","reference":"fixture:pinned"}
    ]}})
}

#[test]
fn supplementary_local_absence_requires_affirmative_spans_and_actual_quiet_pcm() {
    let (_temp, store, plan) = fixture();
    let mut blank = absence_provider();
    blank["track"]["verified_absence_spans"] = serde_json::json!([]);
    publish_supplement(&store, &plan.placements[0], blank, 0.00001, true);
    assert!(candidate_facts(&store, &plan, 0, &Cancellation::default())
        .unwrap()
        .a
        .vocal_phrases
        .is_none());
    publish_supplement(
        &store,
        &plan.placements[0],
        absence_provider(),
        0.00001,
        true,
    );
    let measured = candidate_facts(&store, &plan, 0, &Cancellation::default()).unwrap();
    assert!(measured.a.vocal_phrases.as_ref().unwrap().is_empty());
    assert!(measured.b.vocal_phrases.is_none());
    publish_supplement(&store, &plan.placements[0], absence_provider(), 0.1, true);
    assert!(candidate_facts(&store, &plan, 0, &Cancellation::default())
        .unwrap()
        .a
        .vocal_phrases
        .is_none());
}

#[test]
fn supplementary_analysis_cannot_replace_saved_beat_generation() {
    let (_temp, store, plan) = fixture();
    publish_supplement(
        &store,
        &plan.placements[0],
        absence_provider(),
        0.00001,
        false,
    );
    assert!(load_tactic_track_analysis(&store, &plan.placements[0])
        .unwrap()
        .is_none());
    assert!(candidate_facts(&store, &plan, 0, &Cancellation::default())
        .unwrap()
        .a
        .vocal_phrases
        .is_none());
}

#[test]
fn explicit_structural_and_independent_clocks_preserve_their_timing_basis() {
    let (_temp, store, mut plan) = fixture();
    plan.header.extensions.insert(
        "tactic_cue_candidate".into(),
        serde_json::json!({"timing_basis":"structural"}),
    );
    let structural = candidate_facts(&store, &plan, 0, &Cancellation::default()).unwrap();
    assert_eq!(structural.timing_basis, TacticTimingBasis::Structural);
    assert!(structural.a.bars.is_empty() && structural.b.bars.is_empty());
    plan.header.extensions.insert(
        "tactic_cue_candidate".into(),
        serde_json::json!({"timing_basis":"independent_pulses"}),
    );
    let independent = candidate_facts(&store, &plan, 0, &Cancellation::default()).unwrap();
    assert_eq!(
        independent.timing_basis,
        TacticTimingBasis::IndependentPulses
    );
    assert!(independent.pair.beat_skew_ms.is_none());
    assert!(!independent.pair.tempo_maps_validated);
    assert!(independent
        .b
        .bars
        .iter()
        .all(|b| b.beats_per_bar == Some(3)));
}

#[test]
fn operation_cache_reuses_source_analysis_but_not_candidate_time_maps() {
    let (_temp, store, plan) = fixture();
    let view = store.read_snapshot();
    let mut cache = CandidateEvidenceCache::new();
    let first = cache
        .candidate_facts(&view, &plan, 0, &Cancellation::default())
        .unwrap();
    assert_eq!(cache.cached_sources(), 2);
    let mut shifted = plan.clone();
    for p in &mut shifted.placements {
        for a in &mut p.time_map.anchors {
            a.output_frame += 1000;
        }
        for a in &mut p.automation {
            a.output_frame += 1000;
        }
    }
    for t in &mut shifted.transitions {
        t.start_output_frame += 1000;
        t.end_output_frame += 1000;
    }
    shifted.total_output_frames += 1000;
    let second = cache
        .candidate_facts(&view, &shifted, 0, &Cancellation::default())
        .unwrap();
    assert_eq!(cache.cached_sources(), 2);
    assert_eq!(
        second.bar_frames,
        first
            .bar_frames
            .iter()
            .map(|f| f + 1000)
            .collect::<Vec<_>>()
    );
    assert!(
        cache
            .candidate_facts(&store.read_snapshot(), &plan, 0, &Cancellation::default())
            .unwrap_err()
            .code
            == "TACTIC_READ_SNAPSHOT"
    );
    view.verify_read_snapshot(&Cancellation::default()).unwrap();
}

#[test]
fn operation_cache_requires_final_metadata_verification_and_observes_new_revision() {
    let (_temp, store, plan) = fixture();
    let view = store.read_snapshot();
    let mut cache = CandidateEvidenceCache::new();
    cache
        .candidate_facts(&view, &plan, 0, &Cancellation::default())
        .unwrap();
    let track = store.track(&plan.placements[0].track_id).unwrap();
    let mut corrections = store.overrides(&track).unwrap();
    corrections.operations.push(Correction::Meter {
        start_frame: 0,
        end_frame: track.format.frames,
        pulses_per_bar: 4,
    });
    store.set_overrides(&track.track_id, corrections).unwrap();
    assert_eq!(
        view.verify_read_snapshot(&Cancellation::default())
            .unwrap_err()
            .code,
        "READ_SNAPSHOT_CHANGED"
    );
    assert_eq!(
        candidate_facts(&store, &plan, 0, &Cancellation::default())
            .unwrap_err()
            .code,
        "OVERRIDE_CHANGED"
    );
}
