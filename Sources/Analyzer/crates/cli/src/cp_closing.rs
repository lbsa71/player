//! Closing work and its portable checkpoint. Only measured edge identities
//! and work cursors cross runs. Plans and endings are rebuilt on restore.

use super::*;
use crate::composition::ReplayScoreBound;
use crate::set_replays::{ReplayCheckpoint, ReplayDomain, ReplayPreparation};
use mixlib_planner::ChainBookends;
use mixlib_set_search::clock_choices::Advance;

const CHECKPOINT_PROFILE: &str = "cp-closing/3";

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct EndingKey {
    end: u64,
    fade: u64,
    natural: bool,
}

impl From<&ChainBookends> for EndingKey {
    fn from(ending: &ChainBookends) -> Self {
        Self {
            end: ending.end,
            fade: ending.outro_fade_frames,
            natural: ending.ending == "natural_tail",
        }
    }
}

#[derive(Serialize, Deserialize)]
struct SavedFrontier {
    pending: Vec<SavedPending>,
    completed_parts: Vec<String>,
    incumbent_score: Option<f64>,
    #[serde(default)]
    closing_continuations: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
struct SavedPending {
    part: String,
    route: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct SavedJob {
    route: Vec<String>,
    work: SavedWork,
}

#[derive(Serialize, Deserialize)]
struct SavedRoute<T = SavedWork> {
    route: Vec<String>,
    pending: Vec<T>,
    incumbent_score: Option<f64>,
    closing_continuation: Option<String>,
}

fn incumbent_preserved(search: &Search<'_>, score: Option<f64>) -> bool {
    score.is_none_or(|score| {
        score.is_finite()
            && search
                .incumbent
                .as_ref()
                .is_some_and(|plan| crate::composition::score(plan) >= score)
    })
}

#[derive(Serialize, Deserialize)]
enum SavedWork {
    Route,
    Openings(Vec<u64>),
    Opening {
        source_frame: u64,
        work: Box<SavedWork>,
    },
    Endings(Vec<EndingKey>),
    Replay {
        ending: EndingKey,
        base_key: String,
        replay: ReplayCheckpoint,
    },
}

impl SavedWork {
    fn capture(work: &Work) -> Value {
        match work {
            Work::Open(work) => json!(work),
            Work::Assembled(_) => json!(Self::Route),
            Work::Openings(_, frames) => json!(Self::Openings(frames.iter().copied().collect())),
            Work::Endings(plan, endings, _) => json!(Self::at_opening(
                plan,
                Self::Endings(endings.iter().map(EndingKey::from).collect()),
            )),
            Work::PrepareReplay(base, prep) => json!(Self::at_opening(
                &base.plan,
                Self::Replay {
                    ending: base.ending,
                    base_key: base.key.clone(),
                    replay: prep.checkpoint(),
                },
            )),
            Work::Replay(base, replay) => json!(Self::at_opening(
                &base.plan,
                Self::Replay {
                    ending: base.ending,
                    base_key: base.key.clone(),
                    replay: replay.checkpoint(base.plan.placements.len()),
                },
            )),
        }
    }

    fn at_opening(plan: &MixPlan, work: Self) -> Self {
        match plan
            .header
            .extensions
            .get("selected_opening_source_frame")
            .and_then(Value::as_u64)
        {
            Some(source_frame) => Self::Opening {
                source_frame,
                work: Box::new(work),
            },
            None => work,
        }
    }
}

enum Work {
    Open(SavedWork),
    Assembled(MixPlan),
    Openings(Arc<MixPlan>, VecDeque<u64>),
    Endings(
        Arc<MixPlan>,
        VecDeque<ChainBookends>,
        Option<ReplayScoreBound>,
    ),
    PrepareReplay(FixedEnding, ReplayPreparation),
    Replay(FixedEnding, ReplayDomain),
}

struct FixedEnding {
    plan: MixPlan,
    ending: EndingKey,
    key: String,
    bound: Option<crate::composition::ReplayScoreBound>,
    opening_upper: Option<f64>,
}

impl FixedEnding {
    fn new(
        composer: &crate::composition::Context,
        plan: MixPlan,
        ending: EndingKey,
    ) -> Result<Self> {
        Ok(Self {
            key: route_key(&plan)?,
            bound: composer.replay_score_bound(&plan),
            opening_upper: None,
            plan,
            ending,
        })
    }
}

struct Route {
    indices: Vec<usize>,
    keys: Vec<String>,
    identity: String,
}

impl Route {
    fn new(indices: Vec<usize>, keys: Vec<String>) -> Result<Self> {
        let identity = canonical_hash(&keys)?.digest;
        Ok(Self {
            indices,
            keys,
            identity,
        })
    }
}

struct Job {
    route: Arc<Route>,
    work: Work,
}

#[derive(Default)]
pub(in super::super) struct ClosingSearch {
    pending: VecDeque<Job>,
    suspended: Vec<(Job, f64)>,
    seen: BTreeMap<String, Arc<Route>>,
    duplicate_routes: usize,
    unknown: BTreeSet<String>,
    checkpoint_key: Option<String>,
    dirty_routes: BTreeSet<String>,
    route_checkpoint_writes: usize,
    route_checkpoint_seconds: f64,
    route_checkpoint_overrun_seconds: f64,
    restored_route_records: usize,
    restored: usize,
    endings: usize,
    openings: usize,
    opening_rejections: usize,
    replay_candidates: usize,
    improvements: usize,
    replay_invariant_rejections: usize,
    replay_penalty_bound_pruned: usize,
    fixed_opening_bound_pruned: usize,
    candidate_rejections: BTreeMap<&'static str, usize>,
    verified_candidates: usize,
    quality_routes: BTreeMap<String, QualityClosing>,
    closing_continuations: BTreeMap<String, usize>,
    restored_continuations: usize,
}

#[derive(Clone, Serialize)]
struct QualityClosing {
    #[serde(skip)]
    anchor: usize,
    opening_track_id: String,
    closing_track_id: String,
    last_join_end_seconds: f64,
    state: &'static str,
    opening_candidates: usize,
    openings_attempted: usize,
    openings_without_duration: usize,
    openings_below_duration_floor: usize,
    short_opening_ceiling_seconds: Option<f64>,
    opening_map_rejections: usize,
    ending_candidates: usize,
    ending_attempts: usize,
    replay_candidates: usize,
    incumbent_improvements: usize,
    replay_preparation_steps: usize,
    replay_domains_ready: usize,
    replay_invariant_rejections: usize,
    terminal_development_rejections: usize,
    candidate_rejections: BTreeMap<&'static str, usize>,
    best_verified_score: Option<f64>,
    last_base_evaluation: Option<BaseEvaluation>,
}

impl QualityClosing {
    fn update_opening_state(&mut self) {
        self.state = if self.ending_candidates > 0 {
            "evaluating_endings"
        } else if self.openings_attempted < self.opening_candidates {
            "evaluating_openings"
        } else if self.openings_without_duration == self.opening_candidates {
            "no_duration_window"
        } else if self.opening_map_rejections == self.opening_candidates {
            "opening_map_rejected"
        } else {
            "no_musical_ending"
        };
    }
}

#[derive(Clone, Serialize)]
struct BaseEvaluation {
    seconds: f64,
    decision: CandidateDecision,
}

impl ClosingSearch {
    pub(super) fn no_duration_window(&self, route_id: &str) -> bool {
        self.quality_routes
            .get(route_id)
            .is_some_and(|report| report.state == "no_duration_window")
    }

    pub(super) fn continuation_hints(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.continuation_routes()
            .filter_map(|(anchor, edges)| Some((anchor, *edges.last()?)))
    }

    pub(super) fn continuation_routes(&self) -> impl Iterator<Item = (usize, &[usize])> + '_ {
        self.closing_continuations
            .iter()
            .filter_map(|(id, &anchor)| {
                if self.unknown.contains(id) {
                    return None;
                }
                Some((anchor, self.seen.get(id)?.indices.as_slice()))
            })
    }

    pub(in super::super) fn queue(&mut self, edges: &[usize], keys: Vec<String>) -> Result<()> {
        if edges.is_empty() {
            return Ok(());
        }
        let route = Arc::new(Route::new(edges.to_vec(), keys)?);
        if self.seen.contains_key(&route.identity) {
            self.duplicate_routes += 1;
        } else {
            self.dirty_routes.insert(route.identity.clone());
            self.seen.insert(route.identity.clone(), Arc::clone(&route));
            self.pending.push_back(Job {
                route,
                work: Work::Open(SavedWork::Route),
            });
        }
        Ok(())
    }

    pub(in super::super) fn queue_assembled(
        &mut self,
        edges: &[usize],
        keys: Vec<String>,
        plan: MixPlan,
        anchor: Option<usize>,
    ) -> Result<String> {
        let route = Arc::new(Route::new(edges.to_vec(), keys)?);
        let id = route.identity.clone();
        if self.seen.contains_key(&id) {
            self.duplicate_routes += 1;
            return Ok(id);
        }
        if let Some(anchor) = anchor {
            require(
                edges.contains(&anchor),
                "CP_MODEL",
                "quality anchor missing from closing route",
            )?;
            let rate = f64::from(plan.output.sample_rate_hz);
            self.quality_routes.insert(
                id.clone(),
                QualityClosing {
                    anchor,
                    opening_track_id: plan.placements.first().unwrap().track_id.clone(),
                    closing_track_id: plan.placements.last().unwrap().track_id.clone(),
                    last_join_end_seconds: plan.transitions.last().unwrap().end_output_frame as f64
                        / rate,
                    state: "queued",
                    opening_candidates: 0,
                    openings_attempted: 0,
                    openings_without_duration: 0,
                    openings_below_duration_floor: 0,
                    short_opening_ceiling_seconds: None,
                    opening_map_rejections: 0,
                    ending_candidates: 0,
                    ending_attempts: 0,
                    replay_candidates: 0,
                    incumbent_improvements: 0,
                    replay_preparation_steps: 0,
                    replay_domains_ready: 0,
                    replay_invariant_rejections: 0,
                    terminal_development_rejections: 0,
                    candidate_rejections: BTreeMap::new(),
                    best_verified_score: None,
                    last_base_evaluation: None,
                },
            );
        }
        self.seen.insert(id.clone(), Arc::clone(&route));
        self.dirty_routes.insert(id.clone());
        let job = Job {
            route,
            work: Work::Assembled(plan),
        };
        if anchor.is_some() {
            self.pending.push_front(job);
        } else {
            self.pending.push_back(job);
        }
        Ok(id)
    }

    /// These assignments are already owned by closing. This is a handoff
    /// between schedulers, including live and incumbent-pruned work.
    pub(in super::super) fn owned_routes(&self) -> impl Iterator<Item = &[usize]> {
        self.seen
            .values()
            .filter(|route| !self.unknown.contains(&route.identity))
            .map(|route| route.indices.as_slice())
    }

