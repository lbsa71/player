//! Shared measured-join library and approved-plan persistence for complete-set refinement.
use super::*;
use crate::{
    composition::{Context, Request},
    search_cache::Cache,
    tactics::{CandidateQueue, MeasuredEdge},
};
use mixlib_planner::{BaselineCueIndex, CandidateEvidenceCache, EventCueIndex, StitchRequest, TrackPacing};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Instant,
};

#[derive(Default, Serialize)]
struct Stats {
    prepared_tracks: usize,
    proposed_pairs: usize,
    proposed_windows: usize,
    measured_windows: usize,
    cached_windows: usize,
    cached_proposals: usize,
    admitted_choices: usize,
    known_good_choices: usize,
    complete_candidates: usize,
    improvements: usize,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct Measurement {
    #[serde(default)]
    pub(super) sources: BTreeMap<String, String>,
    pub(super) edges: Vec<MeasuredEdge>,
    pub(super) continuations: Vec<MixPlan>,
    pub(super) admitted: BTreeMap<String, usize>,
    pub(super) rejected: BTreeMap<String, usize>,
    pub(super) evidence: Vec<Value>,
}

fn seconds(track: &Track, frames: u64) -> f64 {
    frames as f64 / f64::from(track.format.sample_rate_hz)
}
struct Search<'a> {
    store: &'a Store,
    snapshot: Store,
    policy: &'a MixingConfig,
    context: usize,
    pool: Vec<Track>,
    request: Request,
    composer: Context,
    required: required_pairs::RequiredPairs,
    pacing: Vec<TrackPacing>,
    cache: Cache,
    facts: CandidateEvidenceCache,
    event_cues: EventCueIndex,
    baseline_cues: BaselineCueIndex,
    worker_facts: Vec<CandidateEvidenceCache>,
    measured_keys: BTreeSet<String>,
    stats: Stats,
    started: Instant,
    progress: Report,
    history: Vec<Value>,
    incumbent: Option<MixPlan>,
    graph: Vec<Vec<Arc<MeasuredEdge>>>,
    known: BTreeSet<String>,
    edge_aliases: BTreeMap<String, String>,
    negative_windows: BTreeSet<String>,
}
impl Search<'_> {
    fn register(&mut self, edge: &MeasuredEdge) -> Result<()> {
        if edge.last_resort {
            return Ok(());
        }
        let quality = edge.dense_quality_seconds + edge.handoff_quality_seconds;
        if !quality.is_finite()
            || !(0.0..=mixlib_planner::MAX_TACTIC_CONTINUATION_SECONDS + 1e-6).contains(&quality)
        {
            self.cache.invalid += 1;
            return Ok(());
        }
        let mut identity = serde_json::to_value(&edge.plan)?;
        remove_ids(&mut identity);
        if !self.known.insert(canonical_hash(&identity)?.digest) {
            return Ok(());
        }
        let from = self
            .pool
            .iter()
            .position(|t| t.track_id == edge.plan.placements[0].track_id)
            .unwrap();
        let to = self
            .pool
            .iter()
            .position(|t| t.track_id == edge.plan.placements[1].track_id)
            .unwrap();
        self.graph[from * self.pool.len() + to].push(Arc::new(edge.clone()));
        self.stats.known_good_choices += 1;
        Ok(())
    }
    fn load_known(&mut self, cancel: &Cancellation) -> Result<()> {
        for key in self.cache.keys(self.store, "windows")? {
            cancel.check()?;
            let Some(measured) = self
                .cache
                .read::<Measurement>(self.store, "windows", &key)?
            else {
                continue;
            };
            self.measured_keys.insert(key.clone());
            if measured.edges.iter().all(|e| e.last_resort) && measured.continuations.is_empty() {
                self.negative_windows.insert(key);
            }
            for mut edge in measured.edges.into_iter().filter(|e| !e.last_resort) {
                let current = edge.plan.placements.iter().all(|p| {
                    self.pool
                        .iter()
                        .find(|t| t.track_id == p.track_id)
                        .is_some_and(|t| {
                            // Old cache records predate the source map. Bootstrap
                            // only uncorrected, fully pinned recordings from those.
                            let signature = measured.sources.get(&p.track_id).map_or(
                                t.active_overrides.is_none() && t.override_revision == 0,
                                |key| self.cache.sources.get(&p.track_id) == Some(key),
                            );
                            signature
                                && p.audio_hash == t.audio_hash
                                && p.audio_revision == t.audio_revision
                                && t.active_analysis.as_ref().is_some_and(|r| {
                                    r.hash == p.analysis.hash && r.path == p.analysis.path
                                })
                                && t.override_revision == p.override_revision
                                && p.time_map.source_sample_rate_hz == t.format.sample_rate_hz
                                && p.source_end_frame <= t.format.frames
                        })
                });
                if current {
                    let legacy = legacy_route_key(&edge.plan)?;
                    crate::search_cache::rebind(&mut edge.plan, self.policy)?;
                    let key = route_key(&edge.plan)?;
                    self.edge_aliases.insert(legacy, key.clone());
                    self.edge_aliases.insert(legacy_route_key(&edge.plan)?, key);
                    self.register(&edge)?;
                }
            }
        }
        eprintln!(
            "set search: loaded {} known good cue/tactic choices from the persistent cache",
            self.stats.known_good_choices
        );
        Ok(())
    }
    fn evidence(&self, status: &str) -> Value {
        json!({"method":"complete-set-refinement/1","status":status,
            "phase":if self.incumbent.is_some(){"refinement"}else{"baseline"},"stats":self.stats,
            "prepared_tracks":self.pool.len(),"elapsed_seconds":self.started.elapsed().as_secs_f64(),
            "first_valid_seconds":self.history.first().and_then(|h|h.get("elapsed_seconds")),
            "best_plan_id":self.incumbent.as_ref().map(|p|&p.plan_id),"improvements":self.history,
            "cache":{"hits":self.cache.hits,"misses":self.cache.misses,"invalid":self.cache.invalid},
            "scope":"Whole-set refinement over measured alternatives; approved complete incumbents are monotone. No global-optimality or infeasibility proof is claimed."})
    }
    fn checkpoint(&mut self, status: &str) -> Result<()> {
        self.progress
            .header
            .extensions
            .insert("search".into(), self.evidence(status));
        self.store.report(&self.progress)?;
        eprintln!("set search: {status} | {} measured / {} cached windows | {} approved improvements | {:.1}s",self.stats.measured_windows,self.stats.cached_windows,self.stats.improvements,self.started.elapsed().as_secs_f64());
        Ok(())
    }
    fn save(&mut self, mut plan: MixPlan, origin: &str, cancel: &Cancellation) -> Result<bool> {
        self.required.annotate(&mut plan)?;
        let Some(evidence) = self.composer.evaluate(&plan, true) else {
            return Ok(false);
        };
        plan.header
            .extensions
            .insert("set_composition".into(), evidence);
        if self
            .incumbent
            .as_ref()
            .is_some_and(|p| !crate::composition::better(&plan, p))
        {
            return Ok(false);
        }
        self.stats.complete_candidates += 1;
        self.snapshot.verify_read_snapshot(cancel)?;
        let previous_score = self.incumbent.as_ref().map(crate::composition::score);
        let event = if previous_score.is_some() {
            "better_set_found"
        } else if origin == "cached_complete_set" {
            "cached_baseline"
        } else {
            "baseline_found"
        };
        let labels: Vec<String> = plan
            .placements
            .iter()
            .map(|p| {
                let track = self.pool.iter().find(|t| t.track_id == p.track_id).unwrap();
                let metadata = self.snapshot.recording_metadata(track)?;
                Ok(
                    match (metadata["artist"].as_str(), metadata["track"].as_str()) {
                        (Some(a), Some(t)) => format!("{a} — {t}"),
                        _ => track
                            .source_locations
                            .first()
                            .map_or_else(|| p.track_id.clone(), |s| s.relative_path.clone()),
                    },
                )
            })
            .collect::<Result<_>>()?;
        plan.plan_id = id();
        plan.planner_version = "complete-set-refinement/1".into();
        crate::search_cache::rebind(&mut plan, self.policy)?;
        self.stats.improvements += 1;
        self.history.push(json!({"event":event,"plan_id":plan.plan_id,"origin":origin,"objective":crate::composition::score(&plan),
            "previous_objective":previous_score,"score_improvement":previous_score.map(|old|crate::composition::score(&plan)-old),"track_order":labels,
            "elapsed_seconds":self.started.elapsed().as_secs_f64(),"measured_windows":self.stats.measured_windows,
            "duration_seconds":plan.total_output_frames as f64/f64::from(plan.output.sample_rate_hz),"tracks":plan.placements.len()}));
        let mut evidence = self.evidence("incumbent");
        evidence["phase"] = json!("refinement");
        evidence["best_plan_id"] = json!(plan.plan_id);
        plan.header
            .extensions
            .insert("lazy_search".into(), evidence);
        self.store
            .write(&format!("plans/{}.json", plan.plan_id), &plan)?;
        self.cache.save_set(self.store, self.policy, &plan)?;
        let duration = (plan.total_output_frames as f64 / f64::from(plan.output.sample_rate_hz))
            .round() as u64;
        let gain = previous_score.map_or_else(
            || "first complete candidate".into(),
            |old| format!("improved by {:+.4}", crate::composition::score(&plan) - old),
        );
        eprintln!("set search: {} | {}:{:02} | {} tracks | score {:.4} ({gain}) | {:.1}s elapsed | {} measured windows",
            event.to_uppercase(), duration / 60, duration % 60, labels.len(), crate::composition::score(&plan), self.started.elapsed().as_secs_f64(), self.stats.measured_windows);
        eprintln!("set search: order: {}", labels.join(" → "));
        eprintln!(
            "set search: saved {}",
            self.store
                .path(&format!("plans/{}.json", plan.plan_id))?
                .display()
        );
        self.incumbent = Some(plan);
        self.checkpoint("running")?;
        Ok(true)
    }
    fn proposals(
        &mut self,
        from: usize,
        to: usize,
        after: u64,
        limit: usize,
        cancel: &Cancellation,
    ) -> Result<Vec<MixPlan>> {
        let key = self.cache.pair_key(
            &self.pool[from].track_id,
            &self.pool[to].track_id,
            &json!({"after":after,"context":self.context,"limit":limit,"baseline_generator":if limit==1 {2}else{0}}),
        )?;
        if let Some(mut plans) = self
            .cache
            .read::<Vec<MixPlan>>(self.store, "proposals", &key)?
        {
            for p in &mut plans {
                crate::search_cache::rebind(p, self.policy)?;
            }
            self.stats.cached_proposals += 1;
            return Ok(plans);
        }
        let request = StitchRequest {
            from: &self.pool[from].track_id,
            to: &self.pool[to].track_id,
            overlap_beats: 8,
            context_beats: self.context,
            outgoing_after_frame: after,
        };
        let plans = if limit == 1 {
            self.baseline_cues.candidates(&self.snapshot,&request,self.policy,4,cancel)?
        } else if limit == 0 {
            self.event_cues.candidates(&self.snapshot,&request,self.policy,2,cancel)?
        } else {
            match mixlib_planner::tactic_stitch_candidates(&self.snapshot,&request,self.policy,limit,cancel) {
                Ok(plans)=>plans,
                Err(e) if matches!(e.code.as_str(),"STITCH_INFEASIBLE"|"STITCH_EXCLUDED")=>vec![],
                Err(e)=>return Err(e),
            }
        };
        self.stats.proposed_pairs += 1;
        self.stats.proposed_windows += plans.len();
        self.cache.write(self.store, "proposals", &key, &plans)?;
        Ok(plans)
    }
    fn measure(&mut self, seed: &MixPlan, cancel: &Cancellation) -> Result<Measurement> {
        let key = window_key(&self.cache, seed)?;
        if self.negative_windows.contains(&key) {
            // These exact source/policy/window decisions were checksum-verified
            // at startup or measured in this run. Their full diagnostics remain
            // on disk; route expansion only needs to know there is no successor.
            self.cache.hits += 1;
            self.stats.cached_windows += 1;
            return Ok(Measurement::default());
        }
        if let Some(mut measured) = self
            .cache
            .read::<Measurement>(self.store, "windows", &key)?
        {
            for e in &mut measured.edges {
                crate::search_cache::rebind(&mut e.plan, self.policy)?;
            }
            for p in &mut measured.continuations {
                crate::search_cache::rebind(p, self.policy)?;
            }
            self.stats.cached_windows += 1;
            for edge in &measured.edges {
                self.register(edge)?;
            }
            return Ok(measured);
        }
        let measured = measure_candidate(
            &self.snapshot,
            self.policy,
            &self.cache,
            &mut self.facts,
            seed,
            self.context,
            cancel,
        )?;
        self.stats.measured_windows += 1;
        self.stats.admitted_choices += measured.edges.len();
        self.cache.write(self.store, "windows", &key, &measured)?;
        self.measured_keys.insert(key.clone());
        if measured.edges.iter().all(|e| e.last_resort) && measured.continuations.is_empty() {
            self.negative_windows.insert(key);
        }
        for edge in &measured.edges {
            self.register(edge)?;
        }
        Ok(measured)
    }
    /// Independent PCM windows share four persistent worker caches. Registration
    /// and durable writes remain serial after the scoped workers finish.
    fn measure_batch(&mut self,seeds:&[MixPlan],cancel:&Cancellation)->Result<Vec<Measurement>> {
        let mut result=vec![Measurement::default();seeds.len()];
        let mut fresh=vec![];
        let mut seen=BTreeSet::new();
        for (i,seed) in seeds.iter().enumerate() {
            let key=window_key(&self.cache,seed)?;
            if !seen.insert(key.clone()) {continue;}
            if self.measured_keys.contains(&key) || self.negative_windows.contains(&key) {
                result[i]=self.measure(seed,cancel)?;
            } else { fresh.push((i,key,seed)); }
        }
        if self.worker_facts.is_empty() {self.worker_facts=(0..4).map(|_|CandidateEvidenceCache::new()).collect();}
        let n=self.worker_facts.len();
        let snapshot=&self.snapshot; let policy=self.policy;let cache=&self.cache;let context=self.context;
        let measured=std::thread::scope(|scope|->Result<Vec<_>> {
            let tasks=self.worker_facts.iter_mut().enumerate().map(|(worker,facts)| {
                let fresh=&fresh;
                scope.spawn(move||->Result<Vec<_>> {
                    fresh.iter().skip(worker).step_by(n).map(|(i,key,seed)| {
                        cancel.check()?;
                        Ok((*i,key.clone(),measure_candidate(snapshot,policy,cache,facts,seed,context,cancel)?))
                    }).collect()
                })
            }).collect::<Vec<_>>();
            let mut all=vec![];
            for task in tasks {all.extend(task.join().map_err(|_|Error::new("TACTIC_WORKER","measurement worker failed"))??);}
            Ok(all)
        })?;
        for (i,key,measurement) in measured {
            self.stats.measured_windows+=1;
            self.stats.admitted_choices+=measurement.edges.len();
            self.cache.write(self.store,"windows",&key,&measurement)?;
            self.measured_keys.insert(key.clone());
            if measurement.edges.iter().all(|e|e.last_resort) && measurement.continuations.is_empty() {self.negative_windows.insert(key);}
            for edge in &measurement.edges {self.register(edge)?;}
            result[i]=measurement;
        }
        Ok(result)
    }

}
pub(super) fn window_key(cache: &Cache, seed: &MixPlan) -> Result<String> {
    // Time maps (including all tempo ramps), source intervals and cue facts
    // define the measured window. Random plan/map IDs do not define sound.
    let mut acoustic = serde_json::to_value(seed)?;
    remove_ids(&mut acoustic);
    acoustic.as_object_mut().unwrap().remove("policy");
    acoustic.as_object_mut().unwrap().remove("policy_hash");
    if let Some(e) = acoustic
        .get_mut("extensions")
        .and_then(Value::as_object_mut)
    {
        e.remove("set_config");
    }
    cache.pair_key(
        &seed.placements[0].track_id,
        &seed.placements[1].track_id,
        &acoustic,
    )
}

