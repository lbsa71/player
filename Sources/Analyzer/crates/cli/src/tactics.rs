//! The application adapter for measured candidate selection and saved execution.
use super::*;
use mixlib_planner::{RenderCapability, TacticPolicy};

pub(super) struct PairSelection {
    pub selected: Option<MixPlan>,
    pub missing_analysis: bool,
    pub examined: Vec<serde_json::Value>,
}

#[derive(Clone)]
pub(super) struct MeasuredEdge {
    pub plan: MixPlan,
    pub exit: u64,
    pub entry_end: u64,
    pub score: f64,
    pub last_resort: bool,
    pub dense_quality_seconds: f64,
    pub handoff_quality_seconds: f64,
}

pub(super) struct CandidateQueue {
    pending: std::collections::VecDeque<MixPlan>,
    seen: std::collections::BTreeSet<[u64; 4]>,
    extended: std::collections::BTreeMap<[u64; 2], usize>,
    remaining: usize,
    hold_reserve: usize,
}

fn cue_key(plan: &MixPlan) -> [u64; 4] {
    let t = &plan.transitions[0];
    [
        plan.placements[0]
            .time_map
            .source_at(t.start_output_frame as f64)
            .round() as u64,
        plan.placements[1]
            .time_map
            .source_at(t.start_output_frame as f64)
            .round() as u64,
        plan.placements[0]
            .time_map
            .source_at(t.end_output_frame as f64)
            .round() as u64,
        plan.placements[1]
            .time_map
            .source_at(t.end_output_frame as f64)
            .round() as u64,
    ]
}