    fn unfinished_routes(&self) -> BTreeSet<&str> {
        self.pending
            .iter()
            .map(|job| job.route.identity.as_str())
            .chain(
                self.suspended
                    .iter()
                    .map(|(job, _)| job.route.identity.as_str()),
            )
            .collect()
    }

    pub(in super::super) fn evidence(&self) -> Value {
        let unfinished = self.unfinished_routes();
        let unresolved = unfinished
            .iter()
            .filter(|id| !self.unknown.contains(**id))
            .count();
        let quality = self
            .quality_routes
            .iter()
            .map(|(id, report)| {
                let mut report = report.clone();
                if self.unknown.contains(id) {
                    report.state = "unknown_evidence";
                } else if report.state == "evaluating_endings" && !unfinished.contains(id.as_str())
                {
                    report.state = "evaluated";
                }
                (id, report)
            })
            .collect::<BTreeMap<_, _>>();
        json!({
            "pending_jobs": self.pending.len(),
            "duplicate_route_requests": self.duplicate_routes,
            "owned_measured_routes": self.seen.len().saturating_sub(self.unknown.len()),
            "unfinished_measured_routes": unresolved,
            "unknown_measured_routes": self.unknown.len(),
            "finished_measured_routes": self.seen.len().saturating_sub(self.unknown.len() + unresolved),
            "restored_jobs": self.restored,
            "restored_route_records": self.restored_route_records,
            "route_checkpoint_writes": self.route_checkpoint_writes,
            "route_checkpoint_seconds": self.route_checkpoint_seconds,
            "route_checkpoint_overrun_seconds": self.route_checkpoint_overrun_seconds,
            "bound_pruned_jobs": self.suspended.len(),
            "pruned_closing_upper_bound": self.suspended.iter().map(|(_, upper)| *upper).reduce(f64::max),
            "bound_scope": "Exact measured joins with closing or replay choices unresolved",
            "ending_variants": self.endings,
            "opening_variants": self.openings,
            "opening_map_rejections": self.opening_rejections,
            "replay_arrangements": self.replay_candidates,
            "incumbent_improvements": self.improvements,
            "candidate_rejections": self.candidate_rejections,
            "verified_candidates": self.verified_candidates,
            "replay_invariant_rejections": self.replay_invariant_rejections,
            "replay_jobs_pruned_by_passage_penalties": self.replay_penalty_bound_pruned,
            "fixed_opening_jobs_pruned_by_tighter_bound": self.fixed_opening_bound_pruned,
            "restored_closing_continuations": self.restored_continuations,
            "quality_routes": quality,
        })
    }

    pub(in super::super) fn restore(
        &mut self,
        search: &mut Search<'_>,
        edges: &[Arc<MeasuredEdge>],
        cancel: &Cancellation,
    ) -> Result<()> {
        let key = canonical_hash(&json!({
            "profile": CHECKPOINT_PROFILE,
            "sources": search.cache.sources,
            "context": search.context,
            "set": Cache::set_key(search.policy)?,
            "composer": env!("MIXLIB_MUSICAL_RULES_HASH"),
        }))?
        .digest;
        let frontier =
            search
                .cache
                .read::<SavedFrontier>(search.store, "cp-closing-frontiers", &key)?;
        let route_kind = format!("cp-closing-routes/{key}");
        let route_ids = search.cache.keys(search.store, &route_kind)?;
        if frontier.is_none() && route_ids.is_empty() {
            self.checkpoint_key = Some(key);
            return Ok(());
        }
        let mut lookup = BTreeMap::new();
        for (i, edge) in edges.iter().enumerate() {
            cancel.check()?;
            if !search.budget_open() {
                return Ok(());
            }
            lookup.insert(measured_edge_key(edge)?, i);
        }
        let decode = |keys: Vec<String>| -> Result<Option<Arc<Route>>> {
            let Some(indices) = keys
                .iter()
                .map(|key| lookup.get(key).copied())
                .collect::<Option<Vec<_>>>()
            else {
                return Ok(None);
            };
            if indices.is_empty() {
                return Ok(None);
            }
            Ok(Some(Arc::new(Route::new(indices, keys)?)))
        };
        if let Some(frontier) = frontier {
            let incumbent_preserved = incumbent_preserved(search, frontier.incumbent_score);
            let mut restart = BTreeSet::new();
            let mut pending = Vec::new();
            for reference in frontier.pending {
                cancel.check()?;
                if !search.budget_open() {
                    return Ok(());
                }
                let work = search
                    .cache
                    .read::<SavedJob>(search.store, "cp-closing-jobs", &reference.part)?
                    .filter(|saved| saved.route == reference.route)
                    .map(|saved| saved.work);
                if work.is_none() {
                    restart.insert(reference.route.clone());
                }
                pending.push((reference.route, work));
            }
            for (keys, work) in pending {
                cancel.check()?;
                if !search.budget_open() {
                    return Ok(());
                }
                let restart_route = !incumbent_preserved || restart.contains(&keys);
                let Some(route) = decode(keys)? else {
                    continue;
                };
                let first = !self.seen.contains_key(&route.identity);
                if restart_route && !first {
                    continue;
                }
                self.seen.insert(route.identity.clone(), Arc::clone(&route));
                self.pending.push_back(Job {
                    route,
                    work: Work::Open(if restart_route {
                        SavedWork::Route
                    } else {
                        work.expect("all parts of this route were verified")
                    }),
                });
                self.restored += 1;
            }
            for part in frontier.completed_parts {
                cancel.check()?;
                if !search.budget_open() {
                    return Ok(());
                }
                let Some(routes) = search.cache.read::<Vec<Vec<String>>>(
                    search.store,
                    "refinement-frontier-parts",
                    &part,
                )?
                else {
                    continue;
                };
                for keys in routes {
                    cancel.check()?;
                    if !search.budget_open() {
                        return Ok(());
                    }
                    let Some(route) = decode(keys)? else {
                        continue;
                    };
                    if self.seen.contains_key(&route.identity) {
                        continue;
                    }
                    self.seen.insert(route.identity.clone(), Arc::clone(&route));
                    if !incumbent_preserved {
                        self.pending.push_back(Job {
                            route,
                            work: Work::Open(SavedWork::Route),
                        });
                        self.restored += 1;
                    }
                }
            }
            for (id, anchor) in frontier.closing_continuations {
                cancel.check()?;
                if !search.budget_open() {
                    return Ok(());
                }
                if let Some(&anchor) = lookup.get(&anchor) {
                    if self
                        .seen
                        .get(&id)
                        .is_some_and(|route| route.indices.contains(&anchor))
                    {
                        self.closing_continuations.insert(id, anchor);
                        self.restored_continuations += 1;
                    }
                }
            }
        }
        let mut overlay = BTreeSet::new();
        let mut overlay_jobs = Vec::new();
        for id in route_ids {
            cancel.check()?;
            if !search.budget_open() {
                break;
            }
            let Some(saved) = search
                .cache
                .read::<SavedRoute>(search.store, &route_kind, &id)?
            else {
                continue;
            };
            let Some(route) = decode(saved.route)?.filter(|route| route.identity == id) else {
                continue;
            };
            overlay.insert(id.clone());
            self.seen.insert(id.clone(), Arc::clone(&route));
            if self.closing_continuations.remove(&id).is_some() {
                self.restored_continuations -= 1;
            }
            let pending = if incumbent_preserved(search, saved.incumbent_score) {
                if saved.pending.is_empty() {
                    if let Some(anchor) = saved
                        .closing_continuation
                        .as_ref()
                        .and_then(|key| lookup.get(key))
                        .copied()
                        .filter(|anchor| route.indices.contains(anchor))
                    {
                        self.closing_continuations.insert(id, anchor);
                        self.restored_continuations += 1;
                    }
                }
                saved.pending
            } else {
                vec![SavedWork::Route]
            };
            self.restored_route_records += 1;
            overlay_jobs.extend(pending.into_iter().map(|work| Job {
                route: Arc::clone(&route),
                work: Work::Open(work),
            }));
        }
        let before = self.pending.len();
        self.pending
            .retain(|job| !overlay.contains(&job.route.identity));
        self.restored =
            self.restored.saturating_sub(before - self.pending.len()) + overlay_jobs.len();
        self.pending.extend(overlay_jobs);
        if search.budget_open() {
            self.checkpoint_key = Some(key);
        }
        Ok(())
    }

    /// Write only routes changed by the last bounded closing slice or admission.
    /// Each record replaces all siblings atomically, including completed routes.
    pub(in super::super) fn checkpoint_routes(&mut self, search: &Search<'_>) -> Result<()> {
        let Some(key) = &self.checkpoint_key else {
            return Ok(());
        };
        if self.dirty_routes.is_empty() {
            return Ok(());
        }
        let started = Instant::now();
        let kind = format!("cp-closing-routes/{key}");
        let mut by_route: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for job in self
            .pending
            .iter()
            .chain(self.suspended.iter().map(|(job, _)| job))
        {
            let id = &job.route.identity;
            if self.dirty_routes.contains(id) && !self.unknown.contains(id) {
                by_route
                    .entry(id.as_str())
                    .or_default()
                    .push(SavedWork::capture(&job.work));
            }
        }
        let incumbent_score = search.incumbent.as_ref().map(crate::composition::score);
        while let Some(id) = self.dirty_routes.first().cloned() {
            let route = &self.seen[&id];
            let pending = if self.unknown.contains(&id) {
                vec![json!(SavedWork::Route)]
            } else {
                by_route.remove(id.as_str()).unwrap_or_default()
            };
            let continuation = pending
                .is_empty()
                .then(|| {
                    let anchor = self.closing_continuations.get(&id)?;
                    route
                        .indices
                        .iter()
                        .position(|index| index == anchor)
                        .map(|i| &route.keys[i])
                })
                .flatten();
            search.cache.write(
                search.store,
                &kind,
                &id,
                &SavedRoute {
                    route: route.keys.clone(),
                    pending,
                    incumbent_score,
                    closing_continuation: continuation.cloned(),
                },
            )?;
            self.dirty_routes.remove(&id);
            self.route_checkpoint_writes += 1;
        }
        let finished = Instant::now();
        self.route_checkpoint_seconds += finished.duration_since(started).as_secs_f64();
        if let Some(deadline) = search.deadline {
            self.route_checkpoint_overrun_seconds += finished
                .saturating_duration_since(deadline.max(started))
                .as_secs_f64();
        }
        Ok(())
    }