pub(super) fn measure_candidate(
    snapshot: &Store,
    policy: &MixingConfig,
    cache: &Cache,
    facts: &mut CandidateEvidenceCache,
    seed: &MixPlan,
    context: usize,
    cancel: &Cancellation,
) -> Result<Measurement> {
    let mut admission = tactics::policy(
        policy
            .header
            .extensions
            .get("allow_expressive_tactics")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    );
    admission.normal_speed_deviation = policy.max_speed_deviation;
    let mut queue = CandidateQueue::expansion_only(std::slice::from_ref(seed), 24);
    let mut admitted = BTreeMap::new();
    let mut rejected = BTreeMap::new();
    let mut evidence = vec![];
    let request = StitchRequest {
        from: &seed.placements[0].track_id,
        to: &seed.placements[1].track_id,
        overlap_beats: 8,
        context_beats: context,
        outgoing_after_frame: 0,
    };
    let edges = tactics::measure_window(
        snapshot,
        &request,
        policy,
        facts,
        seed,
        &mut queue,
        &admission,
        &mut admitted,
        &mut rejected,
        &mut evidence,
        cancel,
    )?;
    require(
        edges.iter().all(|e| {
            e.dense_quality_seconds + e.handoff_quality_seconds
                <= mixlib_planner::MAX_TACTIC_CONTINUATION_SECONDS + 1e-6
        }),
        "LAZY_BOUND",
        "measured quality exceeds future-join bound",
    )?;
    Ok(Measurement {
        sources: seed
            .placements
            .iter()
            .map(|p| (p.track_id.clone(), cache.sources[&p.track_id].clone()))
            .collect(),
        edges,
        continuations: queue.take_discovered(),
        admitted,
        rejected,
        evidence,
    })
}

