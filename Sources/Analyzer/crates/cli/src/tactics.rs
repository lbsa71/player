//! The application adapter for measured candidate selection and saved execution.
use super::*;
use mixlib_planner::{RenderCapability, TacticPolicy};

pub(super) struct PairSelection {
    pub selected: Option<MixPlan>,
    pub missing_analysis: bool,
    pub examined: Vec<serde_json::Value>,
}

pub(super) fn choose_pair(
    store: &Store,
    request: &mixlib_planner::StitchRequest<'_>,
    mixing: &MixingConfig,
    next_track: Option<&str>,
    cancel: &Cancellation,
) -> Result<PairSelection> {
    let snapshot = store.read_snapshot();
    let mut evidence_cache = mixlib_planner::CandidateEvidenceCache::new();
    let result = choose_pair_inner(
        &snapshot,
        request,
        mixing,
        next_track,
        &mut evidence_cache,
        cancel,
    );
    snapshot.verify_read_snapshot(cancel)?;
    let selection = result?;
    if selection.selected.is_none() && !selection.missing_analysis {
        let mut report = Report::new("tactic_search");
        report
            .counts
            .insert("examined".into(), selection.examined.len() as u64);
        report
            .header
            .extensions
            .insert("from".into(), serde_json::json!(request.from));
        report
            .header
            .extensions
            .insert("to".into(), serde_json::json!(request.to));
        report
            .header
            .extensions
            .insert("candidates".into(), serde_json::json!(selection.examined));
        store.report(&report)?;
        return Err(Error::new("TACTIC_NO_SAFE_JOIN",format!(
            "all {} measured cue/tactic candidates were rejected; choose another window, order or track; rejection evidence: reports/{}.json",
            selection.examined.len(),report.report_id)));
    }
    Ok(selection)
}