    #[cfg(test)]
    pub(in super::super) fn save(&self, search: &Search<'_>) -> Result<()> {
        let Some(key) = &self.checkpoint_key else {
            return Ok(());
        };
        let mut parts = Vec::with_capacity(self.pending.len());
        let cache = &search.cache;
        let store = search.store;
        let unfinished = || {
            self.pending
                .iter()
                .chain(self.suspended.iter().map(|(job, _)| job))
        };
        // Restart an unknown route rather than checkpointing a cursor that
        // has advanced past the variant with incomplete evidence.
        let restart = self
            .unknown
            .iter()
            .filter_map(|id| self.seen.get(id))
            .map(|route| Job {
                route: Arc::clone(route),
                work: Work::Open(SavedWork::Route),
            })
            .collect::<Vec<_>>();
        for job in unfinished()
            .filter(|job| !self.unknown.contains(&job.route.identity))
            .chain(restart.iter())
        {
            if !search.budget_open() {
                return Ok(());
            }
            let payload = json!({ "route": job.route.keys, "work": SavedWork::capture(&job.work) });
            let part = canonical_hash(&payload)?.digest;
            cache.write_immutable(store, "cp-closing-jobs", &part, &payload)?;
            parts.push(SavedPending {
                part,
                route: job.route.keys.clone(),
            });
        }
        let pending = self.unfinished_routes();
        let closing_continuations = self
            .closing_continuations
            .iter()
            .filter_map(|(id, &anchor)| {
                if pending.contains(id.as_str()) || self.unknown.contains(id) {
                    return None;
                }
                let route = self.seen.get(id)?;
                let index = route.indices.iter().position(|&edge| edge == anchor)?;
                Some((id.clone(), route.keys[index].clone()))
            })
            .collect();
        let completed = self
            .seen
            .values()
            .filter(|route| {
                !pending.contains(route.identity.as_str())
                    && !self.unknown.contains(&route.identity)
            })
            .map(|route| json!(route.keys))
            .collect::<Vec<_>>();
        let Some(completed_parts) = frontier_parts(cache, store, completed, search.deadline)?
        else {
            return Ok(());
        };
        if !search.budget_open() {
            return Ok(());
        }
        cache.write(
            store,
            "cp-closing-frontiers",
            key,
            &SavedFrontier {
                pending: parts,
                completed_parts,
                incumbent_score: search.incumbent.as_ref().map(crate::composition::score),
                closing_continuations,
            },
        )
    }

    fn suspend_opening(&mut self, r: &Refiner<'_>, bound: Option<ReplayScoreBound>) -> Option<f64> {
        let incumbent = crate::composition::score(r.search.incumbent.as_ref()?);
        let bound = bound?;
        if !incumbent.is_finite() || bound.upper > incumbent {
            return None;
        }
        self.fixed_opening_bound_pruned += usize::from(bound.without_passage_penalties > incumbent);
        Some(bound.upper)
    }

    fn record_decision(
        &mut self,
        route: &Route,
        decision: CandidateDecision,
        base_seconds: Option<f64>,
    ) {
        let report = self.quality_routes.get_mut(&route.identity);
        match &decision {
            CandidateDecision::Rejected { reason, .. } => {
                *self.candidate_rejections.entry(reason).or_default() += 1;
                if *reason == "invalid_evidence" {
                    self.unknown.insert(route.identity.clone());
                }
            }
            CandidateDecision::Verified { improved, .. } => {
                self.verified_candidates += 1;
                self.improvements += usize::from(*improved);
            }
        }
        if let Some(report) = report {
            match &decision {
                CandidateDecision::Rejected { reason, .. } => {
                    *report.candidate_rejections.entry(reason).or_default() += 1;
                }
                CandidateDecision::Verified { score, improved } => {
                    report.best_verified_score = Some(
                        report
                            .best_verified_score
                            .map_or(*score, |old| old.max(*score)),
                    );
                    report.incumbent_improvements += usize::from(*improved);
                }
            }
            if let Some(seconds) = base_seconds {
                if matches!(
                    &decision,
                    CandidateDecision::Rejected {
                        reason: "insufficient_development",
                        development: Some(detail),
                    } if detail.track_id == report.closing_track_id
                        && detail.position == route.indices.len() + 1
                ) {
                    report.terminal_development_rejections += 1;
                }
                report.last_base_evaluation = Some(BaseEvaluation { seconds, decision });
            }
        }
        self.maybe_record_terminal_hint(route);
    }

    fn maybe_record_terminal_hint(&mut self, route: &Route) {
        if let Some(report) = self.quality_routes.get(&route.identity) {
            let too_short = report.opening_candidates > 0
                && report.openings_below_duration_floor == report.opening_candidates;
            let lacks_development = report.ending_candidates > 0
                && report.terminal_development_rejections == report.ending_candidates;
            if report.openings_attempted == report.opening_candidates
                && (too_short || lacks_development)
                && !self.unknown.contains(&route.identity)
                && !self
                    .suspended
                    .iter()
                    .any(|(job, _)| job.route.identity == route.identity)
            {
                self.closing_continuations
                    .insert(route.identity.clone(), report.anchor);
            }
        }
    }

    fn open(
        &mut self,
        r: &Refiner<'_>,
        route: Arc<Route>,
        work: SavedWork,
        cancel: &Cancellation,
    ) -> Result<Option<Work>> {
        let plan = match r.assemble_measured_for_closing(&route.indices)? {
            Ok((_, plan)) => plan,
            Err(AssemblyRejection::Prefix { .. }) => return Ok(None),
            Err(AssemblyRejection::Unknown(_)) => {
                self.unknown.insert(route.identity.clone());
                return Ok(None);
            }
        };
        let choices = r.search.composer.opening_choices(&plan);
        match work {
            SavedWork::Opening { source_frame, work }
                if choices.contains(&source_frame)
                    && matches!(
                        *work,
                        SavedWork::Route | SavedWork::Endings(_) | SavedWork::Replay { .. }
                    ) =>
            {
                self.open_candidate(r, route, plan, source_frame, *work, cancel)
            }
            SavedWork::Openings(remaining)
                if !remaining.is_empty() && choices.ends_with(&remaining) =>
            {
                Ok(Some(self.openings_work(&route, plan, remaining)))
            }
            _ => Ok(Some(self.openings_work(&route, plan, choices))),
        }
    }

    fn openings_work(&mut self, route: &Route, plan: MixPlan, choices: Vec<u64>) -> Work {
        if let Some(report) = self.quality_routes.get_mut(&route.identity) {
            report.opening_candidates = choices.len();
            report.state = "evaluating_openings";
        }
        Work::Openings(Arc::new(plan), choices.into())
    }