fn remove_ids(value: &mut Value) {
    match value {
        Value::Object(m) => {
            for k in ["plan_id", "map_id", "transition_id", "created_at"] {
                m.remove(k);
            }
            for v in m.values_mut() {
                remove_ids(v)
            }
        }
        Value::Array(a) => {
            for v in a {
                remove_ids(v)
            }
        }
        _ => {}
    }
}
fn route_key(plan: &MixPlan) -> Result<String> {
    normalized_route_key(plan, true)
}
fn legacy_route_key(plan: &MixPlan) -> Result<String> {
    normalized_route_key(plan, false)
}
fn normalized_route_key(plan: &MixPlan, stable: bool) -> Result<String> {
    fn normalize(value: &mut Value) {
        match value {
            Value::Object(fields) => {
                // Recomputed passage-cache keys contain random map IDs. They
                // are memoization identity, not musical state or score evidence.
                fields.remove("cache_key");
                fields.remove("set_config");
                for value in fields.values_mut() {
                    normalize(value);
                }
            }
            Value::Array(values) => {
                for value in values {
                    normalize(value);
                }
            }
            _ => {}
        }
    }
    let mut value = serde_json::to_value(plan)?;
    // Musical policy is already part of the admission-cache namespace. Rebinding
    // search budgets or recipe names must not rename a measured musical choice.
    if stable {
        value.as_object_mut().unwrap().remove("policy");
        value.as_object_mut().unwrap().remove("policy_hash");
    }
    remove_ids(&mut value);
    normalize(&mut value);
    Ok(canonical_hash(&value)?.digest)
}