impl CandidateQueue {
    pub(super) fn expansion_only(plans: &[MixPlan], budget: usize) -> Self {
        let mut queue = Self::new(plans.to_vec(), budget);
        queue.pending.clear();
        queue
    }
    pub(super) fn take_discovered(&mut self) -> Vec<MixPlan> {
        self.pending.drain(..).collect()
    }
    fn new(plans: Vec<MixPlan>, budget: usize) -> Self {
        Self {
            seen: plans.iter().map(cue_key).collect(),
            pending: plans.into(),
            extended: Default::default(),
            remaining: budget,
            hold_reserve: budget / 2,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn extend_cues(
    store: &Store,
    request: &mixlib_planner::StitchRequest<'_>,
    mixing: &MixingConfig,
    candidate: &MixPlan,
    selection: &mixlib_planner::TacticSelection,
    facts: &mixlib_planner::CandidateFacts,
    queue: &mut CandidateQueue,
    cancel: &Cancellation,
) -> Result<()> {
    let promising_dense = facts.pair.profiles.get(&TacticId::M01).is_some_and(|p| {
        p.predicted_peak_dbfs.is_some_and(|v| v <= -1.)
            && p.complementarity.is_some_and(|v| v >= 0.65)
            && p.harmonic_distance.is_some_and(|v| v <= 0.30)
            && p.lead_conflict.is_some_and(|v| v <= 0.20)
            && p.selected_composite_density
                .as_ref()
                .is_some_and(|v| v.len() >= 8 && v[..v.len() - 2].iter().all(|v| *v >= 0.62))
    });
    let held = promising_dense
        || selection.candidates.iter().any(|c| {
            !c.last_resort
                && matches!(
                    c.plan.tactic_id,
                    TacticId::M01 | TacticId::M02 | TacticId::S04 | TacticId::S05 | TacticId::S06
                )
        });
    let blend = selection.candidates.iter().any(|c| {
        !c.last_resort
            && matches!(
                c.plan.tactic_id,
                TacticId::S01 | TacticId::S03 | TacticId::S08
            )
    });
    let limit = if held {
        queue.remaining.min(8)
    } else if blend {
        queue.remaining.saturating_sub(queue.hold_reserve).min(4)
    } else {
        0
    };
    if limit == 0 {
        return Ok(());
    }
    let key = cue_key(candidate);
    if queue
        .extended
        .get(&[key[0], key[1]])
        .is_some_and(|old| *old >= limit)
    {
        return Ok(());
    }
    queue.extended.insert([key[0], key[1]], limit);
    for plan in
        mixlib_planner::tactic_continuations(store, request, mixing, candidate, limit, cancel)?
    {
        if queue.seen.insert(cue_key(&plan)) {
            queue.pending.push_back(plan);
            queue.remaining -= 1;
        }
    }
    Ok(())
}

fn window_diagnostic(
    candidate: &MixPlan,
    facts: &mixlib_planner::CandidateFacts,
    selection: &mixlib_planner::TacticSelection,
) -> serde_json::Value {
    serde_json::json!({"cue":candidate.header.extensions.get("tactic_cue_candidate"),
        "bars":facts.bar_frames.len()-1,"accepted":selection.candidates.iter().map(|c|c.plan.tactic_id).collect::<Vec<_>>(),
        "ranked":selection.candidates.iter().map(|c|serde_json::json!({"tactic":c.plan.tactic_id,"score":c.score,"dense_quality_seconds":c.dense_quality_seconds,"handoff_quality_seconds":c.handoff_quality_seconds,"phrase_exit":c.plan.phrase_exit})).collect::<Vec<_>>(),
        "profiles":facts.pair.profiles,"swift_profiles":facts.pair.swift_profiles,"rhythmic_profiles":facts.pair.rhythmic_profiles,"rhythmic_exit":facts.pair.rhythmic_exit,"a_roles":facts.a.roles,"b_roles":facts.b.roles,
        "a_endpoint":facts.a.endpoint_reasons,"b_endpoint":facts.b.endpoint_reasons,"rejected":selection.rejected})
}

/// Measure independent alternatives. The chain search owns the decision about
/// how much runway a preceding join leaves for each one.
pub(super) fn measure_edges(
    store: &Store,
    request: &mixlib_planner::StitchRequest<'_>,
    mixing: &MixingConfig,
    cache: &mut mixlib_planner::CandidateEvidenceCache,
    budget: usize,
    cancel: &Cancellation,
) -> Result<(Vec<MeasuredEdge>, serde_json::Value)> {
    let mut admission = policy(
        mixing
            .header
            .extensions
            .get("allow_expressive_tactics")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    );
    admission.normal_speed_deviation = mixing.max_speed_deviation;
    let candidates =
        match mixlib_planner::tactic_stitch_candidates(store, request, mixing, budget, cancel) {
            Ok(candidates) => candidates,
            Err(error)
                if matches!(error.code.as_str(), "STITCH_INFEASIBLE" | "STITCH_EXCLUDED") =>
            {
                return Ok((vec![], serde_json::json!({"timing_rejection":error.code})))
            }
            Err(error) => return Err(error),
        };
    let mut edges = vec![];
    let mut admitted = std::collections::BTreeMap::<String, usize>::new();
    let mut rejected = std::collections::BTreeMap::<String, usize>::new();
    let mut queue = CandidateQueue::new(candidates, budget);
    let mut windows = 0;
    let mut details = vec![];
    while let Some(candidate) = queue.pending.pop_front() {
        windows += 1;
        cancel.check()?;
        edges.extend(measure_window(
            store,
            request,
            mixing,
            cache,
            &candidate,
            &mut queue,
            &admission,
            &mut admitted,
            &mut rejected,
            &mut details,
            cancel,
        )?);
    }
    Ok((
        edges,
        serde_json::json!({"from":request.from,"to":request.to,
        "windows":windows,"admitted":admitted,"rejected":rejected,"window_details":details}),
    ))
}

/// One expensive audio measurement, with exactly the same admission and dynamic
/// endpoint discovery used by eager search. Caller owns scheduling and caching.
#[allow(clippy::too_many_arguments)]
pub(super) fn measure_window(
    store: &Store,
    request: &mixlib_planner::StitchRequest<'_>,
    mixing: &MixingConfig,
    cache: &mut mixlib_planner::CandidateEvidenceCache,
    candidate: &MixPlan,
    queue: &mut CandidateQueue,
    admission: &TacticPolicy,
    admitted: &mut std::collections::BTreeMap<String, usize>,
    rejected: &mut std::collections::BTreeMap<String, usize>,
    details: &mut Vec<serde_json::Value>,
    cancel: &Cancellation,
) -> Result<Vec<MeasuredEdge>> {
    let mut edges = vec![];
    let facts = match cache.candidate_facts(store, candidate, 0, cancel) {
        Ok(facts) => facts,
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
            *rejected.entry(error.code).or_default() += 1;
            return Ok(vec![]);
        }
        Err(error) => return Err(error),
    };
    let selection = mixlib_planner::select_tactics(&facts, admission)?;
    details.push(window_diagnostic(candidate, &facts, &selection));
    extend_cues(
        store, request, mixing, candidate, &selection, &facts, queue, cancel,
    )?;
    for profile in facts
        .pair
        .profiles
        .values()
        .filter(|p| p.predicted_peak_dbfs.is_none())
    {
        for reason in &profile.evidence {
            *rejected.entry(reason.clone()).or_default() += 1;
        }
    }
    for rejection in &selection.rejected {
        for reason in &rejection.reasons {
            *rejected
                .entry(format!("{:?}: {reason}", rejection.tactic_id))
                .or_default() += 1;
        }
    }
    let t = &candidate.transitions[0];
    let exit = candidate.placements[0]
        .time_map
        .source_at(t.start_output_frame as f64)
        .round() as u64;
    let entry_end = candidate.placements[1]
        .time_map
        .source_at(t.end_output_frame as f64)
        .round() as u64;
    for ranked in selection.candidates {
        let id = format!("{:?}", ranked.plan.tactic_id);
        let mut plan = candidate.clone();
        if write_tactics(&mut plan, vec![ranked.plan]).is_err() {
            continue;
        }
        plan.header.extensions.insert(
            "dense_quality_seconds".into(),
            serde_json::json!(ranked.dense_quality_seconds),
        );
        plan.header.extensions.insert(
            "handoff_quality_seconds".into(),
            serde_json::json!(ranked.handoff_quality_seconds),
        );
        plan.renderer_version = mixlib_audio::TACTIC_RENDERER.into();
        plan.validate()?;
        *admitted.entry(id).or_default() += 1;
        edges.push(MeasuredEdge {
            plan,
            exit,
            entry_end,
            score: ranked.score,
            last_resort: ranked.last_resort,
            dense_quality_seconds: ranked.dense_quality_seconds,
            handoff_quality_seconds: ranked.handoff_quality_seconds,
        });
    }
    Ok(edges)
}

#[derive(Clone, Copy)]
enum Search<'a> {
    Ranked { next_track: Option<&'a str> },
    FirstValid,
}

pub(super) fn choose_pair(
    store: &Store,
    request: &mixlib_planner::StitchRequest<'_>,
    mixing: &MixingConfig,
    next_track: Option<&str>,
    previous_join_end: u64,
    cancel: &Cancellation,
) -> Result<PairSelection> {
    let snapshot = store.read_snapshot();
    let mut evidence_cache = mixlib_planner::CandidateEvidenceCache::new();
    let mut selection = choose_pair_inner(
        &snapshot,
        request,
        mixing,
        Search::Ranked { next_track },
        previous_join_end,
        &mut evidence_cache,
        24,
        cancel,
    )?;
    if selection.selected.is_none() && !selection.missing_analysis {
        selection = choose_pair_inner(
            &snapshot,
            request,
            mixing,
            Search::Ranked { next_track },
            previous_join_end,
            &mut evidence_cache,
            64,
            cancel,
        )?;
    }
    snapshot.verify_read_snapshot(cancel)?;
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

#[allow(clippy::too_many_arguments)]
fn choose_pair_inner(
    store: &Store,
    request: &mixlib_planner::StitchRequest<'_>,
    mixing: &MixingConfig,
    search: Search<'_>,
    previous_join_end: u64,
    evidence_cache: &mut mixlib_planner::CandidateEvidenceCache,
    candidate_budget: usize,
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
    let outgoing_pacing = (previous_join_end > 0)
        .then(|| mixlib_planner::read_track_pacing(store, request.from, cancel))
        .transpose()?;
    let incoming_pacing = if matches!(
        search,
        Search::Ranked {
            next_track: Some(_)
        }
    ) {
        Some(mixlib_planner::read_track_pacing(
            store, request.to, cancel,
        )?)
    } else {
        None
    };
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
    let mut successor_witness: Option<u64> = None;
    let seeds =
        mixlib_planner::tactic_stitch_candidates(store, request, mixing, candidate_budget, cancel)?;
    let mut queue = CandidateQueue::new(seeds, candidate_budget);
    while let Some(candidate) = queue.pending.pop_front() {
        cancel.check()?;
        let pacing = if let Some(policy) = &outgoing_pacing {
            let exit = candidate.placements[0]
                .time_map
                .source_at(candidate.transitions[0].start_output_frame as f64)
                .round() as u64;
            let Some(decision) = policy.assess(previous_join_end, exit) else {
                examined.push(serde_json::json!({"cue":candidate.header.extensions.get("tactic_cue_candidate"),"pacing_rejection":"no complete measured phrase at this early exit; insufficient conservative runway"}));
                continue;
            };
            Some(decision)
        } else {
            None
        };
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
        extend_cues(
            store, request, mixing, &candidate, &selection, &facts, &mut queue, cancel,
        )?;
        for ranked in &selection.candidates {
            let mut trial = candidate.clone();
            if let Err(error) = write_tactics(&mut trial, vec![ranked.plan.clone()]) {
                examined.push(serde_json::json!({"cue":candidate.header.extensions.get("tactic_cue_candidate"),
                    "tactic":ranked.plan.tactic_id,"validation_rejection":error.to_string()}));
                continue;
            }
            trial.renderer_version = mixlib_audio::TACTIC_RENDERER.into();
            if let Search::Ranked {
                next_track: Some(next),
            } = search
            {
                let incoming = &trial.placements[1];
                let entry_end = incoming
                    .time_map
                    .source_at(trial.transitions[0].end_output_frame as f64)
                    .round() as u64;
                let after = incoming_pacing.as_ref().unwrap().earliest_exit(entry_end);
                let future = mixlib_planner::StitchRequest {
                    from: request.to,
                    to: next,
                    overlap_beats: 8,
                    context_beats: request.context_beats,
                    outgoing_after_frame: after,
                };
                let safe = if successor_witness.is_some_and(|frame| {
                    incoming_pacing
                        .as_ref()
                        .unwrap()
                        .assess(entry_end, frame)
                        .is_some()
                }) {
                    true
                } else if let Some(safe) = successor_cache.get(&entry_end) {
                    *safe
                } else {
                    let safe = match choose_pair_inner(
                        store,
                        &future,
                        mixing,
                        Search::FirstValid,
                        entry_end,
                        evidence_cache,
                        candidate_budget,
                        cancel,
                    ) {
                        Ok(selection) => {
                            if let Some(plan) = selection.selected {
                                let source = plan.placements[0]
                                    .time_map
                                    .source_at(plan.transitions[0].start_output_frame as f64)
                                    .floor() as u64;
                                successor_witness =
                                    Some(successor_witness.map_or(source, |old| old.max(source)));
                                true
                            } else {
                                false
                            }
                        }
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
                    successor_cache.insert(entry_end, safe);
                    safe
                };
                if !safe {
                    continue;
                }
            }
            if let Some(decision) = &pacing {
                trial
                    .header
                    .extensions
                    .insert("pacing".into(), serde_json::to_value(decision)?);
            }
            trial.validate()?;
            if matches!(search, Search::FirstValid) {
                return Ok(PairSelection {
                    selected: Some(trial),
                    missing_analysis: false,
                    examined: vec![],
                });
            }
            accepted = Some((
                ranked.score - pacing.as_ref().map_or(0., |p| p.penalty_quality_seconds),
                trial,
            ));
            break;
        }
        examined.push(serde_json::json!({"cue":candidate.header.extensions.get("tactic_cue_candidate"),
            "selected":accepted.as_ref().map(|(_,p)|read_tactics(p).map(|t|t[0].tactic_id)).transpose()?,
            "evidence_summary":{"a_vocal_phrases":facts.a.vocal_phrases.as_ref().map(Vec::len),
                "b_vocal_phrases":facts.b.vocal_phrases.as_ref().map(Vec::len),
                "a_boundaries":facts.a.phrase_boundaries,"b_boundaries":facts.b.phrase_boundaries,
                "a_stems":facts.a.stems,"b_stems":facts.b.stems,
                "beat_skew_ms":facts.pair.beat_skew_ms,"harmonic_distance":facts.pair.harmonic_distance,
                "predicted_peak_dbfs":facts.pair.predicted_peak_dbfs,
                "short_fade_match":facts.pair.short_fade_match,
                "outgoing_build_release_frame":facts.a.build_release_frame},
            "window_diagnostic":window_diagnostic(&candidate,&facts,&selection),
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