    fn open_candidate(
        &mut self,
        r: &Refiner<'_>,
        route: Arc<Route>,
        mut plan: MixPlan,
        frame: u64,
        work: SavedWork,
        cancel: &Cancellation,
    ) -> Result<Option<Work>> {
        self.openings += 1;
        if let Some(report) = self.quality_routes.get_mut(&route.identity) {
            report.openings_attempted += 1;
        }
        match chain::open_measured(&mut plan, frame) {
            Ok(()) => self.open_assembled(r, route, plan, work, cancel),
            Err(error) if measured_beam::route_local(&error) => {
                self.opening_rejections += 1;
                if let Some(report) = self.quality_routes.get_mut(&route.identity) {
                    report.opening_map_rejections += 1;
                    report.update_opening_state();
                }
                self.maybe_record_terminal_hint(&route);
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn open_assembled(
        &mut self,
        r: &Refiner<'_>,
        route: Arc<Route>,
        plan: MixPlan,
        work: SavedWork,
        cancel: &Cancellation,
    ) -> Result<Option<Work>> {
        if !r.search.required.satisfied(&plan) {
            if let Some(report) = self.quality_routes.get_mut(&route.identity) {
                report.state = "required_pairs";
            }
            return Ok(None);
        }
        let latest_base = r.search.composer.latest_ending_output_frame(&plan);
        let rate = f64::from(plan.output.sample_rate_hz);
        if let Some(maximum) = latest_base
            .map(|base| base.saturating_add((base as f64 * MAX_REPLAY_FRACTION).floor() as u64))
            .filter(|&max| max < (r.search.request.min() * rate).ceil() as u64)
        {
            if let Some(report) = self.quality_routes.get_mut(&route.identity) {
                let seconds = maximum as f64 / rate;
                report.short_opening_ceiling_seconds = Some(
                    report
                        .short_opening_ceiling_seconds
                        .map_or(seconds, |old| old.max(seconds)),
                );
                report.openings_below_duration_floor += 1;
                report.openings_without_duration += 1;
                report.last_join_end_seconds =
                    plan.transitions.last().unwrap().end_output_frame as f64 / rate;
                report.update_opening_state();
            }
            self.maybe_record_terminal_hint(&route);
            return Ok(None);
        }
        let opening_bound = r
            .search
            .composer
            .fixed_opening_score_bound(&plan, latest_base);
        if let Some(upper) = self.suspend_opening(r, opening_bound) {
            if let Some(report) = self.quality_routes.get_mut(&route.identity) {
                report.state = "incumbent_bound";
            }
            self.suspended.push((
                Job {
                    route,
                    work: Work::Open(SavedWork::at_opening(&plan, work)),
                },
                upper,
            ));
            return Ok(None);
        }
        let (window, mut endings) =
            r.search
                .composer
                .ending_choices(&r.search.snapshot, &plan, cancel)?;
        if let Some(report) = self.quality_routes.get_mut(&route.identity) {
            report.ending_candidates += endings.len();
            report.openings_without_duration += usize::from(window.is_none());
            if report.openings_attempted == 1 {
                report.last_join_end_seconds = plan.transitions.last().unwrap().end_output_frame
                    as f64
                    / f64::from(plan.output.sample_rate_hz);
            }
            report.update_opening_state();
        }
        self.maybe_record_terminal_hint(&route);
        match work {
            SavedWork::Route => {}
            SavedWork::Opening { .. } | SavedWork::Openings(_) => {
                unreachable!("opening cursors are decoded before closing")
            }
            SavedWork::Endings(remaining) => {
                if remaining
                    .iter()
                    .all(|key| endings.iter().any(|e| EndingKey::from(e) == *key))
                {
                    endings.retain(|e| remaining.contains(&EndingKey::from(e)));
                }
            }
            SavedWork::Replay {
                ending,
                base_key,
                replay,
            } => {
                if let Some(index) = endings.iter().position(|e| EndingKey::from(e) == ending) {
                    let finished =
                        chain::finish_measured_at_bookends(plan.clone(), endings.remove(index));
                    match finished {
                        Ok(base) => {
                            let mut fixed = FixedEnding::new(&r.search.composer, base, ending)?;
                            fixed.opening_upper = opening_bound.map(|bound| bound.upper);
                            let mut prep = r.search.composer.prepare_replay(&fixed.plan);
                            if fixed.key == base_key {
                                prep.restore(&fixed.plan, replay);
                            }
                            return Ok(Some(Work::PrepareReplay(fixed, prep)));
                        }
                        Err(e) if measured_beam::route_local(&e) => {}
                        Err(e) => return Err(e),
                    }
                }
            }
        }
        Ok((!endings.is_empty())
            .then(|| Work::Endings(Arc::new(plan), endings.into(), opening_bound)))
    }

    pub(in super::super) fn slice(
        &mut self,
        r: &mut Refiner<'_>,
        deadline: Instant,
        cancel: &Cancellation,
    ) -> Result<bool> {
        let mut in_flight = None;
        let result = self.advance_slice(r, deadline, cancel, &mut in_flight);
        if result.is_err() {
            if let Some(route) = in_flight {
                // The popped job was not committed. Keep its durable cursor
                // and report the interrupted route as unresolved.
                self.unknown.insert(route);
            }
        }
        result
    }

    fn advance_slice(
        &mut self,
        r: &mut Refiner<'_>,
        deadline: Instant,
        cancel: &Cancellation,
        in_flight: &mut Option<String>,
    ) -> Result<bool> {
        for _ in 0..8 {
            *in_flight = None;
            if Instant::now() >= deadline {
                break;
            }
            cancel.check()?;
            let Some(Job { route, work }) = self.pending.pop_front() else {
                return Ok(false);
            };
            *in_flight = Some(route.identity.clone());
            self.dirty_routes.insert(route.identity.clone());
            let upper = match &work {
                Work::Open(_) | Work::Assembled(_) | Work::Openings(..) => None,
                Work::Endings(_, _, bound) => self.suspend_opening(r, *bound),
                Work::PrepareReplay(base, _) | Work::Replay(base, _) => {
                    base.bound.and_then(|bound| {
                        let incumbent = crate::composition::score(r.search.incumbent.as_ref()?);
                        if !incumbent.is_finite() || bound.upper > incumbent {
                            return None;
                        }
                        self.replay_penalty_bound_pruned +=
                            usize::from(bound.without_passage_penalties > incumbent);
                        Some(bound.upper)
                    })
                }
            };
            if let Some(upper) = upper {
                self.suspended.push((Job { route, work }, upper));
                continue;
            }
            let quality = self.quality_routes.get(&route.identity);
            let prioritize_next = quality.is_some_and(|report| {
                matches!(&work, Work::Assembled(_))
                    || (matches!(&work, Work::Openings(..)) && report.openings_attempted == 0)
                    || (matches!(&work, Work::Endings(..)) && report.ending_attempts == 0)
            });
            let next = match work {
                Work::Open(saved) => self.open(r, Arc::clone(&route), saved, cancel)?,
                Work::Assembled(plan) => {
                    let choices = r.search.composer.opening_choices(&plan);
                    Some(self.openings_work(&route, plan, choices))
                }
                Work::Openings(plan, mut openings) => {
                    let Some(opening) = openings.pop_front() else {
                        continue;
                    };
                    let next = self.open_candidate(
                        r,
                        Arc::clone(&route),
                        (*plan).clone(),
                        opening,
                        SavedWork::Route,
                        cancel,
                    )?;
                    if !openings.is_empty() {
                        self.pending.push_back(Job {
                            route: Arc::clone(&route),
                            work: Work::Openings(plan, openings),
                        });
                    }
                    next
                }
                Work::Endings(plan, mut endings, opening_bound) => {
                    let Some(ending) = endings.pop_front() else {
                        continue;
                    };
                    self.endings += 1;
                    if let Some(report) = self.quality_routes.get_mut(&route.identity) {
                        report.ending_attempts += 1;
                    }
                    let ending_key = EndingKey::from(&ending);
                    let base = chain::finish_measured_at_bookends((*plan).clone(), ending);
                    if !endings.is_empty() {
                        self.pending.push_back(Job {
                            route: Arc::clone(&route),
                            work: Work::Endings(plan, endings, opening_bound),
                        });
                    }
                    let base = match base {
                        Ok(plan) => plan,
                        Err(e) if measured_beam::route_local(&e) => continue,
                        Err(e) => return Err(e),
                    };
                    let decision =
                        r.search
                            .save_candidate(base.clone(), "cp_musical_ending", true, cancel)?;
                    if let CandidateDecision::Verified { score, .. } = decision {
                        require(
                            opening_bound.is_none_or(|bound| score <= bound.upper),
                            "CP_OPENING_BOUND",
                            "verified ending exceeded its fixed-opening score ceiling",
                        )?;
                    }
                    let seconds =
                        base.total_output_frames as f64 / f64::from(base.output.sample_rate_hz);
                    // Replays translate the narrative clock but keep every
                    // original solo range, its development and overlap credit.
                    let development_failed = matches!(
                        decision,
                        CandidateDecision::Rejected {
                            reason: "no_solo_runway" | "insufficient_development",
                            ..
                        }
                    );
                    self.record_decision(&route, decision, Some(seconds));
                    if development_failed {
                        self.replay_invariant_rejections += 1;
                        if let Some(report) = self.quality_routes.get_mut(&route.identity) {
                            report.replay_invariant_rejections += 1;
                        }
                        continue;
                    }
                    let prep = r.search.composer.prepare_replay(&base);
                    let mut fixed = FixedEnding::new(&r.search.composer, base, ending_key)?;
                    fixed.opening_upper = opening_bound.map(|bound| bound.upper);
                    Some(Work::PrepareReplay(fixed, prep))
                }
                Work::PrepareReplay(base, mut prep) => {
                    let mut ready = None;
                    let steps = if quality.is_some() { 4 } else { 1 };
                    for _ in 0..steps {
                        if Instant::now() >= deadline {
                            break;
                        }
                        ready = r.search.composer.advance_replay(
                            &r.search.snapshot,
                            &base.plan,
                            &mut prep,
                            cancel,
                        )?;
                        if let Some(report) = self.quality_routes.get_mut(&route.identity) {
                            report.replay_preparation_steps += 1;
                            report.replay_domains_ready += usize::from(ready.is_some());
                        }
                        if ready.is_some() {
                            break;
                        }
                    }
                    Some(match ready {
                        Some(replay) => Work::Replay(base, replay),
                        None => Work::PrepareReplay(base, prep),
                    })
                }
                Work::Replay(base, mut replay) => {
                    match replay.clock.advance(128) {
                        Advance::Exhausted => continue,
                        Advance::Candidate(choices) => {
                            let arranged = replay.plan(&base.plan, &choices)?;
                            if arranged.total_output_frames != base.plan.total_output_frames {
                                self.replay_candidates += 1;
                                if let Some(report) = self.quality_routes.get_mut(&route.identity) {
                                    report.replay_candidates += 1;
                                }
                                let decision = r.search.save_candidate(
                                    arranged,
                                    "cp_exact_replay",
                                    false,
                                    cancel,
                                )?;
                                if let CandidateDecision::Verified { score, .. } = decision {
                                    require(
                                        base.opening_upper.is_none_or(|upper| score <= upper),
                                        "CP_OPENING_BOUND",
                                        "verified replay exceeded its fixed-opening score ceiling",
                                    )?;
                                    require(
                                        base.bound.is_none_or(|bound| score <= bound.upper),
                                        "CP_REPLAY_BOUND",
                                        "verified replay exceeded its fixed-ending score ceiling",
                                    )?;
                                }
                                self.record_decision(&route, decision, None);
                            }
                        }
                        Advance::Pending => {}
                    }
                    Some(Work::Replay(base, replay))
                }
            };
            if let Some(work) = next {
                if prioritize_next {
                    self.pending.push_front(Job { route, work });
                } else {
                    self.pending.push_back(Job { route, work });
                }
            }
        }
        Ok(!self.pending.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_openings_change_duration_without_moving_measured_source_cues() {
        use crate::duration_search::complete_refinement::tests::fixture;
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let r = fixture(&store, &policy, &[3600; 3]);
        let mut plan = super::super::tests::bookend_fixture_plan(&r);
        let rate = u64::from(plan.output.sample_rate_hz);
        // Scale the fixture's maps to 56:56 without changing their speeds.
        // Ten extra seconds from the first song reaches 57:06.
        let scale = 61;
        for p in &mut plan.placements {
            for anchor in &mut p.time_map.anchors {
                anchor.output_frame *= scale;
                anchor.source_frame *= scale;
            }
            p.source_start_frame *= scale;
            p.source_end_frame *= scale;
            for point in &mut p.automation {
                point.output_frame *= scale;
            }
        }
        let first = &mut plan.placements[0];
        for anchor in &mut first.time_map.anchors {
            anchor.source_frame += 10 * rate;
        }
        first.source_start_frame += 10 * rate;
        first.source_end_frame += 10 * rate;
        let mut joins = Vec::new();
        for t in &mut plan.transitions {
            t.start_output_frame *= scale;
            t.end_output_frame *= scale;
            joins.push(json!({"start_output_frame":t.start_output_frame,"pair_output_offset":0}));
        }
        plan.header
            .extensions
            .insert("chain_joins".into(), json!(joins));
        plan.total_output_frames *= scale;
        let mut later = plan.clone();
        chain::open_measured(&mut later, 10 * rate).unwrap();
        assert_eq!(
            r.search
                .composer
                .evaluate_for_save(&later, false)
                .unwrap_err()
                .reason,
            "duration_outside_window"
        );
        chain::open_measured(&mut plan, 0).unwrap();
        let verified = r.search.composer.evaluate_for_save(&plan, false).unwrap();
        assert_eq!(verified["duration_seconds"], 3426.);
        assert_eq!(
            plan.transitions[0].start_output_frame,
            later.transitions[0].start_output_frame + 10 * rate
        );
        let source_cue = |p: &MixPlan| {
            p.placements[0]
                .time_map
                .source_at(p.transitions[0].start_output_frame as f64)
                .round() as u64
        };
        assert_eq!(source_cue(&plan), source_cue(&later));
    }

    #[test]
    fn pending_openings_and_selected_ending_resume_as_separate_work() {
        use crate::duration_search::complete_refinement::tests::{edge, fixture};
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[1200; 3]);
        let id = edge(&mut r, 0, 1, 55., 10.);
        let cancel = Cancellation::default();
        let mut search = ClosingSearch::default();
        search.restore(&mut r.search, &r.edges, &cancel).unwrap();
        search
            .queue(&[id], r.closing_route_keys(&[id]).unwrap())
            .unwrap();
        let route = Arc::clone(&search.pending[0].route);
        let raw = super::super::tests::bookend_fixture_plan(&r);
        search.pending[0].work = Work::Openings(Arc::new(raw.clone()), [0, 20].into());
        let mut opened = raw;
        opened
            .header
            .extensions
            .insert("selected_opening_source_frame".into(), json!(40));
        search.pending.push_back(Job {
            route,
            work: Work::Endings(
                Arc::new(opened),
                [ChainBookends {
                    start: 40,
                    end: 3000,
                    outro_fade_frames: 100,
                    ending: "phrase_fade",
                }]
                .into(),
                None,
            ),
        });
        search.save(&r.search).unwrap();
        let mut resumed = ClosingSearch::default();
        resumed.restore(&mut r.search, &r.edges, &cancel).unwrap();
        assert_eq!(resumed.pending.len(), 2);
        assert!(
            matches!(&resumed.pending[0].work, Work::Open(SavedWork::Openings(frames)) if frames == &[0, 20])
        );
        assert!(matches!(&resumed.pending[1].work,
            Work::Open(SavedWork::Opening { source_frame: 40, work })
                if matches!(&**work, SavedWork::Endings(ends) if ends.len() == 1 && ends[0].end == 3000)
        ));
    }

    #[test]
    fn route_checkpoints_survive_the_deadline_and_replace_all_siblings_together() {
        use crate::duration_search::complete_refinement::tests::{edge, fixture};

        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[1200; 3]);
        let first = edge(&mut r, 0, 1, 55., 10.);
        let second = edge(&mut r, 1, 2, 56., 11.);
        let mut incumbent = r.edges[first].plan.clone();
        incumbent
            .header
            .extensions
            .insert("set_composition".into(), json!({"objective": 2.0}));
        r.search.incumbent = Some(incumbent);
        let cancel = Cancellation::default();
        let mut closing = ClosingSearch::default();
        closing.restore(&mut r.search, &r.edges, &cancel).unwrap();
        let first_keys = r.closing_route_keys(&[first]).unwrap();
        let second_keys = r.closing_route_keys(&[second]).unwrap();
        closing.queue(&[first], first_keys.clone()).unwrap();
        closing.queue(&[second], second_keys.clone()).unwrap();
        closing.checkpoint_routes(&r.search).unwrap();
        assert_eq!(closing.route_checkpoint_writes, 2);
        let route = Arc::clone(&closing.pending[0].route);
        closing.pending[0].work = Work::Open(SavedWork::Openings(vec![50, 60]));
        closing.pending.push_back(Job {
            route: Arc::clone(&route),
            work: Work::Open(SavedWork::Opening {
                source_frame: 40,
                work: Box::new(SavedWork::Endings(vec![EndingKey {
                    end: 3000,
                    fade: 100,
                    natural: false,
                }])),
            }),
        });
        closing.dirty_routes.insert(route.identity.clone());
        r.search.deadline = Some(Instant::now());
        closing.checkpoint_routes(&r.search).unwrap();
        assert_eq!(closing.route_checkpoint_writes, 3);
        assert!(closing.route_checkpoint_overrun_seconds > 0.);
        let bulk = r.search.cache.keys(&store, "cp-closing-frontiers").unwrap();
        assert!(bulk.is_empty());

        r.search.deadline = None;
        r.edges.swap(first, second);
        let mut resumed = ClosingSearch::default();
        resumed.restore(&mut r.search, &r.edges, &cancel).unwrap();
        assert_eq!(resumed.restored_route_records, 2);
        let jobs = resumed
            .pending
            .iter()
            .filter(|job| job.route.keys == first_keys)
            .collect::<Vec<_>>();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].route.indices, [second]);
        assert!(
            matches!(&jobs[0].work, Work::Open(SavedWork::Openings(frames)) if frames == &[50, 60])
        );
        assert!(
            matches!(&jobs[1].work, Work::Open(SavedWork::Opening { source_frame: 40, work }) if matches!(&**work, SavedWork::Endings(ends) if ends[0].end == 3000))
        );
        assert!(resumed
            .pending
            .iter()
            .any(|job| job.route.keys == second_keys
                && matches!(job.work, Work::Open(SavedWork::Route))));

        resumed
            .pending
            .retain(|job| job.route.identity != route.identity);
        resumed.dirty_routes.insert(route.identity.clone());
        resumed.checkpoint_routes(&r.search).unwrap();
        let mut completed = ClosingSearch::default();
        completed.restore(&mut r.search, &r.edges, &cancel).unwrap();
        assert_eq!(completed.pending.len(), 1);
        assert!(completed.seen.contains_key(&route.identity));

        r.search.incumbent = None;
        let mut reopened = ClosingSearch::default();
        reopened.restore(&mut r.search, &r.edges, &cancel).unwrap();
        assert_eq!(reopened.pending.len(), 2);
        assert!(reopened
            .pending
            .iter()
            .all(|job| matches!(job.work, Work::Open(SavedWork::Route))));
    }

    #[test]
    fn rejected_closing_cue_keeps_the_mashup_and_opens_a_successor() {
        use super::super::tests::clock_edge;
        use crate::duration_search::complete_refinement::tests::fixture;

        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[1800, 1600, 600, 600]);
        let lead = clock_edge(&mut r, 0, 1, 1780, 0);
        let anchor = clock_edge(&mut r, 1, 2, 1550, 0);
        let successor = clock_edge(&mut r, 2, 3, 90, 0);
        Arc::make_mut(&mut r.edges[anchor]).dense_quality_seconds = 18.;
        Arc::make_mut(&mut r.edges[successor]).score = 0.;
        let cancel = Cancellation::default();
        let mut cp = CpSearch::default();
        cp.closing
            .restore(&mut r.search, &r.edges, &cancel)
            .unwrap();
        let initial = ProposalDomain::AroundMeasured {
            edge: anchor,
            allow_unresolved: false,
        };
        let cutoff = || Instant::now() + Duration::from_secs(2);
        assert_eq!(
            cp.propose(&r, initial, cutoff(), &cancel).unwrap(),
            Some((vec![0, 1, 2], vec![Some(lead), Some(anchor)]))
        );

        // This fixture supplies two measured endings directly. Feed the
        // production composer's decisions to the closing report.
        let mut base = r.edges[anchor].plan.clone();
        base.placements
            .insert(0, r.edges[lead].plan.placements[0].clone());
        let rate = u64::from(base.output.sample_rate_hz);
        for placement in &mut base.placements[1..] {
            for anchor in &mut placement.time_map.anchors {
                anchor.output_frame += 1800 * rate;
            }
        }
        let mut previous = r.edges[lead].plan.transitions[0].clone();
        previous.start_output_frame = 1780 * rate;
        previous.end_output_frame = 1800 * rate;
        base.transitions.truncate(1);
        base.transitions[0].start_output_frame = 3350 * rate;
        base.transitions[0].end_output_frame = 3370 * rate;
        base.transitions.insert(0, previous);
        let route_id = cp
            .closing
            .queue_assembled(
                &[lead, anchor],
                r.closing_route_keys(&[lead, anchor]).unwrap(),
                base.clone(),
                Some(anchor),
            )
            .unwrap();
        cp.note_quality_verification(
            initial,
            QualityVerification::QueuedForClosing {
                route_id: route_id.clone(),
            },
        );
        let route = Arc::clone(&cp.closing.pending.pop_front().unwrap().route);
        let report = cp.closing.quality_routes.get_mut(&route_id).unwrap();
        report.ending_candidates = 2;
        report.state = "evaluating_endings";
        for (i, seconds) in [3412, 3422].into_iter().enumerate() {
            base.total_output_frames = seconds * rate;
            let decision = r
                .search
                .save_candidate(base.clone(), "mock_ending", true, &cancel)
                .unwrap();
            assert!(
                matches!(&decision, CandidateDecision::Rejected {
                    reason: "insufficient_development",
                    development: Some(d),
                } if d.position == 3),
                "{}",
                json!(decision)
            );
            cp.closing
                .record_decision(&route, decision, Some(seconds as f64));
            assert_eq!(
                cp.closing.continuation_hints().next(),
                (i == 1).then_some((anchor, anchor))
            );
        }
        let retry = cp.next_proposal(&r);
        assert!(
            retry
                == ProposalDomain::ContinueAfterClosingRejection {
                    edge: anchor,
                    terminal_join: anchor,
                    allow_unresolved: false,
                }
        );
        cp.note_retry(retry, r.edges.len());
        assert_eq!(
            cp.propose(&r, retry, cutoff(), &cancel).unwrap(),
            Some((
                vec![0, 1, 2, 3],
                vec![Some(lead), Some(anchor), Some(successor)]
            ))
        );
        assert_eq!(
            cp.quality_proposals
                .last()
                .unwrap()
                .continuation_after_rejected_ending
                .as_ref()
                .unwrap()
                .to_track_id,
            r.search.pool[2].track_id
        );
        let mixed = cp.next_proposal(&r);
        assert!(
            mixed
                == ProposalDomain::ContinueAfterClosingRejection {
                    edge: anchor,
                    terminal_join: anchor,
                    allow_unresolved: true,
                }
        );
        cp.note_retry(mixed, r.edges.len());
        assert!(cp.next_proposal(&r) != retry && cp.next_proposal(&r) != mixed);

        // The scheduling row must not eliminate the original musical cue
        // from the ordinary proposal model.
        assert_eq!(
            super::super::tests::mock_proposal(&r, initial),
            Some((vec![0, 1, 2], vec![Some(lead), Some(anchor)]))
        );

        let targets = cp.closing_probes(&r);
        assert_eq!(
            targets[0],
            Probe {
                from: 2,
                to: 3,
                after: 65 * rate,
                tier: 0
            }
        );
        assert!(targets
            .iter()
            .all(|probe| probe.from == 2 && probe.to == 3 && probe.after == 65 * rate));
        assert_eq!(targets.len(), 3);
        assert_eq!(
            targets.iter().map(|probe| probe.tier).collect::<Vec<_>>(),
            [0, 1, 24]
        );
        let mut discovery = Schedule::new(0);
        let frames = r
            .search
            .pool
            .iter()
            .map(|track| track.format.frames)
            .collect::<Vec<_>>();
        assert_eq!(
            discovery.next_targeted(&targets, &frames, &[0; 4]),
            Some(targets[0])
        );
        assert_eq!(discovery.pool_cursor, 0);
        assert_eq!(
            discovery.next_targeted(&targets, &frames, &[0; 4]),
            Some(targets[1])
        );
        assert!(cp.matches_closing_window(&r, targets[0], &r.edges[successor].plan));
        assert!(!cp.matches_closing_window(
            &r,
            Probe {
                to: 0,
                ..targets[0]
            },
            &r.edges[successor].plan
        ));

        // New measured windows only reopen the retry if their admitted join
        // can follow the pinned arrival. An early cue does not reopen it.
        let early = clock_edge(&mut r, 2, 3, 55, 0);
        assert!(!cp.matches_closing_window(&r, targets[0], &r.edges[early].plan));
        assert!(cp.next_proposal(&r) != retry);
        clock_edge(&mut r, 2, 3, 100, 0);
        assert!(cp.next_proposal(&r) == retry);

        cp.closing.save(&r.search).unwrap();
        r.edges.swap(anchor, successor);
        let mut resumed = CpSearch::default();
        resumed
            .closing
            .restore(&mut r.search, &r.edges, &cancel)
            .unwrap();
        assert_eq!(
            resumed.closing.evidence()["restored_closing_continuations"],
            1
        );
        assert!(
            resumed.next_proposal(&r)
                == ProposalDomain::ContinueAfterClosingRejection {
                    edge: successor,
                    terminal_join: successor,
                    allow_unresolved: false,
                }
        );
    }