/// Prepare immutable source, composition and measurement context.
fn new_search<'a>(
    store: &'a Store,
    policy: &'a MixingConfig,
    context: usize,
    pool: Vec<Track>,
    request: Request,
    cancel: &Cancellation,
) -> Result<Search<'a>> {
    let started = Instant::now();
    require(
        (3..=128).contains(&pool.len()),
        "CHAIN_POOL",
        "duration search needs 3–128 distinct prepared songs",
    )?;
    let capacity = pool.iter().map(|t| seconds(t, t.format.frames)).sum();
    require(
        request.capacity_possible(capacity, 1. - policy.max_speed_deviation),
        "SET_POOL_DURATION",
        "prepared source capacity is shorter than the requested set",
    )?;
    let snapshot = store.read_snapshot();
    let required = required_pairs::RequiredPairs::resolve(&snapshot, policy, &pool, pool.len())?;
    let composer = Context::load(&snapshot, &pool, request, cancel)?;
    let mut pacing = vec![];
    for t in &pool {
        cancel.check()?;
        pacing.push(mixlib_planner::read_track_pacing(
            &snapshot,
            &t.track_id,
            cancel,
        )?);
    }

    let cache = Cache::open(policy, &pool)?;
    cache.migrate_legacy(store)?;
    let graph = vec![vec![]; pool.len() * pool.len()];
    let mut search = Search {
        store,
        snapshot,
        policy,
        context,
        request,
        composer,
        required,
        pacing,
        cache,
        facts: CandidateEvidenceCache::new(),
        event_cues: EventCueIndex::new(context),
        baseline_cues: BaselineCueIndex::new(context),
        worker_facts: vec![],
        measured_keys: BTreeSet::new(),
        stats: Stats {
            prepared_tracks: pool.len(),
            ..Stats::default()
        },
        started,
        progress: Report::new("duration_set_search"),
        history: vec![],
        incumbent: None,
        graph,
        known: BTreeSet::new(),
        edge_aliases: BTreeMap::new(),
        negative_windows: BTreeSet::new(),
        pool,
    };
    search
        .progress
        .header
        .extensions
        .insert("required_pairs".into(), search.required.evidence());
    Ok(search)
}

