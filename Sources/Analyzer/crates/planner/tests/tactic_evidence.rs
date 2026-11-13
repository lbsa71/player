#[path = "../../../tests/support.rs"]
mod support;
use mixlib_catalog::*;
use mixlib_planner::*;
use mixlib_types::*;
use std::fs;

fn fixture() -> (tempfile::TempDir, Store, MixPlan) {
    fixture_audio(16, false)
}

fn fixture_audio(seconds: u64, quiet_tail: bool) -> (tempfile::TempDir, Store, MixPlan) {
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
            (seconds * 48000) as usize,
            b"CC0",
            |i, _| {
                let level = if quiet_tail && i >= 8 * 48000 {
                    0.006
                } else {
                    0.08
                };
                level * (std::f64::consts::TAU * frequency * i as f64 / 48000.).sin()
            },
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
            events: (0..=seconds * 2 - 2)
                .map(|i| Beat {
                    event_id: format!("b{i}"),
                    source_frame: i * 24000,
                    score: 0.95,
                    local_bpm: Some(120.),
                })
                .collect(),
            downbeat_ids: (0..=seconds * 2 - 2)
                .step_by(3)
                .map(|i| format!("b{i}"))
                .collect(),
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
fn detected_endpoint_family_keeps_every_extension_past_the_ranked_limit() {
    let (_temp, store, mut seed) = fixture();
    let pulse = 24_000;
    let (outgoing, incoming, overlap) = (3 * pulse, 0, 6 * pulse);
    let point = |source_frame, output_frame| Anchor {
        source_frame,
        output_frame,
    };
    seed.transitions[0].start_output_frame = outgoing;
    seed.transitions[0].end_output_frame = outgoing + overlap;
    seed.placements[0].time_map.anchors = vec![
        point(0, 0),
        point(outgoing, outgoing),
        point(outgoing + overlap, outgoing + overlap),
    ];
    seed.placements[1].time_map.anchors = vec![
        point(incoming, outgoing),
        point(incoming + overlap, outgoing + overlap),
        point(30 * pulse, outgoing + 30 * pulse),
    ];
    let a = &seed.placements[0];
    publish_supplement(&store, a, absence_provider(), 0., true);
    let mut track = store.track(&a.track_id).unwrap();
    let mut manifest = store.analysis(&track).unwrap();
    let mut record: TacticTrackAnalysis = store
        .read_ref(&manifest.artifacts["tactic_analysis"])
        .unwrap();
    record.signal_evidence["events"] = serde_json::json!([9, 12, 15, 18, 21]
        .map(|n| { serde_json::json!({"kind":"drop","score":0.9,"frame":outgoing + n * pulse}) }));
    let prefix = format!(
        "tracks/{}/{}/analysis/{}",
        shard(&track.track_id),
        track.track_id,
        id()
    );
    manifest.artifacts.insert(
        "tactic_analysis".into(),
        store
            .write(&format!("{prefix}/tactic-analysis.json"), &record)
            .unwrap(),
    );
    manifest.analysis_id = id();
    track.active_analysis = Some(
        store
            .write(&format!("{prefix}/manifest.json"), &manifest)
            .unwrap(),
    );
    store.save_track(&track).unwrap();

    let request = StitchRequest {
        from: &seed.placements[0].track_id,
        to: &seed.placements[1].track_id,
        overlap_beats: 6,
        context_beats: 3,
        outgoing_after_frame: 0,
    };
    let cancel = Cancellation::default();
    let ranked = tactic_continuations(&store, &request, &seed.policy, &seed, 2, &cancel).unwrap();
    let full = tactic_continuations(&store, &request, &seed.policy, &seed, 256, &cancel).unwrap();
    let lengths = |plans: &[MixPlan]| {
        plans
            .iter()
            .map(|plan| {
                let cue = &plan.header.extensions["tactic_cue_candidate"];
                assert_eq!(cue["outgoing_source_frame"], outgoing);
                assert_eq!(cue["incoming_source_frame"], incoming);
                assert!(read_tactics(plan).unwrap().is_empty());
                cue["overlap_beats"].as_u64().unwrap()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(lengths(&ranked), [9, 21]);
    assert_eq!(lengths(&full), [9, 21, 12, 18, 15]);
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

#[test]
fn paged_bar_domain_covers_every_reliable_fixture_pair() {
    use std::collections::BTreeSet;

    let (_temp, store, plan) = fixture();
    let [a, b] = &plan.placements[..] else {
        panic!("expected a pair");
    };
    let cancel = Cancellation::default();
    let domain =
        BarCueDomain::open(&store, &a.track_id, &b.track_id, 3, &plan.policy, &cancel).unwrap();
    let whole = domain.range();
    let (first, second) = whole.clone().split().unwrap();
    assert_eq!(first.start, whole.start);
    assert_eq!(first.end, second.start);
    assert_eq!(second.end, whole.end);
    let mut actual = BTreeSet::new();
    let mut pruned_cells = 0;
    for mut range in [first, second] {
        while range.start < range.end {
            let page = domain.page(&store, &range, 13, 7, &cancel).unwrap();
            assert_eq!(page.scanned.start, range.start);
            assert!(page.scanned.start < page.scanned.end);
            assert_eq!(page.scanned.end, page.remainder.start);
            assert_eq!(page.remainder.end, range.end);
            assert!(page.scanned.end - page.scanned.start - page.block_pruned_cells <= 13);
            let mut cursor = page.scanned.start;
            let mut windows = 0;
            for span in &page.outcomes {
                assert_eq!(span.start, cursor);
                assert!(span.end > span.start);
                cursor = span.end;
                if span.outcome.label() == "timing_window" {
                    assert_eq!(span.end - span.start, 1);
                    windows += 1;
                }
            }
            assert_eq!(cursor, page.scanned.end);
            assert_eq!(windows, page.plans.len());
            pruned_cells += page.block_pruned_cells;
            for plan in page.plans {
                let cue = &plan.header.extensions["tactic_cue_candidate"];
                assert!(actual.insert((
                    cue["outgoing_source_frame"].as_u64().unwrap(),
                    cue["incoming_source_frame"].as_u64().unwrap(),
                    cue["overlap_beats"].as_u64().unwrap(),
                )));
                assert!(read_tactics(&plan).unwrap().is_empty());
            }
            range = serde_json::from_value(serde_json::to_value(page.remainder).unwrap()).unwrap();
        }
    }
    // Equal 120 BPM, 30 pulses and 3-pulse downbeats. Mapped half/double
    // variants exceed the speed limit, so enumerate only equal spans here.
    let mut expected = BTreeSet::new();
    for overlap in (3..30).step_by(3) {
        for exit in (3..=30 - overlap).step_by(3) {
            for entry in (0..=27 - overlap).step_by(3) {
                expected.insert((exit * 24000, entry * 24000, overlap));
            }
        }
    }
    assert_eq!(actual, expected);
    assert!(pruned_cells > 0);
    let other =
        BarCueDomain::open(&store, &a.track_id, &b.track_id, 4, &plan.policy, &cancel).unwrap();
    assert!(other.page(&store, &whole, 1, 1, &cancel).is_err());

    let mut track = store.track(&b.track_id).unwrap();
    let mut analysis = store.analysis(&track).unwrap();
    let mut beats: Beats = store.read_ref(&analysis.artifacts["beats"]).unwrap();
    beats.events = (0..=60)
        .map(|i| Beat {
            event_id: format!("b{i}"),
            source_frame: i * 12000,
            score: 0.95,
            local_bpm: Some(240.),
        })
        .collect();
    beats.downbeat_ids = (0..=60).step_by(3).map(|i| format!("b{i}")).collect();
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
    analysis.analysis_id = id();
    track.active_analysis = Some(
        store
            .write(&format!("{prefix}/manifest.json"), &analysis)
            .unwrap(),
    );
    store.save_track(&track).unwrap();

    let mapped =
        BarCueDomain::open(&store, &a.track_id, &b.track_id, 3, &plan.policy, &cancel).unwrap();
    assert!(mapped.page(&store, &whole, 1, 1, &cancel).is_err());
    let first = mapped
        .page(&store, &mapped.range(), 128, 1, &cancel)
        .unwrap();
    assert_eq!(first.plans.len(), 1);
    assert_eq!(
        first.plans[0].header.extensions["tactic_cue_candidate"]["pulse_ratio"],
        2.
    );
}

#[test]
fn structural_cue_pages_cover_the_unsampled_native_windows() {
    use std::collections::BTreeSet;

    let (_temp, store, plan) = fixture_audio(32, false);
    let [a, b] = &plan.placements[..] else {
        panic!("expected a pair");
    };
    let cancel = Cancellation::default();
    let open = || {
        StructuralCueDomain::open(&store, &a.track_id, &b.track_id, 3, &plan.policy, &cancel)
            .unwrap()
    };
    assert!(open().is_none());
    let publish = |p: &Placement, events: serde_json::Value, segments: serde_json::Value| {
        publish_supplement(&store, p, absence_provider(), 0., true);
        let mut track = store.track(&p.track_id).unwrap();
        let mut analysis = store.analysis(&track).unwrap();
        let mut signals: TacticTrackAnalysis = store
            .read_ref(&analysis.artifacts["tactic_analysis"])
            .unwrap();
        signals.signal_evidence["events"] = events;
        signals.signal_evidence["segments"] = segments;
        signals.signal_evidence["unmetered_windows"] = serde_json::json!([]);
        let prefix = format!(
            "tracks/{}/{}/analysis/{}",
            shard(&track.track_id),
            track.track_id,
            id()
        );
        analysis.artifacts.insert(
            "tactic_analysis".into(),
            store
                .write(&format!("{prefix}/tactic-analysis.json"), &signals)
                .unwrap(),
        );
        analysis.analysis_id = id();
        track.active_analysis = Some(
            store
                .write(&format!("{prefix}/manifest.json"), &analysis)
                .unwrap(),
        );
        store.save_track(&track).unwrap();
    };
    let span = |start, end| {
        serde_json::json!({
            "start_frame":start, "end_frame":end, "roles":["sustained_texture"]
        })
    };
    let starts = (0..72u64).map(|i| 48000 + 4096 * i).collect::<Vec<_>>();
    publish(
        a,
        serde_json::json!([{"kind":"natural_ending","frame":960000}]),
        serde_json::json!(std::iter::once(span(0, 48000))
            .chain(starts.iter().map(|&s| span(s, s + 48000)))
            .collect::<Vec<_>>()),
    );
    publish(
        b,
        serde_json::json!([{"kind":"audible_start","frame":48000}]),
        serde_json::json!([
            span(96000, 144000),
            span(192000, 288000),
            span(31 * 48000, 32 * 48000),
            span(96000, 144000)
        ]),
    );

    let domain = open().expect("pinned signal evidence");
    let range = domain.range();
    assert_eq!(range.end, 3 + 73 * 3);
    let (left, right) = range.clone().split().unwrap();
    let mut actual = BTreeSet::new();
    let mut pruned = 0;
    for mut pending in [left, right] {
        while pending.start < pending.end {
            let page = domain.page(&pending, 11, 3, &cancel).unwrap();
            assert_eq!(page.scanned.start, pending.start);
            assert_eq!(page.scanned.end, page.remainder.start);
            assert!(page.scanned.end - page.scanned.start - page.block_pruned_cells <= 11);
            assert_eq!(
                page.block_pruned_cells,
                page.outcomes
                    .iter()
                    .filter(|span| span.outcome.label() == "block_timing_rejected")
                    .map(|span| span.end - span.start)
                    .sum::<u64>()
            );
            pruned += page.block_pruned_cells;
            for plan in page.plans {
                let cue = &plan.header.extensions["tactic_cue_candidate"];
                let frames = [
                    "outgoing_source_frame",
                    "outgoing_end_frame",
                    "incoming_source_frame",
                    "incoming_end_frame",
                ];
                assert!(actual.insert(frames.map(|key| cue[key].as_u64().unwrap())));
                assert!(read_tactics(&plan).unwrap().is_empty());
            }
            pending =
                serde_json::from_value(serde_json::to_value(page.remainder).unwrap()).unwrap();
        }
    }
    let mut expected = starts
        .into_iter()
        .map(|s| [s, s + 48000, 96000, 144000])
        .collect::<BTreeSet<_>>();
    for seconds in [4, 8] {
        expected.insert([
            960000 - seconds * 48000,
            960000,
            45600,
            45600 + seconds * 48000,
        ]);
    }
    expected.insert([960000, 1008000, 45600, 93600]);
    assert_eq!(actual, expected);
    assert_eq!(pruned, 3 + 72 * 2);
    let no_context_row = BarCueRange {
        start: 3,
        end: 6,
        ..range
    };
    let block = domain.page(&no_context_row, 1, 1, &cancel).unwrap();
    assert_eq!(block.scanned, no_context_row);
    assert_eq!(block.block_pruned_cells, 3);
    assert!(block.plans.is_empty());
    let bar =
        BarCueDomain::open(&store, &a.track_id, &b.track_id, 3, &plan.policy, &cancel).unwrap();
    assert!(domain.page(&bar.range(), 1, 1, &cancel).is_err());
}

#[test]
fn ending_domain_retains_more_than_eight_clear_closings() {
    let (_temp, store, plan) = fixture_audio(32, true);
    let cancel = Cancellation::default();
    let first = &plan.placements[0];
    let last = plan.placements.last().unwrap();
    let mut absence = absence_provider();
    absence["track"]["verified_absence_spans"][0]["end_seconds"] = serde_json::json!(32.);
    publish_supplement(&store, last, absence, 0.00001, true);
    let window = ChainClosingWindow {
        earliest: 12 * 48000,
        latest: 31 * 48000,
    };
    let start =
        read_chain_opening(&store, &first.track_id, first.source_start_frame, &cancel).unwrap();
    let endings = chain_ending_candidates(&store, start, last, Some(window), &cancel).unwrap();
    assert_eq!(
        endings.iter().filter(|e| e.ending == "phrase_fade").count(),
        13
    );
    let preferred =
        choose_chain_bookends_in_window(&store, first, last, Some(window), &cancel).unwrap();
    assert_eq!(preferred.end, endings[0].end);
    assert_eq!(preferred.outro_fade_frames, endings[0].outro_fade_frames);
}