    #[test]
    #[ignore = "requires a saved CP run and its replay preparation checkpoint"]
    fn saved_quality_ending_and_continuation_can_be_audited() {
        let started = Instant::now();
        let library = std::env::var_os("MIXLIB_AUDIT_LIBRARY").unwrap();
        let result = std::env::var_os("MIXLIB_AUDIT_RESULT").unwrap();
        let job_path =
            std::path::PathBuf::from(std::env::var_os("MIXLIB_AUDIT_CLOSING_JOB").unwrap());
        let store = Store::open(std::path::Path::new(&library)).unwrap();
        let result: Value = serde_json::from_slice(&std::fs::read(result).unwrap()).unwrap();
        let manifest = &result["search"]["cp_search"]["domain_manifest"]["declaration"];
        assert_eq!(
            manifest["admission_implementation_hash"],
            env!("MIXLIB_ADMISSION_RULES_HASH")
        );
        let incumbent: MixPlan = serde_json::from_value(result["plan"].clone()).unwrap();
        let policy = &incumbent.policy;
        assert_eq!(
            manifest["admission_policy_hash"],
            crate::search_cache::policy_key(policy).unwrap()
        );
        let pool = manifest["pool"]
            .as_array()
            .unwrap()
            .iter()
            .map(|source| {
                let track = store.track(source["id"].as_str().unwrap()).unwrap();
                assert_eq!(source["audio_hash"], json!(track.audio_hash));
                assert_eq!(source["audio_revision"], track.audio_revision);
                assert_eq!(source["analysis"], json!(track.active_analysis));
                assert_eq!(source["overrides"], json!(track.active_overrides));
                assert_eq!(source["override_revision"], track.override_revision);
                track
            })
            .collect();
        let relative = job_path.strip_prefix(&store.root).unwrap();
        let job: Report = store.read(relative.to_str().unwrap()).unwrap();
        let payload = &job.header.extensions["payload"];
        assert_eq!(
            job.header.extensions["digest"],
            json!(canonical_hash(payload).unwrap())
        );
        let saved: SavedJob = serde_json::from_value(payload.clone()).unwrap();
        let route_id = canonical_hash(&saved.route).unwrap().digest;
        assert!(
            result["search"]["cp_search"]["closing_search"]["quality_routes"]
                .get(&route_id)
                .is_some()
        );
        let SavedWork::Replay {
            ending, base_key, ..
        } = saved.work
        else {
            panic!("expected the saved musical ending and replay preparation");
        };
        let context = manifest["timing"]["context_beats"].as_u64().unwrap() as usize;
        let request = crate::composition::Request::from_policy(policy)
            .unwrap()
            .unwrap();
        let cancel = Cancellation::default();
        let mut search = new_search(
            &store,
            policy,
            context,
            Some(pool),
            request,
            started,
            &cancel,
        )
        .unwrap();
        let benchmark_root = relative.ancestors().nth(3).unwrap();
        assert!(benchmark_root.starts_with("search-bench/v1"));
        search.cache.include_audit_windows(
            benchmark_root
                .join("admission")
                .to_str()
                .unwrap()
                .to_owned(),
        );
        search.load_known(&cancel).unwrap();
        let mut r = Refiner::new(search);
        r.sync_edges().unwrap();
        let lookup = r
            .edges
            .iter()
            .enumerate()
            .map(|(id, edge)| (measured_edge_key(edge).unwrap(), id))
            .collect::<BTreeMap<_, _>>();
        let edges = saved
            .route
            .iter()
            .map(|key| lookup[key])
            .collect::<Vec<_>>();
        let (_, plan) = r
            .assemble_measured_chain(&edges)
            .unwrap()
            .expect("saved route must reassemble");
        let (_, endings) = r
            .search
            .composer
            .ending_choices(&r.search.snapshot, &plan, &cancel)
            .unwrap();
        let ending_count = endings.len();
        let bookend = endings
            .into_iter()
            .find(|e| EndingKey::from(e) == ending)
            .expect("saved ending must reappear");
        let base = chain::finish_measured_at_bookends(plan.clone(), bookend).unwrap();
        assert_eq!(base_key, route_key(&base).unwrap());
        let evaluation = r.search.composer.evaluate_for_save(&base, true);
        let decision = match &evaluation {
            Ok(evidence) => json!({"score": evidence["objective"]}),
            Err(rejection) => {
                json!({
                    "reason": rejection.reason,
                    "development": rejection.development
                })
            }
        };
        let failed_track = decision["development"]["track_id"].as_str().map(|id| {
            store.track(id).unwrap().source_locations[0]
                .relative_path
                .clone()
        });
        eprintln!(
            "{route_id}: {} joins, {:.3}s base, decision {decision}, track {failed_track:?}, incumbent {:.5}; replay checkpoint after {} of {} placements",
            edges.len(),
            base.total_output_frames as f64 / f64::from(base.output.sample_rate_hz),
            crate::composition::score(&incumbent),
            payload["work"]["Replay"]["replay"]["next_placement"],
            base.placements.len(),
        );
        if let (1, Err(rejection)) = (ending_count, evaluation) {
            if rejection
                .development
                .as_ref()
                .is_some_and(|d| d.position == base.placements.len())
            {
                let attempt = result["search"]["cp_search"]["quality_neighborhoods"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|attempt| attempt["verification"]["route_id"] == route_id)
                    .expect("quality proposal linked to saved ending");
                let &anchor = edges
                    .iter()
                    .find(|&&id| {
                        let edge = &r.edges[id];
                        attempt["from_track_id"] == edge.plan.placements[0].track_id
                            && attempt["to_track_id"] == edge.plan.placements[1].track_id
                            && attempt["outgoing_cue_frame"] == edge.exit
                            && attempt["incoming_cue_frame"]
                                == edge.plan.placements[1].source_start_frame
                    })
                    .expect("exact saved quality anchor");
                let mut cp = CpSearch::default();
                let id = cp
                    .closing
                    .queue_assembled(&edges, saved.route, plan, Some(anchor))
                    .unwrap();
                assert_eq!(id, route_id);
                let route = cp.closing.pending.pop_front().unwrap().route;
                cp.closing
                    .quality_routes
                    .get_mut(&id)
                    .unwrap()
                    .ending_candidates = ending_count;
                cp.closing.record_decision(
                    &route,
                    CandidateDecision::Rejected {
                        reason: rejection.reason,
                        development: rejection.development,
                    },
                    Some(base.total_output_frames as f64 / f64::from(base.output.sample_rate_hz)),
                );
                let proposal = cp.next_proposal(&r);
                assert_eq!(proposal.anchored_edge(), Some(anchor));
                assert_eq!(proposal.join_requiring_successor(), edges.last().copied());
                let candidate = cp
                    .propose(
                        &r,
                        proposal,
                        Instant::now() + Duration::from_secs(8),
                        &cancel,
                    )
                    .unwrap();
                if let Some((order, joins)) = &candidate {
                    assert!(joins.contains(&Some(anchor)));
                    assert_ne!(joins.last().copied().flatten(), edges.last().copied());
                    eprintln!("measured continuation proposal: {} tracks", order.len());
                }
                eprintln!("continuation audit: {}", json!(cp.quality_proposals));
                r.cp = Some(cp);
                let mut schedule = Schedule::new(0);
                let frames = r
                    .search
                    .pool
                    .iter()
                    .map(|t| t.format.frames)
                    .collect::<Vec<_>>();
                let minimum = (0..frames.len())
                    .map(|i| r.minimum_after(i, 0))
                    .collect::<Vec<_>>();
                let mut scheduled = BTreeSet::new();
                let mut exploration = VecDeque::new();
                let mut fresh = Vec::new();
                for _ in 0..8 {
                    if let Some(seed) = r
                        .schedule_closing_window(
                            &mut schedule,
                            &frames,
                            &minimum,
                            &mut scheduled,
                            &mut exploration,
                            &cancel,
                        )
                        .unwrap()
                    {
                        assert_eq!(
                            seed.placements[0].track_id,
                            base.placements.last().unwrap().track_id
                        );
                        let destination = store.track(&seed.placements[1].track_id).unwrap();
                        let exit = tactics::cue_key(&seed)[0] as f64
                            / f64::from(seed.placements[0].time_map.source_sample_rate_hz);
                        eprintln!(
                            "fresh continuation window: {exit:.3}s into the closing track → {}",
                            destination.source_locations[0].relative_path
                        );
                        fresh.push(seed);
                    }
                }
                eprintln!(
                    "closing window audit: {}",
                    r.cp.as_ref().unwrap().evidence()["closing_continuation_discovery"]
                );
                if std::env::var_os("MIXLIB_AUDIT_MEASURE_CLOSING").is_some() {
                    assert_eq!(r.search.cache.mode, crate::set_config::CacheMode::WarmFacts);
                    let before = r.edges.len();
                    let cp = r.cp.as_mut().unwrap();
                    cp.note_retry(proposal, before);
                    for batch in fresh.chunks(4) {
                        r.search.measure_batch(batch, &cancel).unwrap();
                    }
                    r.sync_edges().unwrap();
                    let mut cp = r.cp.take().unwrap();
                    let next = cp.next_proposal(&r);
                    eprintln!(
                        "closing measurement audit: {} windows, {} new choices, compatible successor {}",
                        r.search.stats.measured_windows, r.edges.len() - before, next == proposal
                    );
                    if next == proposal {
                        let candidate = cp
                            .propose(&r, next, Instant::now() + Duration::from_secs(8), &cancel)
                            .unwrap();
                        eprintln!(
                            "continuation after measurement: {}",
                            json!({"proposals": cp.quality_proposals, "candidate_tracks": candidate.map(|(tracks, _)| tracks.len())})
                        );
                    }
                }
            }
        }
        r.search.snapshot.verify_read_snapshot(&cancel).unwrap();
        eprintln!(
            "fixed-ending audit took {:.2}s",
            started.elapsed().as_secs_f64()
        );
    }