#[path = "complete_refinement.rs"]
mod complete_refinement;

pub(super) fn refine_set(
    store: &Store,
    policy: &MixingConfig,
    context: usize,
    pool: Vec<Track>,
    request: Request,
    cancel: &Cancellation,
) -> Result<Value> {
    complete_refinement::run(store, policy, context, pool, request, cancel)
}

pub(super) fn choose(
    store: &Store,
    policy: &MixingConfig,
    context: usize,
    pool: Vec<Track>,
    request: Request,
    cancel: &Cancellation,
) -> Result<MixPlan> {
    let result = refine_set(store, policy, context, pool, request, cancel)?;
    result
        .get("plan")
        .filter(|p| !p.is_null())
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .ok_or_else(|| {
            Error::new(
                "SET_REFINEMENT_PENDING",
                format!(
                    "Full-duration draft saved; approved joins remain unresolved. {}",
                    result["draft_path"]
                ),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn tactic_plan() -> MixPlan {
        let mut plan: MixPlan =
            serde_json::from_str(include_str!("../../../examples/mix-plan.json")).unwrap();
        plan.placements.truncate(2);
        plan.transitions.truncate(1);
        plan.total_output_frames = plan.placements[1]
            .time_map
            .anchors
            .last()
            .unwrap()
            .output_frame;
        plan.renderer_version = mixlib_audio::TACTIC_RENDERER.into();
        let start = plan.transitions[0].start_output_frame;
        let end = plan.transitions[0].end_output_frame;
        let tactic = TacticPlan {
            policy_version: TACTICS_POLICY_VERSION.into(),
            tactic_id: TacticId::S13,
            modifiers: vec![],
            arrangement: TacticArrangement::Blend,
            character: TacticCharacter::Subtle,
            permission: TacticPermission::Normal,
            outgoing_placement: 0,
            incoming_placement: 1,
            start_frame: start,
            hold_end_frame: start,
            exit_start_frame: start,
            phrase_exit: None,
            end_frame: end,
            bar_frames: (0..=4).map(|i| start + (end - start) * i / 4).collect(),
            timing_basis: TacticTimingBasis::Bars,
            bar_bpm: None,
            pulse_ratio: None,
            lanes: (0..2)
                .map(|placement| LaneSchedule {
                    placement,
                    lane: StemLane::Stereo,
                    gain: vec![
                        ScalarPoint {
                            output_frame: start,
                            value: if placement == 0 { 1. } else { 0. },
                        },
                        ScalarPoint {
                            output_frame: end,
                            value: if placement == 0 { 0. } else { 1. },
                        },
                    ],
                    eq: vec![],
                })
                .collect(),
            stem_assets: vec![],
            effects: vec![],
            source_edits: vec![],
            edit_speed_ratio_limits: [0.94, 1.06],
            vocal_occupancy: vec![],
            vocal_guard_frames: 14400,
            evidence: vec!["instrumental fixture with an explicit gradual schedule".into()],
            selection_reason: "cache identity regression fixture".into(),
            emergency: None,
        };
        write_tactics(&mut plan, vec![tactic]).unwrap();
        plan.validate().unwrap();
        plan
    }

    #[test]
    fn measured_edge_identity_survives_budget_recipe_and_random_id_rebinding() {
        let mut edge = MeasuredEdge {
            plan: tactic_plan(),
            exit: 0,
            entry_end: 0,
            score: 5.,
            last_resort: false,
            dense_quality_seconds: 0.,
            handoff_quality_seconds: 0.,
        };
        assert_eq!(read_tactics(&edge.plan).unwrap().len(), 1);
        let original = route_key(&edge.plan).unwrap();
        let mut policy = edge.plan.policy.clone();
        policy
            .header
            .extensions
            .insert("lazy_search_seconds".into(), json!(17));
        policy
            .header
            .extensions
            .insert("lazy_measurement_budget".into(), json!(81));
        policy.header.extensions.insert(
            "set_config".into(),
            json!({"name":"Another recipe","path":"Sets/other.json","hash":"different"}),
        );
        crate::search_cache::rebind(&mut edge.plan, &policy).unwrap();
        edge.plan.plan_id = id();
        for placement in &mut edge.plan.placements {
            placement.time_map.map_id = id();
        }
        edge.plan.transitions[0].transition_id = id();
        assert_eq!(route_key(&edge.plan).unwrap(), original);

        let mut changed_cue = edge.plan.clone();
        changed_cue.placements[1].source_start_frame += 1;
        changed_cue.placements[1].time_map.anchors[0].source_frame += 1;
        changed_cue.validate().unwrap();
        assert_ne!(route_key(&changed_cue).unwrap(), original);

        let mut changed_schedule = edge.plan.clone();
        let mut tactics = read_tactics(&changed_schedule).unwrap();
        tactics[0].tactic_id = TacticId::S02;
        let middle = (tactics[0].start_frame + tactics[0].end_frame) / 2;
        tactics[0].lanes[0].gain.insert(
            1,
            ScalarPoint {
                output_frame: middle,
                value: 0.25,
            },
        );
        write_tactics(&mut changed_schedule, tactics).unwrap();
        changed_schedule.validate().unwrap();
        assert_ne!(route_key(&changed_schedule).unwrap(), original);
    }
}