fn choose_pair_inner(
    store: &Store,
    request: &mixlib_planner::StitchRequest<'_>,
    mixing: &MixingConfig,
    next_track: Option<&str>,
    evidence_cache: &mut mixlib_planner::CandidateEvidenceCache,
    cancel: &Cancellation,
) -> Result<PairSelection> {
    for id in [request.from, request.to] {
        let track = store.track(id)?;
        if !store
            .analysis(&track)?
            .artifacts
            .contains_key("tactic_analysis")
        {
            return Ok(PairSelection {
                selected: None,
                missing_analysis: true,
                examined: vec![serde_json::json!({
                    "track_id":id,"reason":"full-track tactic evidence is unavailable"
                })],
            });
        }
    }
    let allow_expressive = mixing
        .header
        .extensions
        .get("allow_expressive_tactics")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let mut admission = policy(allow_expressive);
    admission.normal_speed_deviation = mixing.max_speed_deviation;
    let mut examined = Vec::new();
    let mut best: Option<(f64, MixPlan)> = None;
    let mut successor_cache = std::collections::BTreeMap::new();
    for mut candidate in
        mixlib_planner::tactic_stitch_candidates(store, request, mixing, 24, cancel)?
    {
        cancel.check()?;
        let measured = evidence_cache
            .candidate_facts(store, &candidate, 0, cancel)
            .and_then(|facts| {
                mixlib_planner::select_tactics(&facts, &admission)
                    .map(|selection| (facts, selection))
            });
        let (facts, selection) = match measured {
            Ok(value) => value,
            Err(error)
                if matches!(
                    error.code.as_str(),
                    "TACTIC_FACTS"
                        | "TACTIC_GRID"
                        | "TACTIC_BAR_WINDOW"
                        | "TACTIC_NATIVE_GRID"
                        | "TACTIC_BOUNDARIES"
                ) =>
            {
                examined.push(serde_json::json!({"cue":candidate.header.extensions.get("tactic_cue_candidate"),
                    "evidence_rejection":{"code":error.code,"reason":error.message}}));
                continue;
            }
            Err(error) => return Err(error),
        };
        let mut accepted = None;
        for ranked in &selection.candidates {
            let mut trial = candidate.clone();
            if let Err(error) = write_tactics(&mut trial, vec![ranked.plan.clone()]) {
                examined.push(serde_json::json!({"cue":candidate.header.extensions.get("tactic_cue_candidate"),
                    "tactic":ranked.plan.tactic_id,"validation_rejection":error.to_string()}));
                continue;
            }
            trial.renderer_version = mixlib_audio::TACTIC_RENDERER.into();
            if let Some(next) = next_track {
                let incoming = &trial.placements[1];
                let after = incoming
                    .time_map
                    .source_at(trial.transitions[0].end_output_frame as f64)
                    .round() as u64
                    + mixlib_planner::CHAIN_SOLO_SECONDS
                        * u64::from(incoming.time_map.source_sample_rate_hz);
                let future = mixlib_planner::StitchRequest {
                    from: request.to,
                    to: next,
                    overlap_beats: 8,
                    context_beats: request.context_beats,
                    outgoing_after_frame: after,
                };
                let safe = if let Some(safe) = successor_cache.get(&after) {
                    *safe
                } else {
                    let safe = match choose_pair_inner(
                        store,
                        &future,
                        mixing,
                        None,
                        evidence_cache,
                        cancel,
                    ) {
                        Ok(selection) => selection.selected.is_some(),
                        Err(error)
                            if matches!(
                                error.code.as_str(),
                                "STITCH_INFEASIBLE" | "TACTIC_NO_SAFE_JOIN"
                            ) =>
                        {
                            false
                        }
                        Err(error) => return Err(error),
                    };
                    successor_cache.insert(after, safe);
                    safe
                };
                if !safe {
                    continue;
                }
            }
            trial.validate()?;
            accepted = Some((ranked.score, trial));
            break;
        }
        examined.push(serde_json::json!({"cue":candidate.header.extensions.remove("tactic_cue_candidate"),
            "selected":accepted.as_ref().map(|(_,p)|read_tactics(p).map(|t|t[0].tactic_id)).transpose()?,
            "evidence_summary":{"a_vocal_phrases":facts.a.vocal_phrases.as_ref().map(Vec::len),
                "b_vocal_phrases":facts.b.vocal_phrases.as_ref().map(Vec::len),
                "a_boundaries":facts.a.phrase_boundaries,"b_boundaries":facts.b.phrase_boundaries,
                "a_stems":facts.a.stems,"b_stems":facts.b.stems,
                "beat_skew_ms":facts.pair.beat_skew_ms,"harmonic_distance":facts.pair.harmonic_distance,
                "predicted_peak_dbfs":facts.pair.predicted_peak_dbfs,
                "short_fade_match":facts.pair.short_fade_match,
                "outgoing_build_release_frame":facts.a.build_release_frame},
            "rejected_tactics":selection.rejected}));
        if let Some((score, trial)) = accepted {
            if best.as_ref().is_none_or(|(old, _)| score > *old) {
                best = Some((score, trial));
            }
        }
    }
    let selected = best.map(|(_, mut plan)| {
        plan.header
            .extensions
            .insert("joint_tactic_search".into(), serde_json::json!(examined));
        plan
    });
    Ok(PairSelection {
        selected,
        examined,
        missing_analysis: false,
    })
}

pub(super) fn policy(allow_expressive: bool) -> TacticPolicy {
    TacticPolicy {
        allow_expressive,
        capabilities: [
            RenderCapability::Gain,
            RenderCapability::Eq,
            RenderCapability::Stems,
            RenderCapability::Filter,
            RenderCapability::Delay,
            RenderCapability::Reverb,
            RenderCapability::Gate,
            RenderCapability::Noise,
            RenderCapability::Spiral,
            RenderCapability::SourceEdits,
            RenderCapability::PitchSampler,
            RenderCapability::TempoMap,
            RenderCapability::PulseMapping,
        ]
        .into_iter()
        .collect(),
        ..TacticPolicy::default()
    }
}