    #[test]
    fn invalid_candidate_evidence_restarts_the_route_instead_of_closing_it() {
        use crate::duration_search::complete_refinement::tests::{edge, fixture};

        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[1200; 3]);
        let id = edge(&mut r, 0, 1, 55., 10.);
        let cancel = Cancellation::default();
        let mut search = ClosingSearch::default();
        search.restore(&mut r.search, &r.edges, &cancel).unwrap();
        search
            .queue(&[id], r.closing_route_keys(&[id]).unwrap())
            .unwrap();
        let route = Arc::clone(&search.pending[0].route);
        search.pending[0].work = Work::Open(SavedWork::Endings(vec![]));
        search.record_decision(
            &route,
            CandidateDecision::Rejected {
                reason: "invalid_evidence",
                development: None,
            },
            None,
        );
        assert_eq!(search.owned_routes().count(), 0);
        assert_eq!(search.evidence()["unknown_measured_routes"], 1);
        search.save(&r.search).unwrap();

        let mut resumed = ClosingSearch::default();
        resumed.restore(&mut r.search, &r.edges, &cancel).unwrap();
        assert_eq!(resumed.pending.len(), 1);
        assert!(matches!(
            resumed.pending[0].work,
            Work::Open(SavedWork::Route)
        ));
        assert_eq!(resumed.evidence()["finished_measured_routes"], 0);
    }

    #[test]
    fn quality_route_goes_first_and_reports_the_measured_clock_failure() {
        use crate::duration_search::complete_refinement::tests::{edge, fixture};

        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[1200; 3]);
        let older = edge(&mut r, 0, 1, 55., 10.);
        let quality = edge(&mut r, 1, 2, 56., 11.);
        let mut closing = ClosingSearch::default();
        closing
            .queue(&[older], r.closing_route_keys(&[older]).unwrap())
            .unwrap();
        let route_id = closing
            .queue_assembled(
                &[quality],
                r.closing_route_keys(&[quality]).unwrap(),
                super::super::tests::bookend_fixture_plan(&r),
                Some(quality),
            )
            .unwrap();
        assert_eq!(closing.pending[0].route.indices, [quality]);
        assert_eq!(closing.pending[1].route.indices, [older]);
        assert!(matches!(closing.pending[0].work, Work::Assembled(_)));
        closing
            .slice(
                &mut r,
                Instant::now() + Duration::from_secs(2),
                &Cancellation::default(),
            )
            .unwrap();
        let evidence = closing.evidence();
        assert_eq!(
            evidence["quality_routes"][&route_id]["state"],
            "no_duration_window"
        );
        assert_eq!(evidence["quality_routes"][&route_id]["ending_attempts"], 0);
        assert_eq!(
            evidence["quality_routes"][&route_id]["openings_below_duration_floor"],
            1
        );
        assert_eq!(
            closing.continuation_hints().next(),
            Some((quality, quality))
        );
    }

    #[test]
    fn closing_discovery_keeps_distinct_prefixes_and_prefers_a_usable_exit() {
        use super::super::tests::clock_edge;

        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::super::tests::fixture(&store, &policy, &[1200; 6]);
        let lead = clock_edge(&mut r, 0, 2, 1100, 0);
        let alternate = clock_edge(&mut r, 1, 2, 1100, 0);
        let arrival = clock_edge(&mut r, 2, 3, 1100, 0);
        let early = clock_edge(&mut r, 3, 4, 50, 0);
        let usable = clock_edge(&mut r, 3, 5, 90, 0);
        Arc::make_mut(&mut r.edges[early]).score = 90.;
        Arc::make_mut(&mut r.edges[usable]).score = 30.;

        let mut cp = CpSearch::default();
        let route = |cp: &mut CpSearch, lead| {
            let edges = [lead, arrival];
            let keys = r.closing_route_keys(&edges).unwrap();
            let id = canonical_hash(&keys).unwrap().digest;
            cp.closing.queue(&edges, keys).unwrap();
            cp.closing.closing_continuations.insert(id, arrival);
        };
        route(&mut cp, lead);
        let probes = cp.closing_probes(&r);
        assert_eq!(probes[0].to, 5);
        assert!(probes.iter().any(|p| p.to == 4));
        assert!(probes.iter().any(|p| p.to == 1));
        assert!(probes.iter().all(|p| p.to != 0));
        route(&mut cp, alternate);
        assert!(cp.closing_probes(&r).iter().any(|p| p.to == 0));
        let next = clock_edge(&mut r, 3, 0, 90, 0);
        assert!(cp.matches_closing_window(&r, Probe { to: 0, ..probes[0] }, &r.edges[next].plan));
    }

    #[test]
    fn closing_lane_defers_a_successor_that_cannot_fit_its_pinned_arrival() {
        use super::super::tests::clock_edge;

        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::super::tests::fixture(&store, &policy, &[1100; 3]);
        let arrival = clock_edge(&mut r, 0, 1, 1080, 350);
        let short = clock_edge(&mut r, 1, 2, 730, 0);
        let long = clock_edge(&mut r, 1, 2, 990, 0);
        for edge in [arrival, short, long] {
            let plan = &mut Arc::make_mut(&mut r.edges[edge]).plan;
            plan.policy = policy.clone();
            plan.transitions.truncate(1);
            plan.transitions[0].from_track_id = plan.placements[0].track_id.clone();
            plan.transitions[0].to_track_id = plan.placements[1].track_id.clone();
            let outgoing = &mut plan.placements[0];
            outgoing.source_end_frame = outgoing.time_map.anchors.last().unwrap().source_frame;
        }
        r.search.capacity_proof = r.search.directed_capacity();
        let mut cp = CpSearch::default();
        let keys = r.closing_route_keys(&[arrival]).unwrap();
        let id = canonical_hash(&keys).unwrap().digest;
        cp.closing.queue(&[arrival], keys).unwrap();
        cp.closing.closing_continuations.insert(id, arrival);
        let probe = cp.closing_probes(&r)[0];
        assert_eq!((probe.from, probe.to), (1, 2));
        assert!(!cp.matches_closing_window(&r, probe, &r.edges[short].plan));
        assert!(cp.matches_closing_window(&r, probe, &r.edges[long].plan));
        let mut uncertain = r.edges[short].plan.clone();
        uncertain.placements[0].time_map.anchors[1].output_frame -= 1;
        assert!(cp.matches_closing_window(&r, probe, &uncertain));
        assert_eq!(r.search.stats.measured_windows, 0);
    }

    #[test]
    fn closing_lane_uses_trims_from_the_entire_committed_route() {
        use super::super::tests::clock_edge;

        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::super::tests::fixture(&store, &policy, &[1000, 910, 910, 910]);
        let early_lead = clock_edge(&mut r, 0, 1, 60, 0);
        let late_lead = clock_edge(&mut r, 0, 1, 980, 0);
        let arrival = clock_edge(&mut r, 1, 2, 890, 0);
        let successor = clock_edge(&mut r, 2, 3, 890, 0);
        for edge in [early_lead, late_lead, arrival, successor] {
            let plan = &mut Arc::make_mut(&mut r.edges[edge]).plan;
            plan.policy = policy.clone();
            plan.transitions.truncate(1);
            plan.transitions[0].from_track_id = plan.placements[0].track_id.clone();
            plan.transitions[0].to_track_id = plan.placements[1].track_id.clone();
            let outgoing = &mut plan.placements[0];
            outgoing.source_end_frame = outgoing.time_map.anchors.last().unwrap().source_frame;
        }
        r.search.capacity_proof = r.search.directed_capacity();
        let proof = r.search.capacity_proof.as_ref().unwrap();
        let seed = &r.edges[successor].plan;
        assert!(
            r.search
                .adjacent_windows_capacity_upper(&r.edges[arrival].plan, seed, proof, [1, 2, 3])
                .unwrap()
                > r.search.request.min()
        );
        let record = |cp: &mut CpSearch, lead| {
            let route = [lead, arrival];
            let keys = r.closing_route_keys(&route).unwrap();
            let id = canonical_hash(&keys).unwrap().digest;
            cp.closing.queue(&route, keys).unwrap();
            cp.closing.closing_continuations.insert(id, arrival);
        };
        let mut cp = CpSearch::default();
        record(&mut cp, early_lead);
        let probe = cp.closing_probes(&r)[0];
        assert_eq!((probe.from, probe.to), (2, 3));
        assert!(!cp.matches_closing_window(&r, probe, seed));
        record(&mut cp, late_lead);
        assert!(cp.matches_closing_window(&r, probe, seed));
        assert_eq!(r.search.stats.measured_windows, 0);
    }

    #[test]
    fn short_mashup_route_retries_with_a_successor_after_all_openings_fail() {
        use super::super::tests::clock_edge;
        use crate::duration_search::complete_refinement::tests::fixture;

        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[1580, 1600, 270, 700]);
        let lead = clock_edge(&mut r, 0, 1, 1470, 0);
        let anchor = clock_edge(&mut r, 1, 2, 1330, 110);
        let successor = clock_edge(&mut r, 2, 3, 220, 0);
        Arc::make_mut(&mut r.edges[anchor]).dense_quality_seconds = 18.;
        Arc::make_mut(&mut r.edges[successor]).score = 0.;
        let cancel = Cancellation::default();
        let mut cp = CpSearch::default();
        cp.closing
            .restore(&mut r.search, &r.edges, &cancel)
            .unwrap();
        let initial = ProposalDomain::AroundMeasured {
            edge: anchor,
            allow_unresolved: false,
        };
        let cutoff = || Instant::now() + Duration::from_secs(2);
        assert_eq!(
            cp.propose(&r, initial, cutoff(), &cancel).unwrap(),
            Some((vec![0, 1, 2], vec![Some(lead), Some(anchor)]))
        );

        // Supply the exact output clock after stitching the two measured
        // joins. The closing song enters at 110s with 160s available.
        let mut plan = super::super::tests::bookend_fixture_plan(&r);
        let rate = u64::from(plan.output.sample_rate_hz);
        plan.transitions[1].end_output_frame = 2820 * rate;
        plan.placements[2].time_map = r.edges[anchor].plan.placements[1].time_map.clone();
        for point in &mut plan.placements[2].time_map.anchors {
            point.output_frame += 1470 * rate;
        }
        plan.header
            .extensions
            .insert("selected_opening_source_frame".into(), json!(0));
        let route_id = cp
            .closing
            .queue_assembled(
                &[lead, anchor],
                r.closing_route_keys(&[lead, anchor]).unwrap(),
                plan.clone(),
                Some(anchor),
            )
            .unwrap();
        let route = cp.closing.pending.pop_front().unwrap().route;
        cp.closing
            .quality_routes
            .get_mut(&route_id)
            .unwrap()
            .opening_candidates = 2;
        for i in 1..=2 {
            cp.closing
                .quality_routes
                .get_mut(&route_id)
                .unwrap()
                .openings_attempted = i;
            assert!(cp
                .closing
                .open_assembled(
                    &r,
                    Arc::clone(&route),
                    plan.clone(),
                    SavedWork::Route,
                    &cancel,
                )
                .unwrap()
                .is_none());
            assert_eq!(
                cp.closing.continuation_hints().next(),
                (i == 2).then_some((anchor, anchor))
            );
            // The other opening is one output frame later.
            for point in &mut plan.placements[2].time_map.anchors {
                point.output_frame += 1;
            }
            plan.transitions[1].end_output_frame += 1;
        }
        let report = &cp.closing.evidence()["quality_routes"][&route_id];
        assert_eq!(report["openings_below_duration_floor"], 2);
        assert_eq!(report["ending_attempts"], 0);
        assert!((report["short_opening_ceiling_seconds"].as_f64().unwrap() - 3404.).abs() < 0.001);

        // Restart with the exact cue identities, then solve the continuation.
        cp.closing.save(&r.search).unwrap();
        let mut resumed = CpSearch::default();
        resumed
            .closing
            .restore(&mut r.search, &r.edges, &cancel)
            .unwrap();
        let retry = resumed.next_proposal(&r);
        assert!(
            retry
                == ProposalDomain::ContinueAfterClosingRejection {
                    edge: anchor,
                    terminal_join: anchor,
                    allow_unresolved: false,
                }
        );
        assert_eq!(
            resumed.propose(&r, retry, cutoff(), &cancel).unwrap(),
            Some((
                vec![0, 1, 2, 3],
                vec![Some(lead), Some(anchor), Some(successor)]
            ))
        );
    }

    #[test]
    fn closing_frontier_restores_work_in_order_by_stable_measured_edges() {
        use crate::duration_search::complete_refinement::tests::{edge, fixture};

        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[1200; 4]);
        let first = edge(&mut r, 0, 1, 55., 10.);
        let second = edge(&mut r, 1, 2, 56., 11.);
        let cancel = Cancellation::default();
        let mut search = ClosingSearch::default();
        search.restore(&mut r.search, &r.edges, &cancel).unwrap();
        search
            .queue(
                &[first, second],
                r.closing_route_keys(&[first, second]).unwrap(),
            )
            .unwrap();
        search
            .queue(&[second], r.closing_route_keys(&[second]).unwrap())
            .unwrap();
        search
            .queue(&[first], r.closing_route_keys(&[first]).unwrap())
            .unwrap();
        // The last route has no pending variants after evaluation.
        search.pending.pop_back();
        search.pending[0].work = Work::Open(SavedWork::Endings(vec![EndingKey {
            end: 3000,
            fade: 100,
            natural: false,
        }]));
        search.save(&r.search).unwrap();

        r.edges.swap(first, second);
        let mut resumed = ClosingSearch::default();
        resumed.restore(&mut r.search, &r.edges, &cancel).unwrap();
        assert_eq!(resumed.pending.len(), 2);
        assert_eq!(resumed.pending[0].route.indices, [second, first]);
        assert!(
            matches!(&resumed.pending[0].work, Work::Open(SavedWork::Endings(ends)) if ends[0].end == 3000)
        );
        assert_eq!(resumed.pending[1].route.indices, [first]);
        // The completed route remains deduplicated after edge indices change.
        let keys = resumed.pending[0].route.keys[..1].to_vec();
        resumed.queue(&[second], keys).unwrap();
        let keys = resumed.pending[0].route.keys.clone();
        resumed.queue(&[second, first], keys).unwrap();
        assert_eq!(resumed.pending.len(), 2);

        r.search.context += 1;
        let mut changed_context = ClosingSearch::default();
        changed_context
            .restore(&mut r.search, &r.edges, &cancel)
            .unwrap();
        assert!(changed_context.pending.is_empty());
        assert!(changed_context.seen.is_empty());
    }

    #[test]
    fn closing_resume_restarts_partial_and_completed_routes_if_incumbent_is_lost() {
        use crate::duration_search::complete_refinement::tests::{edge, fixture};

        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[1200; 3]);
        let first = edge(&mut r, 0, 1, 55., 10.);
        let second = edge(&mut r, 1, 2, 56., 11.);
        let cancel = Cancellation::default();
        let mut search = ClosingSearch::default();
        search.restore(&mut r.search, &r.edges, &cancel).unwrap();
        for i in [first, second] {
            search
                .queue(&[i], r.closing_route_keys(&[i]).unwrap())
                .unwrap();
        }
        search.pending.pop_back();
        search.pending[0].work = Work::Open(SavedWork::Endings(vec![]));
        let mut prior = r.edges[first].plan.clone();
        prior
            .header
            .extensions
            .insert("set_composition".into(), json!({"objective": -2.}));
        r.search.incumbent = Some(prior);
        search.save(&r.search).unwrap();
        r.search.incumbent = None;

        let mut resumed = ClosingSearch::default();
        resumed.restore(&mut r.search, &r.edges, &cancel).unwrap();
        assert_eq!(resumed.owned_routes().count(), 2);
        assert_eq!(resumed.pending.len(), 2);
        assert!(resumed
            .pending
            .iter()
            .all(|job| matches!(job.work, Work::Open(SavedWork::Route))));
        assert_eq!(resumed.evidence()["finished_measured_routes"], 0);
    }

    #[test]
    fn closing_resume_restarts_the_route_if_one_pending_part_is_unreadable() {
        use crate::duration_search::complete_refinement::tests::{edge, fixture};

        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[1200; 3]);
        let first = edge(&mut r, 0, 1, 55., 10.);
        let cancel = Cancellation::default();
        let mut search = ClosingSearch::default();
        search.restore(&mut r.search, &r.edges, &cancel).unwrap();
        search
            .queue(&[first], r.closing_route_keys(&[first]).unwrap())
            .unwrap();
        let route = Arc::clone(&search.pending[0].route);
        for end in [3000, 4000] {
            search.pending.push_back(Job {
                route: Arc::clone(&route),
                work: Work::Open(SavedWork::Endings(vec![EndingKey {
                    end,
                    fade: 100,
                    natural: false,
                }])),
            });
        }
        search.save(&r.search).unwrap();
        let key = search.checkpoint_key.as_ref().unwrap();
        let saved = r
            .search
            .cache
            .read::<SavedFrontier>(&store, "cp-closing-frontiers", key)
            .unwrap()
            .unwrap();
        r.search
            .cache
            .write(
                &store,
                "cp-closing-jobs",
                &saved.pending[1].part,
                &json!({"damaged": true}),
            )
            .unwrap();
        let mut resumed = ClosingSearch::default();
        resumed.restore(&mut r.search, &r.edges, &cancel).unwrap();
        assert_eq!(resumed.pending.len(), 1);
        assert!(matches!(
            resumed.pending[0].work,
            Work::Open(SavedWork::Route)
        ));
        assert_eq!(resumed.evidence()["finished_measured_routes"], 0);
    }
    #[test]
    fn fixed_opening_bound_suspends_before_ending_discovery() {
        use crate::duration_search::complete_refinement::tests::{edge, fixture};
        use mixlib_types::Anchor;

        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[2400; 3]);
        let id = edge(&mut r, 0, 1, 5., 0.);
        let cancel = Cancellation::default();
        let mut search = ClosingSearch::default();
        search.restore(&mut r.search, &r.edges, &cancel).unwrap();
        search
            .queue(&[id], r.closing_route_keys(&[id]).unwrap())
            .unwrap();
        let route = search.pending.pop_front().unwrap().route;
        let mut plan = r.edges[id].plan.clone();
        let rate = u64::from(plan.output.sample_rate_hz);
        plan.total_output_frames = 3500 * rate;
        plan.transitions.truncate(1);
        plan.transitions[0].start_output_frame = 1700 * rate;
        plan.transitions[0].end_output_frame = 1710 * rate;
        plan.placements[1].time_map.anchors = [(0, 1700), (10, 1710), (1800, 3500)]
            .map(|(source, output)| Anchor {
                source_frame: source * rate,
                output_frame: output * rate,
            })
            .into();
        let start = plan.placements[0].source_start_frame;
        plan.header.extensions.extend([
            ("chain_joins".into(), json!([{"score":5.}])),
            ("selected_opening_source_frame".into(), json!(start)),
            ("set_composition".into(), json!({"objective":-1.})),
        ]);
        r.search.incumbent = Some(plan.clone());

        // The fixture has no audio analysis to load for ending discovery.
        assert!(search
            .open_assembled(&r, route, plan, SavedWork::Route, &cancel)
            .unwrap()
            .is_none());
        assert_eq!(search.fixed_opening_bound_pruned, 1);
        assert_eq!(search.suspended.len(), 1);
        assert!(search.suspended[0].1 < -1.);
        search.save(&r.search).unwrap();

        let mut resumed = ClosingSearch::default();
        resumed.restore(&mut r.search, &r.edges, &cancel).unwrap();
        assert!(matches!(
            &resumed.pending[0].work,
            Work::Open(SavedWork::Opening { source_frame, .. }) if *source_frame == start
        ));
    }

    #[test]
    fn closing_bound_suspends_replay_work_without_marking_the_route_complete() {
        use crate::duration_search::complete_refinement::tests::{edge, fixture};
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[1200; 4]);
        let id = edge(&mut r, 0, 1, 5., 0.);
        let cancel = Cancellation::default();
        let mut search = ClosingSearch::default();
        search.restore(&mut r.search, &r.edges, &cancel).unwrap();
        search
            .queue(&[id], r.closing_route_keys(&[id]).unwrap())
            .unwrap();
        let mut base = r.edges[id].plan.clone();
        let rate = u64::from(base.output.sample_rate_hz);
        base.total_output_frames = 57 * 60 * rate;
        base.transitions.truncate(1);
        base.transitions[0].start_output_frame = 1700 * rate;
        base.transitions[0].end_output_frame = 1710 * rate;
        base.header
            .extensions
            .insert("chain_joins".into(), json!([{"score":5.}]));
        let prep = r.search.composer.prepare_replay(&base);
        search.pending[0].work = Work::PrepareReplay(
            FixedEnding::new(
                &r.search.composer,
                base.clone(),
                EndingKey {
                    end: 3000,
                    fade: 100,
                    natural: false,
                },
            )
            .unwrap(),
            prep,
        );
        base.header
            .extensions
            .insert("set_composition".into(), json!({"objective":-1.}));
        r.search.incumbent = Some(base);
        assert!(!search
            .slice(&mut r, Instant::now() + Duration::from_secs(1), &cancel)
            .unwrap());
        assert_eq!(search.suspended.len(), 1);
        assert!(search.suspended[0].1 < -1.);
        assert_eq!(search.replay_penalty_bound_pruned, 1);
        search.checkpoint_routes(&r.search).unwrap();

        let mut resumed = ClosingSearch::default();
        resumed.restore(&mut r.search, &r.edges, &cancel).unwrap();
        assert_eq!(resumed.pending.len(), 1);
        assert!(matches!(
            &resumed.pending[0].work,
            Work::Open(SavedWork::Replay { .. })
        ));
    }
}