pub(super) fn select(
    store: &Store,
    plan: &mut MixPlan,
    apply: bool,
    allow_expressive: bool,
    requested: Option<TacticId>,
    cancel: &Cancellation,
) -> Result<serde_json::Value> {
    let mut policy = policy(allow_expressive);
    policy.normal_speed_deviation = plan.policy.max_speed_deviation;
    let snapshot = store.read_snapshot();
    let mut evidence_cache = mixlib_planner::CandidateEvidenceCache::new();
    let mut selections = Vec::new();
    let mut chosen = Vec::new();
    for index in 0..plan.transitions.len() {
        cancel.check()?;
        let facts = evidence_cache.candidate_facts(&snapshot, plan, index, cancel)?;
        let selection = mixlib_planner::select_tactics(&facts, &policy)?;
        if let Some(tactic_id) = requested {
            require(
                !tactic_id.is_modifier(),
                "TACTIC_MODIFIER",
                "tempo modifiers attach to a primary tactic",
            )?;
            chosen.push(mixlib_planner::schedule_tactic(tactic_id, &facts, &policy)?);
        } else if let Some(best) = selection.candidates.first() {
            chosen.push(best.plan.clone());
        }
        selections
            .push(serde_json::json!({"transition":index,"facts":facts,"selection":selection}));
    }
    snapshot.verify_read_snapshot(cancel)?;
    if apply {
        require(
            !chosen.is_empty() && chosen.len() == plan.transitions.len(),
            "TACTIC_INFEASIBLE",
            "every join requires a tactic with sufficient measured evidence and executable assets",
        )?;
        require(
            plan.drum_intro.is_none() && plan.prepared_patches.is_empty(),
            "TACTIC_PREPARED",
            "prepared stereo handoffs need their original lane assets before rescheduling",
        )?;
        write_tactics(plan, chosen)?;
        plan.renderer_version = mixlib_audio::TACTIC_RENDERER.into();
        plan.header
            .extensions
            .insert("tactic_selection".into(), serde_json::json!(selections));
        plan.validate()?;
    }
    Ok(serde_json::json!({"applied":apply,"joins":selections}))
}

pub(super) fn automatic(store: &Store, plan: &mut MixPlan, cancel: &Cancellation) -> Result<()> {
    if !read_tactics(plan)?.is_empty() {
        return Ok(());
    }
    // These assets already contain scheduled stem envelopes. Reapplying lane
    // automation would process them twice; their preparation owns that decision.
    if plan.drum_intro.is_some() || !plan.prepared_patches.is_empty() {
        return Ok(());
    }
    let allow_expressive = plan
        .policy
        .header
        .extensions
        .get("allow_expressive_tactics")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let mut proposed = plan.clone();
    match select(store, &mut proposed, true, allow_expressive, None, cancel) {
        Ok(_) => {
            *plan = proposed;
        }
        Err(error)
            if [
                "TACTIC_INFEASIBLE",
                "TACTIC_FACTS",
                "TACTIC_GRID",
                "TACTIC_BOUNDARIES",
                "TACTIC_BAR_WINDOW",
            ]
            .contains(&error.code.as_str()) =>
        {
            let evidence_available = plan.placements.iter().all(|placement| {
                store
                    .track(&placement.track_id)
                    .and_then(|track| store.analysis(&track))
                    .is_ok_and(|a| a.artifacts.contains_key("tactic_analysis"))
            });
            require(!evidence_available,"TACTIC_NO_SAFE_JOIN",
                "measured tactic constraints rejected this schedule; legacy rendering cannot bypass them")?;
            plan.header.extensions.insert(
                "tactic_selection_unavailable".into(),
                serde_json::json!({"code":error.code,"reason":error.message,
                    "execution":"legacy_schedule_without_full_tactic_admission"}),
            );
        }
        Err(error) => return Err(error),
    }
    Ok(())
}
