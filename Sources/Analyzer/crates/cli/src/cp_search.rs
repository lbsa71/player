//! SCIP proposal search for the duration-set refiner.
//! Unresolved directed pairs stay selectable. The model ranks routes using
//! proxy scores and capacity envelopes. Only the composer can approve a mix.

use super::*;
use mixlib_planner::{BarCuePage, BarCueRange};
use mixlib_set_search::{
    model::{Domain, Frames, Join, Passage, PrefixConflict, RequiredPair, RouteExclusion},
    objective::transition_shortfall,
    scip::{self, BinaryModel, Row},
};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Choice {
    Measured(usize),
    Unresolved(usize, usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProposalDomain {
    OriginalMaterial,
    MeasuredWithReplay,
    WithUnresolved,
    AroundMeasured {
        edge: usize,
        allow_unresolved: bool,
    },
    AroundMeasuredOriginal {
        edge: usize,
    },
    ContinueAfterClosingRejection {
        edge: usize,
        terminal_join: usize,
        allow_unresolved: bool,
    },
}

impl ProposalDomain {
    fn next(attempts: usize, measured_edges: usize, tracks: usize) -> Self {
        if measured_edges < tracks {
            return Self::WithUnresolved;
        }
        match attempts % 4 {
            0 => Self::OriginalMaterial,
            2 => Self::MeasuredWithReplay,
            _ => Self::WithUnresolved,
        }
    }

    fn allows_unresolved(self) -> bool {
        matches!(
            self,
            Self::WithUnresolved
                | Self::AroundMeasured {
                    allow_unresolved: true,
                    ..
                }
                | Self::ContinueAfterClosingRejection {
                    allow_unresolved: true,
                    ..
                }
        )
    }

    fn anchored_edge(self) -> Option<usize> {
        match self {
            Self::AroundMeasured { edge, .. }
            | Self::AroundMeasuredOriginal { edge }
            | Self::ContinueAfterClosingRejection { edge, .. } => Some(edge),
            _ => None,
        }
    }

    fn original_material(self) -> bool {
        matches!(
            self,
            Self::OriginalMaterial | Self::AroundMeasuredOriginal { .. }
        )
    }

    fn join_requiring_successor(self) -> Option<usize> {
        match self {
            Self::ContinueAfterClosingRejection { terminal_join, .. } => Some(terminal_join),
            _ => None,
        }
    }
}

#[derive(Serialize)]
struct QualityProposal {
    #[serde(skip)]
    edge: usize,
    from_track_id: String,
    to_track_id: String,
    outgoing_cue_frame: u64,
    incoming_cue_frame: u64,
    tactic: Option<TacticId>,
    dense_quality_seconds: f64,
    handoff_quality_seconds: f64,
    allow_unresolved: bool,
    original_material: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    continuation_after_rejected_ending: Option<ClosingContinuation>,
    stop: &'static str,
    capacity_filter: CapacityFilterEvidence,
    unresolved_joins: Option<usize>,
    verification: Option<QualityVerification>,
    cue_repair: Option<CueRepairEvidence>,
}

#[derive(Clone, Copy, Default, Serialize)]
struct CapacityFilterEvidence {
    measured_scope_tracks: Option<u32>,
    measured_scope_pairs: Option<u32>,
    excluded_measured_choices: usize,
    short_measured_windows: usize,
    short_adjacent_combinations: usize,
    measured_clock_row: bool,
}

#[derive(Serialize)]
struct ClosingContinuation {
    from_track_id: String,
    to_track_id: String,
    outgoing_cue_frame: u64,
    incoming_cue_frame: u64,
}

#[derive(Default, Serialize)]
struct CueRepairEvidence {
    initial_conflicts: usize,
    examined: usize,
    substitutions_examined: usize,
    tempo_compatible: usize,
    assembly_rejections: BTreeMap<String, usize>,
    selected: Vec<CueChange>,
}

#[derive(Serialize)]
struct CueChange {
    from_track_id: String,
    to_track_id: String,
    previous_from_track_id: String,
    previous_to_track_id: String,
    previous_outgoing_frame: u64,
    previous_incoming_frame: u64,
    outgoing_frame: u64,
    incoming_frame: u64,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum QualityVerification {
    DurationCapacityConflict {
        measured_joins: usize,
        upper_seconds: f64,
    },
    JoinedSpeedConflict {
        track_id: String,
        incoming_end_frame: u64,
        outgoing_cue_frame: u64,
    },
    AssemblyRejected {
        reason: String,
    },
    QueuedForClosing {
        route_id: String,
    },
}

#[path = "cp_bar_coverage.rs"]
mod bar_coverage;
#[path = "cp_closing.rs"]
mod cp_closing;
use bar_coverage::{BarCoverage, Counts as BarCounts, PageProof};

#[path = "cp_cue_scans.rs"]
mod cue_scans;
use cue_scans::{CueFamily, CueScans, TimingScans};

pub(super) struct PendingCuePage {
    family: CueFamily,
    proof: PageProof,
}

struct ProposalModel {
    master: BinaryModel,
    joins: Vec<Join>,
    choices: Vec<Choice>,
}

#[derive(Serialize, Deserialize)]
struct SavedConflicts {
    prefixes: Vec<Vec<String>>,
    joined_speeds: Vec<[String; 2]>,
    #[serde(default)]
    capacity_subpaths: Vec<Vec<String>>,
}

#[derive(Default)]
pub(super) struct CpSearch {
    manifest: Option<cp_manifest::SearchDomainManifest>,
    pub(super) closing: cp_closing::ClosingSearch,
    pub(super) measured_bootstrap: Option<cp_startup::StartupStats>,
    pub(super) bootstrap_edges: usize,
    pub(super) bootstrap_attempts: usize,
    timing: TimingScans,
    recent_routes: VecDeque<Vec<Choice>>,
    rejected_prefixes: BTreeSet<Vec<usize>>,
    capacity_subpaths: BTreeSet<Vec<usize>>,
    conflict_checkpoint_key: Option<String>,
    restored_prefixes: usize,
    restored_joined_speeds: usize,
    restored_capacity_subpaths: usize,
    demands: Vec<Probe>,
    proposal_turns: usize,
    attempts: usize,
    candidates: usize,
    measured_routes: usize,
    measured_route_rejections: BTreeMap<String, usize>,
    development_conflicts: usize,
    capacity_filter: CapacityFilterEvidence,
    capacity_rejected_proposals: usize,
    joined_speed_conflicts: usize,
    joined_speed_rejected_proposals: usize,
    joined_speed_checks: BTreeMap<(usize, usize), bool>,
    measured_only_attempts: usize,
    measured_bound_skips: usize,
    measured_capacity_skips: usize,
    original_material_attempts: usize,
    original_material_stops: BTreeMap<&'static str, usize>,
    measured_only_stops: BTreeMap<&'static str, usize>,
    mixed_domain_stops: BTreeMap<&'static str, usize>,
    quality_attempts_by_pair: BTreeMap<(usize, usize, bool), usize>,
    quality_attempts_by_edge: BTreeMap<(usize, bool), usize>,
    quality_proposals: Vec<QualityProposal>,
    quality_speed_retries: usize,
    quality_clock_retries: BTreeSet<usize>,
    quality_continuation_retries: BTreeMap<(usize, usize, bool), usize>,
    closing_continuation_retry_count: usize,
    closing_probes_examined: usize,
    closing_windows_scheduled: usize,
    scheduled_probes: BTreeMap<usize, usize>,
    accepted_drafts: usize,
    solve_seconds: f64,
    backend_error: Option<String>,
}

impl CpSearch {
    pub(super) fn bind_manifest(&mut self, search: &Search<'_>) -> Result<()> {
        let manifest = cp_manifest::SearchDomainManifest::from_search(search)?;
        search.cache.write_immutable(
            search.store,
            "cp-domain-manifests",
            &manifest.id,
            &manifest.declaration,
        )?;
        self.manifest = Some(manifest);
        Ok(())
    }

    pub(super) fn demands(&self) -> &[Probe] {
        &self.demands
    }

    pub(super) fn note_probe(&mut self, probe: Probe) {
        if self.demands.contains(&probe) {
            *self.scheduled_probes.entry(probe.tier).or_default() += 1;
        }
    }

    pub(super) fn note_closing_probe(&mut self, scheduled: bool) {
        self.closing_probes_examined += 1;
        self.closing_windows_scheduled += usize::from(scheduled);
    }

    pub(super) fn closing_probes(&self, r: &Refiner<'_>) -> Vec<Probe> {
        let mut hints = self.closing.continuation_routes().collect::<Vec<_>>();
        hints.sort_by(|&(a, _), &(b, _)| {
            let quality =
                |i: usize| r.edges[i].dense_quality_seconds + r.edges[i].handoff_quality_seconds;
            quality(b).total_cmp(&quality(a))
        });
        let mut probes = Vec::new();
        let mut arrivals = BTreeSet::new();
        for (_, route) in hints {
            let Some((from, entry, used)) = closing_arrival(r, route) else {
                continue;
            };
            if !arrivals.insert((from, entry, used)) {
                continue;
            }
            let after = r.search.pacing[from].earliest_exit(entry);
            if after >= r.search.pool[from].format.frames {
                continue;
            }
            // Favor destinations with a measured exit that leaves enough
            // runway after this arrival. Every available destination still
            // gets all three discovery probes.
            let mut pair_fit = BTreeMap::<_, f64>::new();
            for edge in &r.edges {
                let [a, b] = edge.plan.placements.as_slice() else {
                    continue;
                };
                if !edge.last_resort
                    && a.track_id == r.search.pool[from].track_id
                    && r.search.pacing[from].assess(entry, edge.exit).is_some()
                {
                    pair_fit
                        .entry(b.track_id.as_str())
                        .and_modify(|score| *score = score.max(edge.score))
                        .or_insert(edge.score);
                }
            }
            let mut destinations = (0..r.search.pool.len())
                .filter(|&to| used & (1_u128 << to) == 0 && r.search.schedules_transition(from, to))
                .collect::<Vec<_>>();
            let fit = |to: usize| {
                pair_fit
                    .get(r.search.pool[to].track_id.as_str())
                    .copied()
                    .unwrap_or(f64::NEG_INFINITY)
            };
            destinations.sort_by(|&a, &b| fit(b).total_cmp(&fit(a)).then(a.cmp(&b)));
            for neighbors in destinations.chunks(2) {
                // Diversify cue families before advancing through the whole
                // pool. A ranked event can otherwise repeat the same source
                // exit across every destination.
                for tier in [0, 1, 24] {
                    probes.extend(neighbors.iter().map(|&to| Probe {
                        from,
                        to,
                        after,
                        tier,
                    }));
                }
            }
        }
        probes
    }

    pub(super) fn matches_closing_window(
        &self,
        r: &Refiner<'_>,
        probe: Probe,
        seed: &MixPlan,
    ) -> bool {
        let exit = tactics::cue_key(seed)[0];
        let pacing = &r.search.pacing[probe.from];
        self.closing
            .continuation_routes()
            .filter_map(|(_, route)| Some((route, closing_arrival(r, route)?)))
            .any(|(route, (from, entry, used))| {
                from == probe.from
                    && used & (1_u128 << probe.to) == 0
                    && r.search.schedules_transition(from, probe.to)
                    && pacing.earliest_exit(entry) == probe.after
                    && pacing.assess(entry, exit).is_some()
                    && !r.search.capacity_proof.as_ref().is_some_and(|proof| {
                        let arrival = *route.last().expect("nonempty closing route");
                        let windows = route
                            .iter()
                            .map(|&edge| {
                                let (a, b) = r.ends(edge);
                                (a, b, &r.edges[edge].plan)
                            })
                            .chain(std::iter::once((from, probe.to, seed)));
                        r.search
                            .committed_windows_capacity_upper(windows, proof)
                            .or_else(|| {
                                r.search.adjacent_windows_capacity_upper(
                                    &r.edges[arrival].plan,
                                    seed,
                                    proof,
                                    [r.ends(arrival).0, from, probe.to],
                                )
                            })
                            .is_some_and(|upper| upper < r.search.request.min())
                    })
            })
    }

    pub(super) fn evidence(&self) -> Value {
        let mut evidence = json!({
            "method": "cp_route_proposals/1",
            "scope": "Track order and measured transition variants, with unresolved directed pairs retained.",
            "domain_manifest": self.manifest,
            "attempts": self.attempts,
            "proposal_turns": self.proposal_turns,
            "candidates": self.candidates,
            "fully_measured_candidates": self.measured_routes,
            "measured_route_assembly_rejections": self.measured_route_rejections,
            "learned_measured_prefix_conflicts": self.rejected_prefixes.len(),
            "restored_measured_prefix_conflicts": self.restored_prefixes,
            "restored_joined_speed_conflicts": self.restored_joined_speeds,
            "measured_development_conflicts_in_latest_model": self.development_conflicts,
            "duration_capacity_filter_in_latest_model": self.capacity_filter,
            "measured_adjacent_capacity_conflicts_in_latest_model": self.capacity_filter.short_adjacent_combinations,
            "learned_measured_capacity_subpaths": self.capacity_subpaths.len(),
            "restored_measured_capacity_subpaths": self.restored_capacity_subpaths,
            "measured_capacity_rejected_proposals": self.capacity_rejected_proposals,
            "measured_joined_speed_conflicts_in_latest_model": self.joined_speed_conflicts,
            "joined_speed_rejected_proposals": self.joined_speed_rejected_proposals,
            "measured_only_attempts": self.measured_only_attempts,
            "measured_only_turns_pruned_by_score_bound": self.measured_bound_skips,
            "measured_only_turns_pruned_by_duration_capacity": self.measured_capacity_skips,
            "original_material_attempts": self.original_material_attempts,
            "measured_bootstrap": self.measured_bootstrap,
            "measured_bootstrap_attempts": self.bootstrap_attempts,
            "measured_edges_at_last_bootstrap": self.bootstrap_edges,
            "quality_neighborhoods": self.quality_proposals,
            "quality_joined_speed_retries": self.quality_speed_retries,
            "quality_original_clock_retries": self.quality_clock_retries.len(),
            "quality_closing_continuation_retries": self.closing_continuation_retry_count,
            "solve_stops": {
                "measured_only": self.measured_only_stops,
                "original_material": self.original_material_stops,
                "measured_and_unresolved": self.mixed_domain_stops,
            },
            "scheduled_probes_by_tier": self.scheduled_probes,
            "accepted_drafts": self.accepted_drafts,
            "solve_seconds": self.solve_seconds,
            "bar_cue_domain": self.timing.bar.evidence(),
            "backend_error": self.backend_error,
            "closing_search": self.closing.evidence(),
            "objective_bound": null,
        });
        evidence["structural_cue_domain"] = self.timing.structural.evidence();
        evidence["closing_continuation_discovery"] = json!({
            "probes": self.closing_probes_examined,
            "fresh_windows_scheduled": self.closing_windows_scheduled,
        });
        evidence
    }

    fn quality_anchor(&self, r: &Refiner<'_>, allow_unresolved: bool) -> Option<usize> {
        r.edges
            .iter()
            .enumerate()
            .filter(|(_, edge)| {
                !edge.last_resort && edge.dense_quality_seconds + edge.handoff_quality_seconds > 0.
            })
            .filter_map(|(i, edge)| {
                let (from, to) = r.ends(i);
                r.search.permits_transition(from, to).then(|| {
                    let pair_attempts = self
                        .quality_attempts_by_pair
                        .get(&(from, to, allow_unresolved))
                        .copied()
                        .unwrap_or(0);
                    let edge_attempts = self
                        .quality_attempts_by_edge
                        .get(&(i, allow_unresolved))
                        .copied()
                        .unwrap_or(0);
                    (
                        i,
                        pair_attempts,
                        edge_attempts,
                        edge.dense_quality_seconds + edge.handoff_quality_seconds,
                    )
                })
            })
            .min_by(|a, b| {
                a.1.cmp(&b.1)
                    .then(a.2.cmp(&b.2))
                    .then(b.3.total_cmp(&a.3))
                    .then(a.0.cmp(&b.0))
            })
            .map(|(i, _, _, _)| i)
    }

    fn next_proposal(&self, r: &Refiner<'_>) -> ProposalDomain {
        if let Some(attempt) = self.quality_proposals.iter().find(|attempt| {
            !self.quality_clock_retries.contains(&attempt.edge)
                && matches!(&attempt.verification,
                    Some(QualityVerification::QueuedForClosing { route_id })
                        if self.closing.no_duration_window(route_id))
        }) {
            return ProposalDomain::AroundMeasuredOriginal { edge: attempt.edge };
        }
        for allow_unresolved in [false, true] {
            if let Some((edge, terminal_join)) = self
                .closing
                .continuation_hints()
                .filter(|&(edge, arrival)| {
                    self.quality_continuation_retries
                        .get(&(edge, arrival, allow_unresolved))
                        .is_none_or(|&seen| {
                            !allow_unresolved && has_new_successor(r, arrival, seen)
                        })
                })
                .max_by(|&(a, _), &(b, _)| {
                    let quality = |i: usize| {
                        r.edges[i].dense_quality_seconds + r.edges[i].handoff_quality_seconds
                    };
                    quality(a).total_cmp(&quality(b))
                })
            {
                return ProposalDomain::ContinueAfterClosingRejection {
                    edge,
                    terminal_join,
                    allow_unresolved,
                };
            }
        }
        let slot = self.proposal_turns % 8;
        if r.search.incumbent.is_some() {
            match slot {
                0 | 4 => {
                    let allow_unresolved = slot == 4;
                    if let Some(edge) = self.quality_anchor(r, allow_unresolved) {
                        return ProposalDomain::AroundMeasured {
                            edge,
                            allow_unresolved,
                        };
                    }
                }
                2 if r.edges.len() >= r.search.pool.len() => {
                    return ProposalDomain::OriginalMaterial
                }
                _ => {}
            }
        }
        ProposalDomain::next(self.proposal_turns, r.edges.len(), r.search.pool.len())
    }

    fn note_quality_verification(
        &mut self,
        proposal: ProposalDomain,
        verification: QualityVerification,
    ) {
        if proposal.anchored_edge().is_some() {
            if let Some(attempt) = self.quality_proposals.last_mut() {
                attempt.verification = Some(verification);
            }
        }
    }

    pub(super) fn restore_conflicts(
        &mut self,
        refiner: &mut Refiner<'_>,
        cancel: &Cancellation,
    ) -> Result<()> {
        let search = &mut refiner.search;
        let key = canonical_hash(&json!({
            "profile": "cp-measured-conflicts/1",
            "manifest_id": self.manifest.as_ref().map(|manifest| &manifest.id),
            "sources": search.cache.sources,
            "context": search.context,
            "set": Cache::set_key(search.policy)?,
        }))?
        .digest;
        let saved =
            search
                .cache
                .read::<SavedConflicts>(search.store, "cp-measured-conflicts", &key)?;
        if let Some(saved) = saved {
            let lookup = (0..refiner.edges.len())
                .map(|i| Ok((refiner.persistent_key(i)?, i)))
                .collect::<Result<BTreeMap<_, _>>>()?;
            for prefix in saved.prefixes {
                cancel.check()?;
                if !refiner.search.budget_open() {
                    return Ok(());
                }
                let Some(edges) = prefix
                    .iter()
                    .map(|key| lookup.get(key).copied())
                    .collect::<Option<Vec<_>>>()
                else {
                    continue;
                };
                if let Err(AssemblyRejection::Prefix { len, .. }) =
                    refiner.assemble_measured_for_closing(&edges)?
                {
                    self.restored_prefixes +=
                        usize::from(self.rejected_prefixes.insert(edges[..len].to_vec()));
                }
            }
            for [a, b] in saved.joined_speeds {
                cancel.check()?;
                if !refiner.search.budget_open() {
                    return Ok(());
                }
                if let (Some(&enter), Some(&leave)) = (lookup.get(&a), lookup.get(&b)) {
                    if refiner.ends(enter).1 == refiner.ends(leave).0
                        && self
                            .check_joined_speeds(refiner, &[Some(enter), Some(leave)])
                            .is_some()
                    {
                        self.restored_joined_speeds += 1;
                    }
                }
            }
            for subpath in saved.capacity_subpaths {
                cancel.check()?;
                if !refiner.search.budget_open() {
                    return Ok(());
                }
                let Some(edges) = subpath
                    .iter()
                    .map(|key| lookup.get(key).copied())
                    .collect::<Option<Vec<_>>>()
                else {
                    continue;
                };
                if Self::short_subpath_ceiling(refiner, &edges).is_some() {
                    self.restored_capacity_subpaths +=
                        usize::from(self.capacity_subpaths.insert(edges));
                }
            }
        }
        self.conflict_checkpoint_key = Some(key);
        Ok(())
    }

    pub(super) fn save_conflicts(&self, refiner: &Refiner<'_>) -> Result<()> {
        if let Some(key) = self.conflict_checkpoint_key.as_ref() {
            let prefixes = self
                .rejected_prefixes
                .iter()
                .map(|edges| refiner.closing_route_keys(edges))
                .collect::<Result<Vec<_>>>()?;
            let joined_speeds = self
                .joined_speed_checks
                .iter()
                .filter(|(_, rejected)| **rejected)
                .map(|(&(a, b), _)| Ok([refiner.persistent_key(a)?, refiner.persistent_key(b)?]))
                .collect::<Result<Vec<_>>>()?;
            let capacity_subpaths = self
                .capacity_subpaths
                .iter()
                .map(|edges| refiner.closing_route_keys(edges))
                .collect::<Result<Vec<_>>>()?;
            if !prefixes.is_empty() || !joined_speeds.is_empty() || !capacity_subpaths.is_empty() {
                refiner.search.cache.write(
                    refiner.search.store,
                    "cp-measured-conflicts",
                    key,
                    &SavedConflicts {
                        prefixes,
                        joined_speeds,
                        capacity_subpaths,
                    },
                )?;
            }
        }
        Ok(())
    }

    pub(super) fn restore_cue_frontiers(
        &mut self,
        search: &mut Search<'_>,
        cancel: &Cancellation,
    ) -> Result<()> {
        self.timing.bar.restore(search, cancel)?;
        if search.budget_open() {
            self.timing.structural.restore(search, cancel)?;
        }
        Ok(())
    }

    pub(super) fn save_cue_frontiers(&self, search: &Search<'_>) -> Result<()> {
        self.timing.bar.save(search)?;
        self.timing.structural.save(search)
    }

    pub(super) fn has_cue_work(&self) -> bool {
        self.timing.bar.has_work() || self.timing.structural.has_work()
    }

    pub(super) fn schedule_cue_page(
        &mut self,
        search: &mut Search<'_>,
        turn: usize,
        scheduled: &mut BTreeSet<String>,
        batch: &mut Vec<MixPlan>,
        cancel: &Cancellation,
    ) -> Result<Option<PendingCuePage>> {
        let family = if self.timing.structural.has_work()
            && (turn % 4 == 2 || !self.timing.bar.has_work())
        {
            CueFamily::Structural
        } else {
            CueFamily::Bar
        };
        let scans = self.cue_scans(family);
        // Spend one bounded slice consuming pages that require no worker.
        // Stop at the first window that needs this turn's reserved PCM slot.
        let slice_deadline = Instant::now() + Duration::from_millis(150);
        for page_index in 0..8 {
            cancel.check()?;
            if !search.budget_open()
                || !scans.has_work()
                || (page_index > 0 && Instant::now() >= slice_deadline)
            {
                break;
            }
            let Some(page) = scans.page(search, cancel)? else {
                continue;
            };
            let windows = page
                .plans
                .iter()
                .map(|seed| window_key(&search.cache, seed))
                .collect::<Result<Vec<_>>>()?;
            let proof = PageProof::new(&page, windows);
            if proof.observations_ready(search) {
                scans.commit(search, proof)?;
                continue;
            }
            // A refresh can still be needed even if an ordinary probe already
            // visited this key after a previous batch used its refresh slot.
            for (seed, key) in page.plans.into_iter().zip(&proof.windows) {
                if search.oracle.selector_refresh_failed(key) {
                    continue;
                }
                if scheduled.insert(key.clone()) || search.oracle.needs_selector_refresh(key) {
                    batch.push(seed);
                    return Ok(Some(PendingCuePage { family, proof }));
                }
            }
            // No window dispatched here. If another lane in this batch owns
            // the key, wait for its result before deciding whether to defer.
            if !batch.is_empty() {
                return Ok(Some(PendingCuePage { family, proof }));
            }
            scans.commit(search, proof)?;
        }
        Ok(None)
    }

    pub(super) fn commit_cue_page(
        &mut self,
        search: &Search<'_>,
        pending: PendingCuePage,
    ) -> Result<()> {
        self.cue_scans(pending.family).commit(search, pending.proof)
    }

    fn cue_scans(&mut self, family: CueFamily) -> &mut CueScans {
        match family {
            CueFamily::Bar => &mut self.timing.bar,
            CueFamily::Structural => &mut self.timing.structural,
        }
    }

    pub(super) fn step(
        &mut self,
        refiner: &mut Refiner<'_>,
        deadline: Instant,
        cancel: &Cancellation,
    ) -> Result<Option<Draft>> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if self.backend_error.is_some() || remaining < Duration::from_millis(100) {
            return Ok(None);
        }
        cancel.check()?;
        let proposal = self.next_proposal(refiner);
        self.proposal_turns += 1;
        self.note_retry(proposal, refiner.edges.len());
        if let Some(edge) = proposal
            .anchored_edge()
            .filter(|_| remaining >= Duration::from_secs(1))
        {
            refiner
                .search
                .expand_anchor_endpoints(&refiner.edges[edge].plan, cancel)?;
        }
        if !proposal.allows_unresolved() {
            match refiner.search.measured_prune() {
                Some(MeasuredPrune::DurationCapacity) => {
                    self.measured_capacity_skips += 1;
                    return Ok(None);
                }
                Some(MeasuredPrune::ScoreBound) => {
                    self.measured_bound_skips += 1;
                    return Ok(None);
                }
                None => {}
            }
        }
        let prior_speed_rejections = self.joined_speed_rejected_proposals;
        let mut result = self.propose(refiner, proposal, deadline, cancel);
        if matches!(result, Ok(None))
            && proposal.anchored_edge().is_some()
            && self.joined_speed_rejected_proposals > prior_speed_rejections
            && deadline.saturating_duration_since(Instant::now()) >= Duration::from_millis(100)
        {
            cancel.check()?;
            self.quality_speed_retries += 1;
            result = self.propose(
                refiner,
                proposal,
                deadline.min(Instant::now() + Duration::from_secs(2)),
                cancel,
            );
        }
        let (order, choices) = match result {
            Ok(Some(route)) => route,
            Ok(None) => return Ok(None),
            Err(error) if error.code == "CANCELLED" => return Err(error),
            Err(error) => {
                eprintln!(
                    "set search: CP proposals stopped; continuing with legacy refinement: {error}"
                );
                self.backend_error = Some(error.to_string());
                self.demands.clear();
                return Ok(None);
            }
        };
        cancel.check()?;
        self.demands.clear();
        for (i, &from) in order.iter().take(choices.len()).enumerate() {
            if choices[i].is_some() {
                continue;
            }
            let entry = i
                .checked_sub(1)
                .and_then(|p| choices[p])
                .map_or(0, |edge| refiner.edges[edge].entry_end);
            let to = order[i + 1];
            self.timing.bar.prioritize((from, to));
            self.timing.structural.prioritize((from, to));
            let early = refiner.search.pacing[from].earliest_exit(entry);
            let normal = refiner.minimum_after(from, entry);
            let tier_order = if refiner.search.incumbent.is_some() {
                [0, 24, 1]
            } else {
                [1, 0, 24]
            };
            for tier in tier_order {
                self.demands.push(Probe {
                    from,
                    to,
                    after: if tier == 1 { normal } else { early },
                    tier,
                });
            }
        }
        if let Some(edges) = choices.iter().copied().collect::<Option<Vec<_>>>() {
            self.measured_routes += 1;
            match refiner.assemble_measured_for_closing(&edges)? {
                Ok((_, plan)) => {
                    let route_id = self.closing.queue_assembled(
                        &edges,
                        refiner.closing_route_keys(&edges)?,
                        plan,
                        proposal.anchored_edge(),
                    )?;
                    self.note_quality_verification(
                        proposal,
                        QualityVerification::QueuedForClosing { route_id },
                    );
                }
                Err(AssemblyRejection::Prefix { len, reason }) => {
                    self.rejected_prefixes.insert(edges[..len].to_vec());
                    self.note_quality_verification(
                        proposal,
                        QualityVerification::AssemblyRejected {
                            reason: reason.clone(),
                        },
                    );
                    *self.measured_route_rejections.entry(reason).or_default() += 1;
                }
                Err(AssemblyRejection::Unknown(reason)) => {
                    self.note_quality_verification(
                        proposal,
                        QualityVerification::AssemblyRejected {
                            reason: reason.clone(),
                        },
                    );
                    *self.measured_route_rejections.entry(reason).or_default() += 1;
                }
            }
        }
        let draft = refiner.fixed_candidate(&order, &choices)?;
        self.accepted_drafts += usize::from(draft.is_some());
        Ok(draft)
    }

    fn note_retry(&mut self, proposal: ProposalDomain, measured_edges: usize) {
        match proposal {
            ProposalDomain::AroundMeasuredOriginal { edge } => {
                self.quality_clock_retries.insert(edge);
            }
            ProposalDomain::ContinueAfterClosingRejection {
                edge,
                terminal_join,
                allow_unresolved,
            } => {
                self.quality_continuation_retries
                    .insert((edge, terminal_join, allow_unresolved), measured_edges);
                self.closing_continuation_retry_count += 1;
            }
            _ => {}
        }
    }

    fn short_subpath_ceiling(r: &Refiner<'_>, edges: &[usize]) -> Option<f64> {
        // Single windows and adjacent pairs already have their own model rows.
        if edges.len() < 3 {
            return None;
        }
        let proof = r.search.capacity_proof.as_ref()?;
        let windows = edges.iter().map(|&edge| {
            let (from, to) = r.ends(edge);
            (from, to, &r.edges[edge].plan)
        });
        r.search
            .committed_windows_capacity_upper(windows, proof)
            .filter(|&upper| upper < r.search.request.min())
    }

    fn learn_short_subpath(
        &mut self,
        r: &Refiner<'_>,
        choices: &[Option<usize>],
    ) -> Option<(usize, f64)> {
        for run in choices.split(Option::is_none).filter(|run| run.len() >= 3) {
            let edges = run.iter().flatten().copied().collect::<Vec<_>>();
            if let Some(upper) = Self::short_subpath_ceiling(r, &edges) {
                let len = edges.len();
                self.capacity_subpaths.insert(edges);
                self.capacity_rejected_proposals += 1;
                return Some((len, upper));
            }
        }
        None
    }

    /// A measured bridge uses its two boundary speeds and their mean.
    /// Allow each song its slowest measured boundary speed, then remove
    /// both source overlaps and credit their one fixed output overlap.
    fn measured_clock_terms(r: &Refiner<'_>, choices: &[Choice]) -> Option<Vec<(usize, f64)>> {
        let n = r.search.pool.len();
        let rate = r.rate();
        let mut speeds = vec![f64::INFINITY; n];
        let mut trims = Vec::with_capacity(choices.len());
        for &choice in choices {
            let Choice::Measured(edge) = choice else {
                return None;
            };
            let (from, to) = r.ends(edge);
            let plan = &r.edges[edge].plan;
            let (_, _, overlap) = r.search.window_span(plan, from, to)?;
            let departure = &plan.placements[0].time_map;
            let arrival = &plan.placements[1].time_map;
            let transition = &plan.transitions[0];
            let exit = departure.anchors.get(1)?;
            let entry_end = arrival.anchors.iter().rev().nth(1)?;
            if departure.anchors.first()?.output_frame != 0
                || departure.anchors.first()?.source_frame != plan.placements[0].source_start_frame
                || exit.output_frame != transition.start_output_frame
                || entry_end.output_frame != transition.end_output_frame
                || entry_end.source_frame > r.search.pool[to].format.frames
            {
                return None;
            }
            for (track, speed) in [
                (from, *departure.slopes().first()?),
                (to, *arrival.slopes().last()?),
            ] {
                if !(0.5..=2.).contains(&speed) {
                    return None;
                }
                speeds[track] = speeds[track].min(speed);
            }
            let tail = r.search.pool[from]
                .format
                .frames
                .checked_sub(exit.source_frame)?;
            trims.push((from, to, tail, entry_end.source_frame, overlap));
        }
        let output = |track: usize, frames: u64| {
            let source = seconds(&r.search.pool[track], frames);
            (0. ..=86_400.)
                .contains(&source)
                .then_some(source * rate / speeds[track])
        };
        let mut terms = Vec::with_capacity(n + choices.len());
        for (track, source) in r.search.pool.iter().enumerate() {
            if speeds[track].is_finite() {
                // Three rounded bridge intervals, or a rounded opening and
                // its measured lead. Leave additional conversion slack.
                terms.push((track, output(track, source.format.frames)?.ceil() + 8.));
            }
        }
        for (join, (from, to, tail, head, overlap)) in trims.into_iter().enumerate() {
            let removed = (output(from, tail)?.floor() - 1.).max(0.)
                + (output(to, head)?.floor() - 1.).max(0.);
            terms.push((n + join, overlap as f64 - removed));
        }
        Some(terms)
    }

    fn proposal_model(
        &mut self,
        r: &Refiner<'_>,
        proposal: ProposalDomain,
    ) -> Result<ProposalModel> {
        let n = r.search.pool.len();
        let rate = r.rate();
        let computed = r
            .search
            .capacity_proof
            .is_none()
            .then(|| r.search.directed_capacity())
            .flatten();
        let full_capacity = r.search.capacity_proof.as_ref().or(computed.as_ref());
        // Only use this smaller graph for models without unresolved joins.
        // Rebuild it from the currently admitted choices on every solve, so
        // newly measured bridges can restore an otherwise excluded route.
        let measured_capacity = if proposal.allows_unresolved() {
            None
        } else if let Some(capacities) = r.search.track_duration_capacities() {
            let mut adjacent = vec![0_u128; n];
            for edge in 0..r.edges.len() {
                let (from, to) = r.ends(edge);
                if full_capacity.map_or_else(
                    || r.search.permits_transition(from, to),
                    |proof| proof.permits_pair(from, to),
                ) {
                    adjacent[from] |= 1_u128 << to;
                }
            }
            mixlib_set_search::coverage::directed_capacity(
                r.search.request.min(),
                &capacities,
                r.search.required.required_runway().members,
                |from, to| adjacent[from] & (1_u128 << to) != 0,
            )
        } else {
            None
        };
        self.capacity_filter = CapacityFilterEvidence {
            measured_scope_tracks: measured_capacity
                .as_ref()
                .map(|proof| proof.viable_tracks.count_ones()),
            measured_scope_pairs: measured_capacity
                .as_ref()
                .map(|proof| proof.retained_pairs()),
            ..Default::default()
        };
        let capacity_proof = measured_capacity.as_ref().or(full_capacity);
        let viable = capacity_proof.map_or(u128::MAX, |proof| proof.viable_tracks);
        let retained = |track| viable & (1_u128 << track) != 0;
        let minimum_speed = r
            .edges
            .iter()
            .flat_map(|e| &e.plan.placements)
            .map(|p| p.time_map.speed_ratio_limits[0])
            .fold(1. - r.search.policy.max_speed_deviation, f64::min);
        let passages: Vec<_> = (0..n)
            .map(|track| Passage {
                track,
                frames: Frames {
                    min: 0,
                    max: (r.track_seconds(track) * rate / minimum_speed).ceil() as u64,
                },
                can_open: r.search.required.start_allowed(track),
                can_close: r.search.required.finish_allowed(track),
            })
            .collect();
        let clock = Frames {
            min: (r.search.request.min() * rate / (1. + MAX_REPLAY_FRACTION)).floor() as u64,
            max: (r.search.request.max() * rate).ceil() as u64,
        };
        let mut domain = Domain {
            passages,
            joins: vec![],
            required_pairs: r
                .search
                .required
                .pairs
                .iter()
                .filter(|_| r.search.required.mode != crate::required_pairs::Mode::Anywhere)
                .map(|pair| RequiredPair {
                    first_track: pair.indices[0],
                    second_track: pair.indices[1],
                    either_order: r.search.required.mode != crate::required_pairs::Mode::Ordered,
                })
                .collect(),
            clock,
            minimum_tracks: capacity_proof
                .map_or(3, |proof| proof.minimum_tracks.unwrap_or(n + 1).max(3)),
        };
        let mut ids = Vec::with_capacity(r.edges.len() + n * (n - 1));
        let mut join_for_edge = vec![None; r.edges.len()];
        let mut incoming = vec![Vec::new(); n];
        let mut outgoing = vec![Vec::new(); n];
        let mut capacity = domain
            .passages
            .iter()
            .enumerate()
            .map(|(i, p)| (i, p.frames.max as f64))
            .collect::<Vec<_>>();
        let mut original_capacity = proposal.original_material().then(|| {
            (0..n)
                .map(|track| (track, r.track_seconds(track)))
                .collect::<Vec<_>>()
        });
        for (i, edge) in r.edges.iter().enumerate() {
            let (from, to) = r.ends(i);
            if !r.search.permits_transition(from, to) {
                continue;
            }
            let join_index = ids.len();
            join_for_edge[i] = Some(join_index);
            let t = &edge.plan.transitions[0];
            let overlap = t.end_output_frame - t.start_output_frame;
            domain.joins.push(Join {
                from_passage: from,
                to_passage: to,
                overlap: Frames {
                    min: overlap,
                    max: overlap,
                },
                optimistic_surplus: edge.dense_quality_seconds + edge.handoff_quality_seconds
                    - transition_shortfall(edge.score),
            });
            ids.push(Choice::Measured(i));
            incoming[to].push((i, join_index));
            outgoing[from].push((i, join_index));
            // The chosen join commits source trimming on both songs. Leave a
            // frame of source slack before converting to the optimistic clock.
            let cues = tactics::cue_key(&edge.plan);
            if let Some(terms) = &mut original_capacity {
                // Trim the source head/tail, then replace both source overlap
                // intervals by the one measured output overlap. Solo intervals
                // use native speed in this primal heuristic.
                let discarded = seconds(
                    &r.search.pool[from],
                    r.search.pool[from].format.frames.saturating_sub(cues[0]),
                ) + seconds(&r.search.pool[to], cues[3]);
                terms.push((n + join_index, overlap as f64 / rate - discarded));
            }
            let trim_frames = |track: usize, frames: u64| {
                (seconds(&r.search.pool[track], frames.saturating_sub(1)) * rate / minimum_speed)
                    .floor()
            };
            let trimmed = trim_frames(
                from,
                r.search.pool[from].format.frames.saturating_sub(cues[2]),
            ) + trim_frames(to, cues[1]);
            capacity.push((n + join_index, -(overlap as f64 + trimmed)));
        }
        if proposal.allows_unresolved() {
            for from in 0..n {
                for to in 0..n {
                    if !capacity_proof.map_or_else(
                        || r.search.permits_transition(from, to),
                        |proof| proof.permits_pair(from, to),
                    ) {
                        continue;
                    }
                    ids.push(Choice::Unresolved(from, to));
                    domain.joins.push(Join {
                        from_passage: from,
                        to_passage: to,
                        overlap: Frames { min: 0, max: 0 },
                        optimistic_surplus: -25.,
                    });
                }
            }
        }

        let conflicts = self
            .rejected_prefixes
            .iter()
            .filter_map(|prefix| {
                let joins = prefix
                    .iter()
                    .map(|&id| join_for_edge[id])
                    .collect::<Option<Vec<_>>>()?;
                Some(PrefixConflict { joins })
            })
            .collect::<Vec<_>>();
        let variables = ids
            .iter()
            .enumerate()
            .map(|(i, &choice)| (choice, i))
            .collect::<BTreeMap<_, _>>();
        let exclusions = self
            .recent_routes
            .iter()
            .filter_map(|route| {
                route
                    .iter()
                    .map(|choice| variables.get(choice).copied())
                    .collect::<Option<Vec<_>>>()
            })
            .chain(self.closing.owned_routes().filter_map(|route| {
                route
                    .iter()
                    .map(|&edge| join_for_edge.get(edge).copied().flatten())
                    .collect::<Option<Vec<_>>>()
            }))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|joins| RouteExclusion { joins })
            .collect::<Vec<_>>();
        let mut master = domain
            .master_with_restrictions(&conflicts, &exclusions)
            .map_err(|_| Error::new("CP_MODEL", "invalid route domain"))?;
        for subpath in &self.capacity_subpaths {
            let Some(terms) = subpath
                .iter()
                .map(|&edge| Some((n + join_for_edge[edge]?, 1.)))
                .collect::<Option<Vec<_>>>()
            else {
                continue;
            };
            // The proof already grants every reachable bookend extension.
            master.rows.push(Row {
                terms,
                lower: None,
                upper: Some((subpath.len() - 1) as f64),
            });
        }
        for track in (0..n).filter(|&track| !retained(track)) {
            master.rows.push(Row {
                terms: vec![(track, 1.)],
                lower: None,
                upper: Some(0.),
            });
        }
        if let Some(proof) = capacity_proof {
            for (join, choice) in ids.iter().enumerate() {
                if let Choice::Measured(edge) = *choice {
                    let (from, to) = r.ends(edge);
                    let pair_excluded = !proof.permits_pair(from, to);
                    let window_excluded = !pair_excluded
                        && r.search
                            .window_cannot_fit(&r.edges[edge].plan, proof, from, to);
                    if pair_excluded || window_excluded {
                        master.rows.push(Row {
                            terms: vec![(n + join, 1.)],
                            lower: None,
                            upper: Some(0.),
                        });
                        self.capacity_filter.excluded_measured_choices +=
                            usize::from(pair_excluded);
                        self.capacity_filter.short_measured_windows += usize::from(window_excluded);
                    }
                }
            }
        }
        if let Some(edge) = proposal.anchored_edge() {
            let join = join_for_edge.get(edge).copied().flatten().ok_or_else(|| {
                Error::new("CP_MODEL", "quality anchor is outside the route domain")
            })?;
            master.rows.push(Row {
                terms: vec![(n + join, 1.)],
                lower: Some(1.),
                upper: Some(1.),
            });
        }
        if let Some((terminal_join, terminal)) = proposal
            .join_requiring_successor()
            .and_then(|edge| Some((edge, join_for_edge[edge]?)))
        {
            // If this measured arrival is selected, the destination must
            // have an outgoing join in this retry. Other arrivals can
            // still close here. The ordinary model keeps its full domain.
            let closing_track = r.ends(terminal_join).1;
            let mut terms = vec![(n + terminal, 1.)];
            terms.extend(domain.joins.iter().enumerate().filter_map(|(i, join)| {
                (join.from_passage == closing_track).then_some((n + i, -1.))
            }));
            master.rows.push(Row {
                terms,
                lower: None,
                upper: Some(0.),
            });
        }
        if r.search.required.mode == crate::required_pairs::Mode::Anywhere {
            let required_tracks: BTreeSet<_> = r
                .search
                .required
                .pairs
                .iter()
                .flat_map(|pair| pair.indices)
                .collect();
            for track in required_tracks {
                master.rows.push(Row {
                    terms: vec![(track, 1.)],
                    lower: Some(1.),
                    upper: Some(1.),
                });
            }
        }
        master.rows.push(Row {
            terms: capacity,
            lower: Some(clock.min as f64),
            upper: None,
        });
        if !proposal.allows_unresolved() {
            if let Some(terms) = Self::measured_clock_terms(r, &ids) {
                self.capacity_filter.measured_clock_row = true;
                master.rows.push(Row {
                    terms,
                    lower: Some(clock.min as f64),
                    upper: None,
                });
            }
        }
        if let Some(terms) = original_capacity {
            master.rows.push(Row {
                terms,
                lower: Some(r.search.request.min()),
                upper: None,
            });
        }
        let development_ports = r
            .edges
            .iter()
            .map(|edge| crate::composition::measured_development_ports(edge))
            .collect::<Vec<_>>();
        self.development_conflicts = 0;
        self.joined_speed_conflicts = 0;
        for track in 0..n {
            for &(leave, leave_var) in &outgoing[track] {
                // Flow selects at most one incoming join per track. Grouping
                // incompatible arrivals gives the same integer constraints
                // with fewer rows and a stronger fractional relaxation.
                let mut terms = incoming[track]
                    .iter()
                    .filter(|&&(enter, enter_var)| {
                        if r.search.pacing[track]
                            .assess(r.edges[enter].entry_end, r.edges[leave].exit)
                            .is_none()
                        {
                            return true;
                        }
                        if let Some(proof) = capacity_proof {
                            let from = domain.joins[enter_var].from_passage;
                            let to = domain.joins[leave_var].to_passage;
                            let too_short = proof.permits_pair(from, track)
                                && proof.permits_pair(track, to)
                                && r.search
                                    .adjacent_windows_capacity_upper(
                                        &r.edges[enter].plan,
                                        &r.edges[leave].plan,
                                        proof,
                                        [from, track, to],
                                    )
                                    .is_some_and(|upper| upper < r.search.request.min());
                            if too_short {
                                self.capacity_filter.short_adjacent_combinations += 1;
                                return true;
                            }
                        }
                        let incompatible = r.search.composer.rules_out_development(
                            track,
                            development_ports[enter][1],
                            development_ports[leave][0],
                        );
                        self.development_conflicts += usize::from(incompatible);
                        if incompatible {
                            return true;
                        }
                        let incompatible =
                            self.joined_speed_checks.get(&(enter, leave)) == Some(&true);
                        self.joined_speed_conflicts += usize::from(incompatible);
                        incompatible
                    })
                    .map(|&(_, enter_var)| (n + enter_var, 1.))
                    .collect::<Vec<_>>();
                if !terms.is_empty() {
                    terms.push((n + leave_var, 1.));
                    master.rows.push(Row {
                        terms,
                        lower: None,
                        upper: Some(1.),
                    });
                }
            }
        }
        Ok(ProposalModel {
            master,
            joins: domain.joins,
            choices: ids,
        })
    }

    fn propose(
        &mut self,
        r: &Refiner<'_>,
        proposal: ProposalDomain,
        deadline: Instant,
        cancel: &Cancellation,
    ) -> Result<Option<Route>> {
        let n = r.search.pool.len();
        let ProposalModel {
            master,
            joins,
            choices,
        } = self.proposal_model(r, proposal)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        // The first restricted solve has a stable graph and no previously
        // tried route exclusions. Give it more time to find a primal route
        // before alternating short solves with fresh pair discovery.
        let measured_only = !proposal.allows_unresolved();
        let solve_limit = if proposal == ProposalDomain::OriginalMaterial
            && self.original_material_attempts == 0
            && r.search.incumbent.is_none()
        {
            Duration::from_secs(8)
        } else if proposal.anchored_edge().is_some() {
            Duration::from_secs(4)
        } else {
            Duration::from_secs(2)
        };
        self.attempts += 1;
        self.measured_only_attempts += usize::from(measured_only);
        self.original_material_attempts += usize::from(proposal.original_material());
        let start = Instant::now();
        let solved = scip::solve(&master, remaining.min(solve_limit), &cancel.0);
        self.solve_seconds += start.elapsed().as_secs_f64();
        let solved =
            solved.map_err(|e| Error::new("CP_BACKEND", format!("SCIP solve failed: {e:?}")))?;
        let stop = match solved.stop {
            scip::Stop::Optimal => "optimal",
            scip::Stop::Infeasible => "infeasible",
            scip::Stop::TimeLimit if solved.candidate.is_some() => "time_limit_with_candidate",
            scip::Stop::TimeLimit => "time_limit_without_candidate",
            scip::Stop::Interrupted => "interrupted",
            scip::Stop::Other => "other",
        };
        let stops = if measured_only {
            &mut self.measured_only_stops
        } else {
            &mut self.mixed_domain_stops
        };
        *stops.entry(stop).or_default() += 1;
        if proposal.original_material() {
            *self.original_material_stops.entry(stop).or_default() += 1;
        }
        if let Some(edge) = proposal.anchored_edge() {
            let allow_unresolved = proposal.allows_unresolved();
            let (from, to) = r.ends(edge);
            let measured = &r.edges[edge];
            *self
                .quality_attempts_by_pair
                .entry((from, to, allow_unresolved))
                .or_default() += 1;
            *self
                .quality_attempts_by_edge
                .entry((edge, allow_unresolved))
                .or_default() += 1;
            self.quality_proposals.push(QualityProposal {
                edge,
                from_track_id: r.search.pool[from].track_id.clone(),
                to_track_id: r.search.pool[to].track_id.clone(),
                outgoing_cue_frame: measured.exit,
                incoming_cue_frame: measured.plan.placements[1].source_start_frame,
                tactic: read_tactics(&measured.plan)
                    .ok()
                    .and_then(|tactics| tactics.first().map(|t| t.tactic_id)),
                dense_quality_seconds: measured.dense_quality_seconds,
                handoff_quality_seconds: measured.handoff_quality_seconds,
                allow_unresolved,
                original_material: proposal.original_material(),
                continuation_after_rejected_ending: match proposal {
                    ProposalDomain::ContinueAfterClosingRejection { terminal_join, .. } => {
                        let edge = &r.edges[terminal_join];
                        Some(ClosingContinuation {
                            from_track_id: edge.plan.placements[0].track_id.clone(),
                            to_track_id: edge.plan.placements[1].track_id.clone(),
                            outgoing_cue_frame: edge.exit,
                            incoming_cue_frame: edge.plan.placements[1].source_start_frame,
                        })
                    }
                    _ => None,
                },
                stop,
                capacity_filter: self.capacity_filter,
                unresolved_joins: solved.candidate.as_ref().map(|candidate| {
                    candidate
                        .selected
                        .iter()
                        .filter(|&&i| {
                            i.checked_sub(n)
                                .and_then(|j| choices.get(j))
                                .is_some_and(|choice| matches!(choice, Choice::Unresolved(..)))
                        })
                        .count()
                }),
                verification: None,
                cue_repair: None,
            });
        }
        cancel.check()?;
        let Some(candidate) = solved.candidate else {
            return Ok(None);
        };
        let selected = candidate
            .selected
            .iter()
            .filter_map(|&i| {
                i.checked_sub(n)
                    .and_then(|j| choices.get(j).map(|id| (j, *id)))
            })
            .collect::<Vec<_>>();
        let Some(mut order) = route_order(n, &joins, &selected) else {
            return Err(Error::new(
                "CP_MODEL",
                "SCIP candidate did not decode to a single route",
            ));
        };
        self.candidates += 1;
        if let Some((enter, leave)) = self.check_joined_speeds(r, &order.1) {
            self.joined_speed_rejected_proposals += 1;
            let track = r.ends(leave).0;
            self.note_quality_verification(
                proposal,
                QualityVerification::JoinedSpeedConflict {
                    track_id: r.search.pool[track].track_id.clone(),
                    incoming_end_frame: r.edges[enter].entry_end,
                    outgoing_cue_frame: r.edges[leave].exit,
                },
            );
            if proposal.anchored_edge().is_none() || order.1.iter().any(Option::is_none) {
                return Ok(None);
            }
            let repaired = self.repair_quality_cues(
                r,
                &order,
                (enter, leave),
                proposal,
                deadline.min(Instant::now() + Duration::from_millis(250)),
                cancel,
            )?;
            let (candidate, evidence) = repaired;
            if let Some(attempt) = self.quality_proposals.last_mut() {
                attempt.cue_repair = Some(evidence);
            }
            let Some(candidate) = candidate else {
                return Ok(None);
            };
            order = candidate;
        }
        if let Some((measured_joins, upper_seconds)) = self.learn_short_subpath(r, &order.1) {
            self.note_quality_verification(
                proposal,
                QualityVerification::DurationCapacityConflict {
                    measured_joins,
                    upper_seconds,
                },
            );
            return Ok(None);
        }
        self.recent_routes.push_back(
            order
                .0
                .windows(2)
                .zip(&order.1)
                .map(|(pair, edge)| {
                    edge.map_or(Choice::Unresolved(pair[0], pair[1]), Choice::Measured)
                })
                .collect(),
        );
        if self.recent_routes.len() > 24 {
            self.recent_routes.pop_front();
        }
        Ok(Some(order))
    }

    fn repair_quality_cues(
        &mut self,
        r: &Refiner<'_>,
        route: &Route,
        conflict: (usize, usize),
        proposal: ProposalDomain,
        deadline: Instant,
        cancel: &Cancellation,
    ) -> Result<(Option<Route>, CueRepairEvidence)> {
        let mut evidence = CueRepairEvidence::default();
        let anchor = proposal
            .anchored_edge()
            .expect("quality repair has a pinned join");
        let must_continue = proposal.join_requiring_successor();
        let Some(original) = route.1.iter().copied().collect::<Option<Vec<_>>>() else {
            return Ok((None, evidence));
        };
        let conflicts = self.joined_speed_conflicts_for(r, &route.1);
        debug_assert_eq!(conflicts.first().copied(), Some(conflict));
        evidence.initial_conflicts = conflicts.len();
        let mut pending = vec![(original.clone(), conflicts)];
        let mut visited = BTreeSet::from([original.clone()]);
        while let Some((choices, conflicts)) = pending.pop() {
            let Some(&(bad_enter, bad_leave)) = conflicts.first() else {
                continue;
            };
            let Some(left) = choices
                .windows(2)
                .position(|pair| pair == [bad_enter, bad_leave])
            else {
                continue;
            };
            let cue_choices = nearby_cue_choices(r, (bad_enter, bad_leave), anchor);
            let substitutions =
                substitute_middle_choices(r, &choices, (bad_enter, bad_leave), anchor);
            let alternatives = cue_choices
                .into_iter()
                .take(24)
                .map(|pair| (pair, false))
                .chain(substitutions.into_iter().take(16).map(|pair| (pair, true)));
            for ([enter, leave], substitute) in alternatives {
                if Instant::now() >= deadline || evidence.examined >= 64 {
                    return Ok((None, evidence));
                }
                cancel.check()?;
                let mut revised = choices.clone();
                revised[left] = enter;
                revised[left + 1] = leave;
                if revised
                    .last()
                    .is_some_and(|&edge| must_continue == Some(edge))
                {
                    continue;
                }
                if !visited.insert(revised.clone()) {
                    continue;
                }
                evidence.examined += 1;
                evidence.substitutions_examined += usize::from(substitute);
                let selected = revised.iter().copied().map(Some).collect::<Vec<_>>();
                let remaining = self.joined_speed_conflicts_for(r, &selected);
                if !remaining.is_empty() {
                    if remaining.len() < conflicts.len() {
                        pending.push((revised, remaining));
                    }
                    continue;
                }
                evidence.tempo_compatible += 1;
                let assembly = match r.assemble_measured_for_closing(&revised) {
                    Err(error) if measured_beam::route_local(&error) => {
                        *evidence.assembly_rejections.entry(error.code).or_default() += 1;
                        continue;
                    }
                    result => result?,
                };
                match assembly {
                    Ok((tracks, plan)) if r.search.required.satisfied(&plan) => {
                        evidence.selected = original
                            .iter()
                            .zip(&revised)
                            .filter(|(old, new)| old != new)
                            .map(|(&old, &new)| cue_change(r, old, new))
                            .collect();
                        return Ok((Some((tracks, selected)), evidence));
                    }
                    Ok(_) => {
                        *evidence
                            .assembly_rejections
                            .entry("required_pairs".into())
                            .or_default() += 1;
                    }
                    Err(AssemblyRejection::Prefix { len, reason }) => {
                        self.rejected_prefixes.insert(revised[..len].to_vec());
                        *evidence.assembly_rejections.entry(reason).or_default() += 1;
                    }
                    Err(AssemblyRejection::Unknown(reason)) => {
                        *evidence.assembly_rejections.entry(reason).or_default() += 1;
                    }
                }
            }
            // Continue with the fewest remaining conflicts and a stable tie order.
            pending.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(b.0.cmp(&a.0)));
        }
        Ok((None, evidence))
    }

    /// Measure a cheap contextual fact only when the proposal actually selects
    /// both adjacent variants. Later solves receive the exact pair conflict.
    fn check_joined_speeds(
        &mut self,
        r: &Refiner<'_>,
        choices: &[Option<usize>],
    ) -> Option<(usize, usize)> {
        self.joined_speed_conflicts_for(r, choices)
            .into_iter()
            .next()
    }

    fn joined_speed_conflicts_for(
        &mut self,
        r: &Refiner<'_>,
        choices: &[Option<usize>],
    ) -> Vec<(usize, usize)> {
        let mut rejected = Vec::new();
        for pair in choices.windows(2) {
            if let [Some(enter), Some(leave)] = *pair {
                let conflict = *self
                    .joined_speed_checks
                    .entry((enter, leave))
                    .or_insert_with(|| {
                        chain::rules_out_joined_speed(&r.edges[enter].plan, &r.edges[leave].plan)
                    });
                if conflict {
                    rejected.push((enter, leave));
                }
            }
        }
        rejected
    }
}

fn has_new_successor(r: &Refiner<'_>, arrival: usize, measured_edges: usize) -> bool {
    let (previous, from) = r.ends(arrival);
    let incoming = &r.edges[arrival];
    let previous_id = &r.search.pool[previous].track_id;
    let from_id = &r.search.pool[from].track_id;
    let a = crate::composition::measured_development_ports(incoming)[1];
    r.edges
        .iter()
        .enumerate()
        .skip(measured_edges)
        .any(|(id, outgoing)| {
            !outgoing.last_resort
                && &outgoing.plan.placements[0].track_id == from_id
                && &outgoing.plan.placements[1].track_id != previous_id
                && r.search.permits_transition(from, r.ends(id).1)
                && r.search.pacing[from]
                    .assess(incoming.entry_end, outgoing.exit)
                    .is_some()
                && !r.search.composer.rules_out_development(
                    from,
                    a,
                    crate::composition::measured_development_ports(outgoing)[0],
                )
                && chain::measured_bridge(&incoming.plan, &outgoing.plan)
                    .is_ok_and(|bridge| bridge.is_some())
        })
}

fn closing_arrival(r: &Refiner<'_>, route: &[usize]) -> Option<(usize, u64, u128)> {
    let arrival = *route.last()?;
    let (first, _) = r.ends(route[0]);
    let used = route.iter().fold(1_u128 << first, |mask, &edge| {
        mask | (1_u128 << r.ends(edge).1)
    });
    Some((r.ends(arrival).1, r.edges[arrival].entry_end, used))
}

type Route = (Vec<usize>, Vec<Option<usize>>);

fn measured_value(r: &Refiner<'_>, id: usize) -> f64 {
    let edge = &r.edges[id];
    edge.dense_quality_seconds + edge.handoff_quality_seconds - transition_shortfall(edge.score)
}

fn cue_change(r: &Refiner<'_>, old: usize, new: usize) -> CueChange {
    let (from, to) = r.ends(new);
    let (previous_from, previous_to) = r.ends(old);
    CueChange {
        from_track_id: r.search.pool[from].track_id.clone(),
        to_track_id: r.search.pool[to].track_id.clone(),
        previous_from_track_id: r.search.pool[previous_from].track_id.clone(),
        previous_to_track_id: r.search.pool[previous_to].track_id.clone(),
        previous_outgoing_frame: r.edges[old].exit,
        previous_incoming_frame: r.edges[old].plan.placements[1].source_start_frame,
        outgoing_frame: r.edges[new].exit,
        incoming_frame: r.edges[new].plan.placements[1].source_start_frame,
    }
}

fn nearby_cue_choices(
    r: &Refiner<'_>,
    (enter, leave): (usize, usize),
    anchor: usize,
) -> Vec<[usize; 2]> {
    let value = |id| measured_value(r, id);
    let variants = |current: usize| {
        if current == anchor {
            return vec![current];
        }
        let pair = r.ends(current);
        let mut result = (0..r.edges.len())
            .filter(|&id| id != current && !r.edges[id].last_resort && r.ends(id) == pair)
            .collect::<Vec<_>>();
        result.sort_by(|&a, &b| value(b).total_cmp(&value(a)).then(a.cmp(&b)));
        result.truncate(7);
        result.push(current);
        result
    };
    let arrivals = variants(enter);
    let departures = variants(leave);
    let mut alternatives = arrivals
        .iter()
        .flat_map(|&a| departures.iter().map(move |&b| [a, b]))
        .filter(|&pair| pair != [enter, leave])
        .collect::<Vec<_>>();
    let edit_count = |[a, b]: [usize; 2]| usize::from(a != enter) + usize::from(b != leave);
    let pair_value = |[a, b]: [usize; 2]| value(a) + value(b);
    alternatives.sort_by(|&a, &b| {
        edit_count(a)
            .cmp(&edit_count(b))
            .then(pair_value(b).total_cmp(&pair_value(a)))
            .then(a.cmp(&b))
    });
    alternatives
}

fn substitute_middle_choices(
    r: &Refiner<'_>,
    choices: &[usize],
    (enter, leave): (usize, usize),
    anchor: usize,
) -> Vec<[usize; 2]> {
    if enter == anchor || leave == anchor {
        return vec![];
    }
    let used = choices
        .iter()
        .flat_map(|&edge| {
            let (a, b) = r.ends(edge);
            [a, b]
        })
        .collect::<BTreeSet<_>>();
    let (head, _) = r.ends(enter);
    let (_, tail) = r.ends(leave);
    let mut incoming = vec![vec![]; r.search.pool.len()];
    let mut outgoing = vec![vec![]; r.search.pool.len()];
    for (id, _) in r
        .edges
        .iter()
        .enumerate()
        .filter(|(_, edge)| !edge.last_resort)
    {
        let (a, b) = r.ends(id);
        if !r.search.permits_transition(a, b) {
            continue;
        }
        if a == head && !used.contains(&b) {
            incoming[b].push(id);
        }
        if b == tail && !used.contains(&a) {
            outgoing[a].push(id);
        }
    }
    let mut alternatives = vec![];
    for (track, (mut arrivals, mut departures)) in incoming.into_iter().zip(outgoing).enumerate() {
        if arrivals.is_empty() || departures.is_empty() {
            continue;
        }
        for choices in [&mut arrivals, &mut departures] {
            choices.sort_by(|&a, &b| {
                measured_value(r, b)
                    .total_cmp(&measured_value(r, a))
                    .then(a.cmp(&b))
            });
            choices.truncate(3);
        }
        for a in arrivals {
            for &b in &departures {
                if r.search.pacing[track]
                    .assess(r.edges[a].entry_end, r.edges[b].exit)
                    .is_some()
                {
                    alternatives.push([a, b]);
                }
            }
        }
    }
    let value = |[a, b]: [usize; 2]| measured_value(r, a) + measured_value(r, b);
    alternatives.sort_by(|&a, &b| value(b).total_cmp(&value(a)).then(a.cmp(&b)));
    alternatives
}

fn route_order(n: usize, joins: &[Join], selected: &[(usize, Choice)]) -> Option<Route> {
    let mut predecessors = vec![false; n];
    let mut successors = vec![None; n];
    for &(index, choice) in selected {
        let join = &joins[index];
        if predecessors[join.to_passage] || successors[join.from_passage].is_some() {
            return None;
        }
        predecessors[join.to_passage] = true;
        successors[join.from_passage] = Some((join.to_passage, choice));
    }
    let mut track = (0..n).find(|&i| !predecessors[i] && successors[i].is_some())?;
    let mut order = vec![track];
    let mut choices = vec![];
    while let Some((next, choice)) = successors[track].take() {
        choices.push(match choice {
            Choice::Measured(id) => Some(id),
            Choice::Unresolved(..) => None,
        });
        order.push(next);
        track = next;
    }
    (choices.len() == selected.len()).then_some((order, choices))
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn bookend_fixture_plan(r: &Refiner<'_>) -> MixPlan {
        let mut plan: MixPlan =
            serde_json::from_str(include_str!("../../../examples/mix-plan.json")).unwrap();
        for (placement, track) in plan.placements.iter_mut().zip(&r.search.pool) {
            placement.track_id = track.track_id.clone();
        }
        for (i, transition) in plan.transitions.iter_mut().enumerate() {
            transition.from_track_id = plan.placements[i].track_id.clone();
            transition.to_track_id = plan.placements[i + 1].track_id.clone();
        }
        plan.validate().unwrap();
        plan
    }

    #[test]
    #[ignore = "requires a local Analyzer library with an approved measured set and its window cache"]
    fn approved_library_route_survives_joined_speed_filter() {
        let library = std::env::var_os("MIXLIB_AUDIT_LIBRARY").unwrap();
        let plan_id = std::env::var("MIXLIB_AUDIT_PLAN").unwrap();
        let store = Store::open(std::path::Path::new(&library)).unwrap();
        let plan: MixPlan = store.read(&format!("plans/{plan_id}.json")).unwrap();
        assert!(plan.complete);
        let tracks = plan
            .placements
            .iter()
            .map(|p| store.track(&p.track_id).unwrap())
            .collect::<Vec<_>>();
        let joins = plan.header.extensions["chain_joins"].as_array().unwrap();
        let mut matched = vec![Vec::<MeasuredEdge>::new(); joins.len()];
        let mut cache = Cache::open(&plan.policy, &tracks).unwrap();
        for key in cache.keys(&store, "windows").unwrap() {
            let Some(measured) = cache.read::<Measurement>(&store, "windows", &key).unwrap() else {
                continue;
            };
            for edge in measured.edges.into_iter().filter(|edge| !edge.last_resort) {
                let a = &edge.plan.placements[0];
                let b = &edge.plan.placements[1];
                let t = &edge.plan.transitions[0];
                if let Some(index) = joins.iter().position(|join| {
                    join["from"] == a.track_id
                        && join["to"] == b.track_id
                        && join["outgoing_cue_frame"] == edge.exit
                        && join["incoming_cue_frame"] == b.source_start_frame
                        && join["score"]
                            .as_f64()
                            .is_some_and(|score| (score - edge.score).abs() < 1e-8)
                }) {
                    let saved = &plan.transitions[index];
                    let pinned = [a, b]
                        .into_iter()
                        .zip(&plan.placements[index..index + 2])
                        .all(|(candidate, saved)| {
                            candidate.audio_hash == saved.audio_hash
                                && candidate.audio_revision == saved.audio_revision
                                && candidate.analysis.hash == saved.analysis.hash
                                && candidate.override_revision == saved.override_revision
                        });
                    if pinned
                        && t.end_output_frame - t.start_output_frame
                            == saved.end_output_frame - saved.start_output_frame
                        && read_tactics(&edge.plan)
                            .unwrap()
                            .first()
                            .is_some_and(|tactic| joins[index]["choice"] == json!(tactic.tactic_id))
                    {
                        matched[index].push(edge);
                    }
                }
            }
        }
        let mut reachable = Vec::new();
        for (i, variants) in matched.iter().enumerate() {
            assert!(
                !variants.is_empty(),
                "saved join {} missing from window cache",
                i + 1
            );
            let saved_solo = i.checked_sub(1).map(|previous| {
                plan.transitions[i].start_output_frame - plan.transitions[previous].end_output_frame
            });
            reachable = variants
                .iter()
                .filter(|variant| {
                    saved_solo.is_none_or(|elapsed| {
                        reachable.iter().any(|previous: &&MeasuredEdge| {
                            chain::measured_bridge(&previous.plan, &variant.plan)
                                .ok()
                                .flatten()
                                .is_some_and(|bridge| {
                                    bridge.solo_end - bridge.solo_start == elapsed
                                })
                        })
                    })
                })
                .collect();
            assert!(
                !reachable.is_empty(),
                "saved route blocked at join {}",
                i + 1
            );
        }
        eprintln!(
            "verified joined-speed reachability and bridge frame counts for all {} pinned joins",
            matched.len()
        );
    }

    #[test]
    #[ignore = "requires a local Analyzer library and a CP result reporting a joined-speed conflict"]
    fn cached_library_cues_explain_a_reported_speed_conflict() {
        let library = std::env::var_os("MIXLIB_AUDIT_LIBRARY").unwrap();
        let result = std::env::var_os("MIXLIB_AUDIT_RESULT").unwrap();
        let result: Value = serde_json::from_slice(&std::fs::read(result).unwrap()).unwrap();
        let cp = &result["search"]["cp_search"];
        let conflict = cp["quality_neighborhoods"]
            .as_array()
            .unwrap()
            .iter()
            .map(|attempt| &attempt["verification"])
            .find(|verification| verification["status"] == "joined_speed_conflict")
            .expect("result has no joined-speed conflict");
        let middle = conflict["track_id"].as_str().unwrap();
        let incoming_frame = conflict["incoming_end_frame"].as_u64().unwrap();
        let outgoing_frame = conflict["outgoing_cue_frame"].as_u64().unwrap();
        let store = Store::open(std::path::Path::new(&library)).unwrap();
        let policy: MixingConfig =
            serde_json::from_value(result["plan"]["policy"].clone()).unwrap();
        let pool = cp["domain_manifest"]["declaration"]["pool"]
            .as_array()
            .unwrap()
            .iter()
            .map(|source| {
                let track = store.track(source["id"].as_str().unwrap()).unwrap();
                assert_eq!(source["audio_hash"], json!(track.audio_hash));
                assert_eq!(source["audio_revision"], track.audio_revision);
                assert_eq!(source["analysis"], json!(track.active_analysis));
                assert_eq!(source["override_revision"], track.override_revision);
                assert_eq!(source["overrides"], json!(track.active_overrides));
                track
            })
            .collect::<Vec<_>>();
        let pins = pool
            .iter()
            .map(|track| (track.track_id.as_str(), track))
            .collect::<BTreeMap<_, _>>();
        let mut cache = Cache::open(&policy, &pool).unwrap();
        let mut identities = BTreeSet::new();
        let mut arrivals = Vec::new();
        let mut departures = Vec::new();
        for key in cache.keys(&store, "windows").unwrap() {
            let Some(measurement) = cache.read::<Measurement>(&store, "windows", &key).unwrap()
            else {
                continue;
            };
            for edge in measurement
                .edges
                .into_iter()
                .filter(|edge| !edge.last_resort)
            {
                let [from, to] = edge.plan.placements.as_slice() else {
                    continue;
                };
                if (from.track_id != middle && to.track_id != middle)
                    || ![from, to].into_iter().all(|p| {
                        pins.get(p.track_id.as_str()).is_some_and(|track| {
                            let signature = measurement.sources.get(&p.track_id).map_or(
                                track.active_overrides.is_none() && track.override_revision == 0,
                                |key| cache.sources.get(&p.track_id) == Some(key),
                            );
                            signature
                                && p.audio_hash == track.audio_hash
                                && p.audio_revision == track.audio_revision
                                && track.active_analysis.as_ref().is_some_and(|analysis| {
                                    analysis.path == p.analysis.path
                                        && analysis.hash == p.analysis.hash
                                })
                                && p.override_revision == track.override_revision
                                && p.time_map.source_sample_rate_hz == track.format.sample_rate_hz
                                && p.source_end_frame <= track.format.frames
                        })
                    })
                {
                    continue;
                }
                let mut identity = json!(&edge.plan);
                remove_ids(&mut identity);
                if !identities.insert(canonical_hash(&identity).unwrap().digest) {
                    continue;
                }
                if to.track_id == middle {
                    arrivals.push(edge);
                } else {
                    departures.push(edge);
                }
            }
        }
        let pacing =
            mixlib_planner::read_track_pacing(&store, middle, &Cancellation::default()).unwrap();
        let mut counts = BTreeMap::new();
        let mut reported_conflicts = 0;
        let mut reported_bridges = 0;
        let mut recovered_bridges = 0;
        let mut examples = BTreeSet::new();
        for arrival in &arrivals {
            for departure in &departures {
                let incoming = &arrival.plan.placements[1];
                let outgoing = &departure.plan.placements[0];
                let bridge = chain::measured_bridge(&arrival.plan, &departure.plan);
                let recovered = matches!(&bridge, Ok(Some(b)) if b.map.anchors.len()
                    > incoming.time_map.anchors.len() + outgoing.time_map.anchors.len());
                recovered_bridges += usize::from(recovered);
                if arrival.entry_end == incoming_frame && departure.exit == outgoing_frame {
                    reported_bridges += 1;
                    if let Err(error) = &bridge {
                        if matches!(
                            error.code.as_str(),
                            "TIME_MAP_SPEED" | "TIME_MAP_ACCELERATION"
                        ) {
                            reported_conflicts += 1;
                            examples.insert(format!(
                                "{} at the reported cues, speeds {:.4} → {:.4}, {:.1}s source runway, limits {:?} and {:?}",
                                error.code, incoming.time_map.slopes().last().unwrap(),
                                outgoing.time_map.slopes()[0],
                                outgoing_frame.saturating_sub(incoming_frame) as f64
                                    / f64::from(incoming.time_map.source_sample_rate_hz),
                                incoming.time_map.speed_ratio_limits,
                                outgoing.time_map.speed_ratio_limits,
                            ));
                        }
                    }
                }
                let outcome = if arrival.plan.placements[0].track_id
                    == departure.plan.placements[1].track_id
                {
                    "repeated_neighbor"
                } else if pacing.assess(arrival.entry_end, departure.exit).is_none() {
                    "phrase_pacing"
                } else {
                    match &bridge {
                        Ok(Some(_)) => "bridge_pass",
                        Ok(None) => "unknown_bridge",
                        Err(error) => error.code.as_str(),
                    }
                };
                *counts.entry(outcome.to_string()).or_insert(0) += 1;
            }
        }
        eprintln!(
            "{middle}: {} cached arrivals × {} cached departures: {counts:?}",
            arrivals.len(),
            departures.len()
        );
        eprintln!(
            "{reported_conflicts} speed failures among {reported_bridges} bridges at the reported cue coordinates; {recovered_bridges} smooth recoveries across all cue pairs"
        );
        for example in examples.iter().take(3) {
            eprintln!("{example}");
        }
        assert!(
            reported_bridges > 0,
            "reported cues missing from pinned cache"
        );
    }

    pub(super) fn clock_edge(
        r: &mut Refiner<'_>,
        from: usize,
        to: usize,
        exit: u64,
        entry: u64,
    ) -> usize {
        let i = super::super::tests::edge(r, from, to, 30., 0.);
        let rate = r.search.policy.output_sample_rate_hz as u64;
        let (exit, entry, overlap) = (exit * rate, entry * rate, 20 * rate);
        let incoming_end = r.search.pool[to].format.frames;
        let edge = Arc::make_mut(&mut r.edges[i]);
        edge.exit = exit;
        edge.entry_end = entry + overlap;
        let plan = &mut edge.plan;
        plan.transitions[0].start_output_frame = exit;
        plan.transitions[0].end_output_frame = exit + overlap;
        let anchor = |source_frame, output_frame| Anchor {
            source_frame,
            output_frame,
        };
        plan.placements[0].time_map.anchors = vec![
            anchor(0, 0),
            anchor(exit, exit),
            anchor(exit + overlap, exit + overlap),
        ];
        plan.placements[1].source_start_frame = entry;
        plan.placements[1].time_map.anchors = vec![
            anchor(entry, exit),
            anchor(entry + overlap, exit + overlap),
            anchor(incoming_end, exit + incoming_end - entry),
        ];
        i
    }

    fn retime_pair(plan: &mut MixPlan, speed: f64) {
        let retime = |map: &mut TimeMap, output_origin: u64, speed: f64| {
            let source_origin = map.anchors[0].source_frame;
            for anchor in &mut map.anchors {
                anchor.output_frame = output_origin
                    + ((anchor.source_frame - source_origin) as f64 / speed).round() as u64;
            }
            map.speed_ratio_limits = [0.9, 1.1];
            map.max_speed_change_per_second = 0.001;
            map.validate().unwrap();
        };
        let outgoing = &mut plan.placements[0].time_map;
        retime(outgoing, 0, speed);
        let start = outgoing.anchors[1].output_frame;
        plan.transitions[0].start_output_frame = start;
        plan.transitions[0].end_output_frame = outgoing.anchors[2].output_frame;
        retime(&mut plan.placements[1].time_map, start, speed);
    }

    #[test]
    fn smooth_joined_map_recovers_a_speed_overshoot_without_moving_overlap_anchors() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[1800, 1250, 1800]);
        let enter = clock_edge(&mut r, 0, 1, 1780, 0);
        let leave = clock_edge(&mut r, 1, 2, 173, 0);
        let [mut arrival, mut departure] = [enter, leave].map(|id| r.edges[id].plan.clone());
        retime_pair(&mut arrival, 1.);
        retime_pair(&mut departure, 1.0564);
        for placement in [&mut arrival.placements[1], &mut departure.placements[0]] {
            placement.time_map.speed_ratio_limits = [0.94, 1.1];
            placement.time_map.max_speed_change_per_second = 0.02;
            placement.time_map.validate().unwrap();
        }
        let mut original = chain::measured_bridge(&arrival, &departure)
            .unwrap()
            .unwrap();
        original.map.speed_ratio_limits[1] = 1.057;
        assert_eq!(original.map.validate().unwrap_err().code, "TIME_MAP_SPEED");

        arrival.placements[1].time_map.speed_ratio_limits[1] = 1.057;
        departure.placements[0].time_map.speed_ratio_limits[1] = 1.057;
        let recovered = chain::measured_bridge(&arrival, &departure)
            .unwrap()
            .unwrap();
        assert_eq!(recovered.map.speed_ratio_limits, [0.94, 1.057]);
        assert_eq!(recovered.map.max_speed_change_per_second, 0.02);
        assert_eq!(recovered.solo_start, original.solo_start);
        assert_eq!(recovered.solo_end, original.solo_end);
        let overlap = |bridge: &chain::MeasuredBridge| {
            bridge
                .map
                .anchors
                .iter()
                .filter(|a| {
                    a.output_frame <= bridge.solo_start || a.output_frame >= bridge.solo_end
                })
                .map(|a| (a.source_frame, a.output_frame))
                .collect::<Vec<_>>()
        };
        assert_eq!(overlap(&original), overlap(&recovered));
        assert!(!chain::rules_out_joined_speed(&arrival, &departure));
    }

    #[test]
    fn measured_filter_matches_all_mock_cue_combinations_and_keeps_the_best() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[1800, 1250, 1800]);
        let arrivals =
            [(0, 0.95, 1.), (10, 1.05, 7.), (160, 0.95, 12.)].map(|(entry, speed, credit)| {
                let id = clock_edge(&mut r, 0, 1, 1780, entry);
                let edge = Arc::make_mut(&mut r.edges[id]);
                retime_pair(&mut edge.plan, speed);
                edge.dense_quality_seconds = credit;
                id
            });
        let departures =
            [(100, 1.05, 10.), (140, 0.95, 2.), (200, 1.05, 3.)].map(|(exit, speed, credit)| {
                let id = clock_edge(&mut r, 1, 2, exit, 0);
                let edge = Arc::make_mut(&mut r.edges[id]);
                retime_pair(&mut edge.plan, speed);
                edge.dense_quality_seconds = credit;
                id
            });
        // The 180-second runway can also complete the 0.10 speed change.
        // The late arrival overlaps the first two exits and gives the last 20 s.
        let expected = [
            [false, true, true],
            [true, false, true],
            [false, false, false],
        ];
        let cancel = Cancellation::default();
        let mut cp = CpSearch::default();
        for (i, &enter) in arrivals.iter().enumerate() {
            for (j, &leave) in departures.iter().enumerate() {
                let arrival = &r.edges[enter];
                let departure = &r.edges[leave];
                let bridge = chain::measured_bridge(&arrival.plan, &departure.plan);
                let passes = matches!(bridge, Ok(Some(_)))
                    && departure.exit.saturating_sub(arrival.entry_end) >= 45 * 48000;
                assert_eq!(
                    passes,
                    expected[i][j],
                    "bridge for {i}/{j}: {:?}",
                    bridge.err()
                );
                cp.check_joined_speeds(&r, &[Some(enter), Some(leave)]);
            }
        }
        let ProposalModel {
            mut master,
            choices,
            ..
        } = cp
            .proposal_model(&r, ProposalDomain::OriginalMaterial)
            .unwrap();
        let variable = |id| {
            r.search.pool.len()
                + choices
                    .iter()
                    .position(|c| *c == Choice::Measured(id))
                    .unwrap()
        };
        let solve =
            |master: &BinaryModel| scip::solve(master, Duration::from_secs(2), &cancel.0).unwrap();
        for (i, &enter) in arrivals.iter().enumerate() {
            for (j, &leave) in departures.iter().enumerate() {
                master.rows.push(Row {
                    terms: vec![(variable(enter), 1.), (variable(leave), 1.)],
                    lower: Some(2.),
                    upper: Some(2.),
                });
                let expected_stop = if expected[i][j] {
                    scip::Stop::Optimal
                } else {
                    scip::Stop::Infeasible
                };
                assert_eq!(solve(&master).stop, expected_stop, "model for {i}/{j}");
                master.rows.pop();
            }
        }
        let winner = solve(&master);
        assert_eq!(winner.stop, scip::Stop::Optimal);
        let chosen = winner.candidate.unwrap().selected;
        assert!(chosen.contains(&variable(arrivals[1])));
        assert!(chosen.contains(&variable(departures[0])));
    }

    pub(super) fn mock_proposal(r: &Refiner<'_>, proposal: ProposalDomain) -> Option<Route> {
        let mut cp = CpSearch::default();
        let route = cp
            .propose(
                r,
                proposal,
                Instant::now() + Duration::from_secs(2),
                &Cancellation::default(),
            )
            .unwrap();
        if route.is_none() {
            // An empty proposal caused by a timeout proves nothing about a filter.
            assert_eq!(cp.measured_only_stops.get("infeasible"), Some(&1));
        }
        route
    }

    #[test]
    fn original_clock_keeps_long_routes_and_replay_lane_retains_shorter_ones() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[1200; 3]);
        let first = clock_edge(&mut r, 0, 1, 1180, 0);
        let second = clock_edge(&mut r, 1, 2, 1180, 0);
        assert_eq!(
            mock_proposal(&r, ProposalDomain::OriginalMaterial),
            Some((vec![0, 1, 2], vec![Some(first), Some(second)]))
        );

        // The later entry discards 320 seconds without conflicting with the exit.
        r.edges.clear();
        let late = clock_edge(&mut r, 0, 1, 1180, 320);
        let second = clock_edge(&mut r, 1, 2, 1180, 0);
        assert!(mock_proposal(&r, ProposalDomain::OriginalMaterial).is_none());
        assert_eq!(
            mock_proposal(&r, ProposalDomain::MeasuredWithReplay),
            Some((vec![0, 1, 2], vec![Some(late), Some(second)]))
        );
    }

    #[test]
    fn measured_clock_rejects_only_the_route_that_cannot_reach_the_replay_floor() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[1200; 3]);
        let short = clock_edge(&mut r, 0, 1, 1180, 645);
        let long = clock_edge(&mut r, 0, 1, 1180, 500);
        let departure = clock_edge(&mut r, 1, 2, 1180, 0);
        for edge in [short, long, departure] {
            let plan = &mut Arc::make_mut(&mut r.edges[edge]).plan;
            plan.policy = policy.clone();
            plan.transitions.truncate(1);
            plan.transitions[0].from_track_id = plan.placements[0].track_id.clone();
            plan.transitions[0].to_track_id = plan.placements[1].track_id.clone();
            let outgoing = &mut plan.placements[0];
            outgoing.source_end_frame = outgoing.time_map.anchors.last().unwrap().source_frame;
        }
        // The fixed maps play at native speed. Charge both overlaps once.
        let max_clock = |entry| (3600. - entry - 40.) * (1. + MAX_REPLAY_FRACTION);
        assert!(max_clock(645.) < r.search.request.min());
        assert!(max_clock(500.) > r.search.request.min());

        let mut cp = CpSearch::default();
        let mut model = cp
            .proposal_model(&r, ProposalDomain::MeasuredWithReplay)
            .unwrap();
        assert!(cp.capacity_filter.measured_clock_row);
        assert_eq!(cp.capacity_filter.short_adjacent_combinations, 0);
        let cancel = Cancellation::default();
        for (edge, expected) in [(short, scip::Stop::Infeasible), (long, scip::Stop::Optimal)] {
            let join = model
                .choices
                .iter()
                .position(|&choice| choice == Choice::Measured(edge))
                .unwrap();
            model.master.rows.push(Row {
                terms: vec![(r.search.pool.len() + join, 1.)],
                lower: Some(1.),
                upper: Some(1.),
            });
            let result = scip::solve(&model.master, Duration::from_secs(2), &cancel.0).unwrap();
            assert_eq!(result.stop, expected);
            model.master.rows.pop();
        }

        cp.proposal_model(&r, ProposalDomain::WithUnresolved)
            .unwrap();
        assert!(!cp.capacity_filter.measured_clock_row);
        Arc::make_mut(&mut r.edges[short]).plan.placements[1]
            .time_map
            .anchors[1]
            .output_frame -= 1;
        cp.proposal_model(&r, ProposalDomain::MeasuredWithReplay)
            .unwrap();
        assert!(!cp.capacity_filter.measured_clock_row);
    }

    #[test]
    fn phrase_filter_checks_the_selected_arrival_against_the_selected_departure() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[1800, 1200, 1800]);
        clock_edge(&mut r, 0, 1, 1780, 1100);
        let departure = clock_edge(&mut r, 1, 2, 1150, 0);
        // There are enough raw frames, but the internal 30-second runway fails pacing.
        assert!(mock_proposal(&r, ProposalDomain::OriginalMaterial).is_none());
        let earlier = clock_edge(&mut r, 0, 1, 1780, 1030);
        assert_eq!(
            mock_proposal(&r, ProposalDomain::OriginalMaterial),
            Some((vec![0, 1, 2], vec![Some(earlier), Some(departure)]))
        );
    }

    #[test]
    fn quality_neighborhood_changes_the_following_track_to_keep_a_strong_join() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[1800, 1250, 1800, 1800]);
        let early_arrival = clock_edge(&mut r, 0, 1, 1780, 980);
        let strong = clock_edge(&mut r, 0, 1, 1780, 1080);
        Arc::make_mut(&mut r.edges[strong]).dense_quality_seconds = 22.;
        let early_exit = clock_edge(&mut r, 1, 2, 1120, 0);
        let later_exit = clock_edge(&mut r, 1, 3, 1180, 0);
        // The old neighbor is attractive enough to win in the generic model.
        Arc::make_mut(&mut r.edges[early_exit]).dense_quality_seconds = 20.;
        Arc::make_mut(&mut r.edges[later_exit]).score = 0.;
        assert_eq!(
            mock_proposal(&r, ProposalDomain::OriginalMaterial),
            Some((vec![0, 1, 2], vec![Some(early_arrival), Some(early_exit)]))
        );

        let mut cp = CpSearch::default();
        let edge = cp.quality_anchor(&r, false).unwrap();
        assert_eq!(edge, strong);
        let proposal = ProposalDomain::AroundMeasured {
            edge,
            allow_unresolved: false,
        };
        assert_eq!(
            cp.propose(
                &r,
                proposal,
                Instant::now() + Duration::from_secs(2),
                &Cancellation::default()
            )
            .unwrap(),
            Some((vec![0, 1, 3], vec![Some(strong), Some(later_exit)]))
        );
        assert_eq!(cp.quality_proposals[0].unresolved_joins, Some(0));
        assert_eq!(cp.quality_anchor(&r, false), Some(early_exit));
        assert_eq!(cp.quality_anchor(&r, true), Some(strong));
    }

    #[test]
    fn short_quality_route_retries_its_exact_anchor_on_the_original_clock() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[1200, 1200, 1000, 1500]);
        let anchor = clock_edge(&mut r, 0, 1, 1180, 0);
        let short = clock_edge(&mut r, 1, 2, 1000, 0);
        let long = clock_edge(&mut r, 1, 3, 1000, 0);
        Arc::make_mut(&mut r.edges[short]).dense_quality_seconds = 12.;
        let cancel = Cancellation::default();
        let mut cp = CpSearch::default();
        let relaxed = ProposalDomain::AroundMeasured {
            edge: anchor,
            allow_unresolved: false,
        };
        assert_eq!(
            cp.propose(
                &r,
                relaxed,
                Instant::now() + Duration::from_secs(2),
                &cancel
            )
            .unwrap(),
            Some((vec![0, 1, 2], vec![Some(anchor), Some(short)]))
        );
        let route_id = cp
            .closing
            .queue_assembled(
                &[anchor, short],
                r.closing_route_keys(&[anchor, short]).unwrap(),
                bookend_fixture_plan(&r),
                Some(anchor),
            )
            .unwrap();
        cp.note_quality_verification(relaxed, QualityVerification::QueuedForClosing { route_id });
        cp.closing
            .slice(&mut r, Instant::now() + Duration::from_secs(2), &cancel)
            .unwrap();
        let retry = cp.next_proposal(&r);
        assert!(retry == ProposalDomain::AroundMeasuredOriginal { edge: anchor });
        assert_eq!(
            mock_proposal(&r, retry),
            Some((vec![0, 1, 3], vec![Some(anchor), Some(long)]))
        );
        cp.quality_clock_retries.insert(anchor);
        assert!(cp.next_proposal(&r) != retry);
    }

    #[test]
    fn continuation_retry_leaves_other_arrivals_to_the_same_closing_song_selectable() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[1800, 1700, 600]);
        let anchor = clock_edge(&mut r, 0, 1, 1780, 0);
        let rejected = clock_edge(&mut r, 1, 2, 1550, 80);
        let earlier = clock_edge(&mut r, 1, 2, 1550, 40);
        Arc::make_mut(&mut r.edges[rejected]).dense_quality_seconds = 15.;
        let ordinary = ProposalDomain::AroundMeasured {
            edge: anchor,
            allow_unresolved: false,
        };
        assert_eq!(
            mock_proposal(&r, ordinary),
            Some((vec![0, 1, 2], vec![Some(anchor), Some(rejected)]))
        );
        let continuation = ProposalDomain::ContinueAfterClosingRejection {
            edge: anchor,
            terminal_join: rejected,
            allow_unresolved: false,
        };
        assert_eq!(
            mock_proposal(&r, continuation),
            Some((vec![0, 1, 2], vec![Some(anchor), Some(earlier)]))
        );
    }

    #[test]
    fn cue_repair_keeps_anchor_and_tries_single_edits_before_paired_edits() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[1200; 4]);
        let enter = clock_edge(&mut r, 0, 1, 1180, 40);
        let alt_enter = clock_edge(&mut r, 0, 1, 1110, 0);
        let leave = clock_edge(&mut r, 1, 2, 1180, 0);
        let alt_leave = clock_edge(&mut r, 1, 2, 1120, 30);
        let emergency = clock_edge(&mut r, 1, 2, 1170, 40);
        Arc::make_mut(&mut r.edges[emergency]).last_resort = true;
        let separate_anchor = clock_edge(&mut r, 2, 3, 1180, 0);
        assert_eq!(
            nearby_cue_choices(&r, (enter, leave), enter),
            vec![[enter, alt_leave]]
        );
        assert_eq!(
            nearby_cue_choices(&r, (enter, leave), separate_anchor),
            vec![
                [enter, alt_leave],
                [alt_enter, leave],
                [alt_enter, alt_leave]
            ]
        );
    }

    #[test]
    fn middle_substitution_uses_a_new_song_and_preserves_the_exact_anchor() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[1200; 5]);
        let enter = clock_edge(&mut r, 0, 1, 1180, 40);
        let leave = clock_edge(&mut r, 1, 2, 1180, 0);
        let anchor = clock_edge(&mut r, 2, 3, 1180, 0);
        let replacement_enter = clock_edge(&mut r, 0, 4, 1180, 0);
        let replacement_leave = clock_edge(&mut r, 4, 2, 1180, 0);
        clock_edge(&mut r, 0, 3, 1180, 0);
        clock_edge(&mut r, 3, 2, 1180, 0);
        let route = [enter, leave, anchor];
        assert_eq!(
            substitute_middle_choices(&r, &route, (enter, leave), anchor),
            vec![[replacement_enter, replacement_leave]]
        );
        assert!(substitute_middle_choices(&r, &route, (enter, leave), enter).is_empty());
    }

    #[test]
    fn quality_search_gets_the_first_turn_when_startup_already_found_an_incumbent() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[1200; 3]);
        let strong = clock_edge(&mut r, 0, 1, 1180, 0);
        Arc::make_mut(&mut r.edges[strong]).dense_quality_seconds = 12.;
        clock_edge(&mut r, 1, 2, 1180, 0);
        clock_edge(&mut r, 2, 0, 1180, 0);
        let mut cp = CpSearch::default();
        assert!(cp.next_proposal(&r) == ProposalDomain::OriginalMaterial);
        r.search.incumbent = Some(r.edges[strong].plan.clone());
        assert!(
            cp.next_proposal(&r)
                == ProposalDomain::AroundMeasured {
                    edge: strong,
                    allow_unresolved: false
                }
        );
        cp.proposal_turns = 4;
        assert!(
            cp.next_proposal(&r)
                == ProposalDomain::AroundMeasured {
                    edge: strong,
                    allow_unresolved: true
                }
        );
        cp.proposal_turns = 6;
        assert!(cp.next_proposal(&r) == ProposalDomain::MeasuredWithReplay);
    }

    #[test]
    fn measured_score_bound_skips_native_work_and_retains_mixed_turn() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[1200; 3]);
        for (from, to) in [(0, 1), (1, 2), (2, 0)] {
            let edge = clock_edge(&mut r, from, to, 1180, 0);
            let measured = Arc::make_mut(&mut r.edges[edge]);
            measured.score = 10.;
            measured.plan.transitions.truncate(1);
            r.search.graph[from * 3 + to].push(r.edges[edge].clone());
        }
        let mut incumbent = r.edges[0].plan.clone();
        incumbent
            .header
            .extensions
            .insert("set_composition".into(), json!({"objective": -0.4}));
        r.search.incumbent = Some(incumbent);
        let coverage = r.search.score_coverage();
        assert!(
            coverage["measured_routes_upper"].as_f64().unwrap_or(1.) < -0.4,
            "{coverage}"
        );
        let mut cp = CpSearch::default();
        cp.step(
            &mut r,
            Instant::now() + Duration::from_secs(2),
            &Cancellation::default(),
        )
        .unwrap();
        assert_eq!(cp.attempts, 0);
        assert_eq!(cp.measured_bound_skips, 1);
        assert!(cp.next_proposal(&r).allows_unresolved());
        assert_eq!(coverage["measured_assignment"]["minimum_joins"], 2);
        assert!(coverage["measured_routes_upper"].as_f64().unwrap() < -0.4);
        assert!(coverage["global_upper"].as_f64().unwrap() > 0.);

        Arc::make_mut(&mut r.search.graph[1][0]).plan.placements[0]
            .time_map
            .anchors[1]
            .output_frame -= 1;
        assert!(r.search.score_coverage()["measured_assignment"].is_null());
        assert!(r.search.measured_prune().is_none());
    }

    #[test]
    fn measured_capacity_closes_a_fork_until_its_bridge_is_measured() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[1200; 4]);
        let add = |r: &mut Refiner<'_>, from, to| {
            let edge = clock_edge(r, from, to, 1180, 0);
            Arc::make_mut(&mut r.edges[edge])
                .plan
                .transitions
                .truncate(1);
            r.search.graph[from * 4 + to].push(r.edges[edge].clone());
        };
        // Distinct incoming and outgoing owners have enough total clock
        // for the old assignment relaxation. Each actual path has two songs.
        add(&mut r, 0, 1);
        add(&mut r, 2, 1);
        add(&mut r, 2, 3);
        add(&mut r, 0, 3);
        let coverage = r.search.score_coverage();
        assert_eq!(coverage["measured_routes_empty"], true);
        assert_eq!(coverage["measured_directed_capacity"]["excluded_pairs"], 4);
        assert!(matches!(
            r.search.measured_prune(),
            Some(MeasuredPrune::DurationCapacity)
        ));
        let mut cp = CpSearch::default();
        cp.step(
            &mut r,
            Instant::now() + Duration::from_secs(2),
            &Cancellation::default(),
        )
        .unwrap();
        assert_eq!(cp.attempts, 0);
        assert_eq!(cp.measured_capacity_skips, 1);
        let model = CpSearch::default()
            .proposal_model(&r, ProposalDomain::WithUnresolved)
            .unwrap();
        assert!(model.choices.contains(&Choice::Unresolved(1, 2)));
        assert!(coverage["global_upper"].is_number());

        // The bridge yields the three-song path 0 → 1 → 2, whose full
        // source clock reaches the lower limit. The region must reopen.
        add(&mut r, 1, 2);
        let coverage = r.search.score_coverage();
        assert_eq!(coverage["measured_routes_empty"], false);
        assert_eq!(coverage["measured_directed_capacity"]["retained_tracks"], 4);
        assert_eq!(coverage["measured_assignment"]["minimum_joins"], 2);
        assert!(r.search.measured_prune().is_none());
    }

    #[test]
    fn unmeasured_clock_bound_reserves_runway_and_falls_back_for_unpinned_geometry() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[1200; 36]);
        let id = clock_edge(&mut r, 0, 1, 1180, 0);
        Arc::make_mut(&mut r.edges[id]).plan.transitions.truncate(1);
        r.search.graph[1].push(r.edges[id].clone());

        let evidence = r.search.score_coverage();
        let paced = evidence["unmeasured_paced_clock_upper"].as_f64().unwrap();
        assert!((40. ..48.).contains(&paced), "{evidence}");
        assert_eq!(evidence["global_upper"], paced);

        // Stored cue counters that don't agree with the assembled anchor
        // cannot be used to reserve the phrase-pacing clock.
        Arc::make_mut(&mut r.search.graph[1][0]).entry_end += 1;
        let fallback = r.search.score_coverage();
        assert!(fallback["unmeasured_paced_clock_upper"].is_null());
        assert!(fallback["global_upper"].as_f64().unwrap() >= 60.);
    }

    #[test]
    fn forced_chain_prunes_competing_joins_in_the_mixed_model_and_schedule() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[1200; 4]);
        r.search.required = required_pairs::RequiredPairs::new(
            required_pairs::Mode::Either,
            [[0, 1], [1, 2]]
                .map(|indices| required_pairs::Pair {
                    indices,
                    spotify_ids: indices.map(|i| format!("spotify-{i}")),
                    track_ids: indices.map(|i| r.search.pool[i].track_id.clone()),
                })
                .to_vec(),
            4,
        )
        .unwrap();
        r.search.capacity_proof = r.search.directed_capacity();
        assert!(r.search.schedules_transition(2, 1));
        assert!(r.search.schedules_transition(0, 3));
        r.search.excluded_transitions.insert((1, 0));
        r.search.capacity_proof = r.search.directed_capacity();
        assert!(r.search.structural_infeasibility().is_none());
        assert!(r.search.permits_transition(0, 3));
        let model = CpSearch::default()
            .proposal_model(&r, ProposalDomain::WithUnresolved)
            .unwrap();
        for pair in [(0, 1), (1, 2), (2, 3), (3, 0)] {
            assert!(r.search.schedules_transition(pair.0, pair.1));
            assert!(model.choices.contains(&Choice::Unresolved(pair.0, pair.1)));
        }
        for pair in [(2, 1), (0, 3), (3, 2)] {
            assert!(!r.search.schedules_transition(pair.0, pair.1));
            assert!(!model.choices.contains(&Choice::Unresolved(pair.0, pair.1)));
        }
        assert_eq!(r.search.stats.measured_windows, 0);
    }

    #[test]
    fn forced_chain_capacity_preserves_every_feasible_four_song_route() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[850, 1100, 800, 1300]);
        r.search.required = required_pairs::RequiredPairs::new(
            required_pairs::Mode::Either,
            [[0, 1], [1, 2]]
                .map(|indices| required_pairs::Pair {
                    indices,
                    spotify_ids: indices.map(|i| format!("spotify-{i}")),
                    track_ids: indices.map(|i| r.search.pool[i].track_id.clone()),
                })
                .to_vec(),
            4,
        )
        .unwrap();
        let edges = (0..4)
            .flat_map(|from| {
                (0..4)
                    .filter(move |&to| from != to)
                    .map(move |to| (from, to))
            })
            .collect::<Vec<_>>();
        // The three required songs cannot reach the floor on their own.
        // The fourth can only appear immediately before or after the chain.
        let routes = [[3, 0, 1, 2], [0, 1, 2, 3], [3, 2, 1, 0], [2, 1, 0, 3]];
        let mut checked = 0;
        for mask in 0..1 << edges.len() {
            r.search.excluded_transitions = edges
                .iter()
                .enumerate()
                .filter_map(|(bit, &edge)| (mask & (1 << bit) == 0).then_some(edge))
                .collect();
            let legal = routes
                .iter()
                .filter(|route| {
                    route
                        .windows(2)
                        .all(|pair| !r.search.excluded_transitions.contains(&(pair[0], pair[1])))
                })
                .collect::<Vec<_>>();
            r.search.capacity_proof = r.search.directed_capacity();
            for route in legal {
                assert!(
                    r.search.structural_infeasibility().is_none(),
                    "graph {mask}, route {route:?}"
                );
                for pair in route.windows(2) {
                    assert!(
                        r.search.schedules_transition(pair[0], pair[1]),
                        "graph {mask}, route {route:?}"
                    );
                }
                checked += 1;
            }
        }
        assert_eq!(checked, 4 * (1 << 9));
    }

    #[test]
    fn proposal_model_uses_duration_capacity_before_any_window_is_measured() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let r = super::super::tests::fixture(&store, &policy, &[720; 6]);
        assert_eq!(
            r.search.directed_capacity().unwrap().minimum_tracks,
            Some(4)
        );
        let mut cp = CpSearch::default();
        let model = cp
            .proposal_model(&r, ProposalDomain::WithUnresolved)
            .unwrap();
        let track_terms = (0..6).map(|track| (track, 1.)).collect::<Vec<_>>();
        assert!(model
            .master
            .rows
            .iter()
            .any(|row| row.terms == track_terms && row.lower == Some(4.)));
        assert_eq!(r.search.stats.measured_windows, 0);
    }

    #[test]
    fn proposal_model_eliminates_a_directed_fork_before_any_window_is_measured() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[1200; 3]);
        r.search
            .excluded_transitions
            .extend([(1, 0), (1, 2), (2, 0), (2, 1)]);
        let proof = r.search.directed_capacity().unwrap();
        assert_eq!(proof.viable_tracks, 0);
        assert_eq!(proof.iterations, 2);
        let mut cp = CpSearch::default();
        let model = cp
            .proposal_model(&r, ProposalDomain::WithUnresolved)
            .unwrap();
        let solved = scip::solve(
            &model.master,
            Duration::from_secs(1),
            &Cancellation::default().0,
        )
        .unwrap();
        assert_eq!(solved.stop, scip::Stop::Infeasible);
        assert_eq!(r.search.stats.measured_windows, 0);
    }

    #[test]
    fn proposal_model_disables_a_measured_shortcut_without_disabling_its_tracks() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[1200; 3]);
        r.search
            .excluded_transitions
            .extend([(1, 0), (2, 0), (2, 1)]);
        let edge = clock_edge(&mut r, 0, 2, 1180, 0);
        let model = CpSearch::default()
            .proposal_model(&r, ProposalDomain::WithUnresolved)
            .unwrap();
        assert!(!model.choices.contains(&Choice::Unresolved(0, 2)));
        assert!(model.choices.contains(&Choice::Unresolved(0, 1)));
        assert!(model.choices.contains(&Choice::Unresolved(1, 2)));
        let variable = 3 + model
            .choices
            .iter()
            .position(|choice| *choice == Choice::Measured(edge))
            .unwrap();
        assert!(model
            .master
            .rows
            .iter()
            .any(|row| row.terms == [(variable, 1.)] && row.upper == Some(0.)));
        let solved = scip::solve(
            &model.master,
            Duration::from_secs(1),
            &Cancellation::default().0,
        )
        .unwrap();
        let chosen = solved.candidate.unwrap().selected;
        assert!((0..3).all(|track| chosen.contains(&track)));
        assert!(!chosen.contains(&variable));
    }

    #[test]
    fn window_capacity_excludes_only_the_exact_cue_that_is_too_short() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[1200; 3]);
        let short = clock_edge(&mut r, 0, 1, 100, 1000);
        let full = clock_edge(&mut r, 0, 1, 1180, 0);
        let unknown = clock_edge(&mut r, 0, 1, 120, 1000);
        for id in [short, full, unknown] {
            let plan = &mut Arc::make_mut(&mut r.edges[id]).plan;
            plan.policy = policy.clone();
            plan.transitions.truncate(1);
            plan.transitions[0].from_track_id = plan.placements[0].track_id.clone();
            plan.transitions[0].to_track_id = plan.placements[1].track_id.clone();
            if id != unknown {
                let outgoing = &mut plan.placements[0];
                outgoing.source_end_frame = outgoing.time_map.anchors.last().unwrap().source_frame;
            }
            r.search.graph[1].push(r.edges[id].clone());
        }
        r.search.capacity_proof = r.search.directed_capacity();
        let proof = r.search.capacity_proof.as_ref().unwrap();
        assert!(proof.permits_pair(0, 1));
        assert_eq!(proof.other_tracks_at_pair(0, 1), 0b100);
        assert_eq!(
            r.search.measured_window_capacity_counts(proof),
            json!({"checked": 2, "excluded": 1, "unresolved_geometry": 1})
        );

        let model = CpSearch::default()
            .proposal_model(&r, ProposalDomain::WithUnresolved)
            .unwrap();
        assert!(model.choices.contains(&Choice::Unresolved(0, 1)));
        for (id, excluded) in [(short, true), (full, false), (unknown, false)] {
            let variable = 3 + model
                .choices
                .iter()
                .position(|choice| *choice == Choice::Measured(id))
                .unwrap();
            assert_eq!(
                model
                    .master
                    .rows
                    .iter()
                    .any(|row| row.terms == [(variable, 1.)] && row.upper == Some(0.)),
                excluded
            );
        }
        r.edges.clear();
        assert!(r.sync_edges().unwrap());
        assert_eq!(r.edges.len(), 2);
        assert!(Arc::ptr_eq(&r.edges[0], &r.search.graph[1][1]));
        assert!(Arc::ptr_eq(&r.edges[1], &r.search.graph[1][2]));
        assert_eq!(r.search.graph[1].len(), 3);
        assert_eq!(r.search.stats.measured_windows, 0);
    }

    #[test]
    fn adjacent_measured_cues_can_be_too_short_even_when_each_cue_fits() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[1100; 3]);
        let arrival = clock_edge(&mut r, 0, 1, 1080, 350);
        let departure = clock_edge(&mut r, 1, 2, 730, 0);
        let alternative = clock_edge(&mut r, 1, 2, 990, 0);
        for id in [arrival, departure, alternative] {
            let plan = &mut Arc::make_mut(&mut r.edges[id]).plan;
            plan.policy = policy.clone();
            plan.transitions.truncate(1);
            plan.transitions[0].from_track_id = plan.placements[0].track_id.clone();
            plan.transitions[0].to_track_id = plan.placements[1].track_id.clone();
            let outgoing = &mut plan.placements[0];
            outgoing.source_end_frame = outgoing.time_map.anchors.last().unwrap().source_frame;
        }
        let proof = r.search.directed_capacity().unwrap();
        for (edge, from, to) in [(arrival, 0, 1), (departure, 1, 2), (alternative, 1, 2)] {
            assert!(
                r.search
                    .window_capacity_upper(&r.edges[edge].plan, &proof, from, to)
                    .unwrap()
                    >= r.search.request.min()
            );
        }
        let upper = |exit: usize| {
            r.search
                .adjacent_windows_capacity_upper(
                    &r.edges[arrival].plan,
                    &r.edges[exit].plan,
                    &proof,
                    [0, 1, 2],
                )
                .unwrap()
        };
        assert!(upper(departure) < r.search.request.min());
        assert!(upper(alternative) >= r.search.request.min());
        let mut cp = CpSearch::default();
        let model = cp
            .proposal_model(&r, ProposalDomain::WithUnresolved)
            .unwrap();
        assert_eq!(cp.capacity_filter.short_adjacent_combinations, 1);
        let has_conflict = |exit| {
            let variable = |choice| 3 + model.choices.iter().position(|&id| id == choice).unwrap();
            let terms = [
                (variable(Choice::Measured(arrival)), 1.),
                (variable(exit), 1.),
            ];
            model
                .master
                .rows
                .iter()
                .any(|row| row.terms == terms && row.upper == Some(1.))
        };
        assert!(has_conflict(Choice::Measured(departure)));
        assert!(!has_conflict(Choice::Measured(alternative)));
        assert!(!has_conflict(Choice::Unresolved(1, 2)));
        let mut malformed = r.edges[arrival].plan.clone();
        malformed.placements[1].time_map.anchors[1].output_frame -= 1;
        assert!(r
            .search
            .adjacent_windows_capacity_upper(
                &malformed,
                &r.edges[departure].plan,
                &proof,
                [0, 1, 2]
            )
            .is_none());
    }

    #[test]
    fn adjacent_capacity_allows_the_same_cues_when_a_successor_can_supply_the_clock() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[1100, 1100, 1100, 1000]);
        let arrival = clock_edge(&mut r, 0, 1, 1080, 350);
        let departure = clock_edge(&mut r, 1, 2, 730, 0);
        let successor = clock_edge(&mut r, 2, 3, 1080, 0);
        for id in [arrival, departure, successor] {
            let plan = &mut Arc::make_mut(&mut r.edges[id]).plan;
            plan.policy = policy.clone();
            plan.transitions.truncate(1);
            plan.transitions[0].from_track_id = plan.placements[0].track_id.clone();
            plan.transitions[0].to_track_id = plan.placements[1].track_id.clone();
            let outgoing = &mut plan.placements[0];
            outgoing.source_end_frame = outgoing.time_map.anchors.last().unwrap().source_frame;
        }
        r.search.excluded_transitions = (0..4)
            .flat_map(|from| (0..4).map(move |to| (from, to)))
            .filter(|pair| !matches!(pair, (0, 1) | (1, 2)))
            .collect();

        // At native speed, the pinned three-track route plays 2,560 seconds.
        // A 6% slowdown and 15% replay still put it below 57 minutes. The
        // successor contributes 980 new seconds after its overlap.
        let native_clock = 1100. + (750. - 350.) + 1100. - 2. * 20.;
        assert!(native_clock / 0.94 * 1.15 < r.search.request.min());
        let extended_clock = native_clock + 1000. - 20.;
        assert!((r.search.request.min()..=r.search.request.max()).contains(&extended_clock));

        let cancel = Cancellation::default();
        for (can_extend, expected_stop) in
            [(false, scip::Stop::Infeasible), (true, scip::Stop::Optimal)]
        {
            if can_extend {
                r.search.excluded_transitions.remove(&(2, 3));
            }
            r.search.capacity_proof = r.search.directed_capacity();
            let mut cp = CpSearch::default();
            let mut model = cp
                .proposal_model(&r, ProposalDomain::MeasuredWithReplay)
                .unwrap();
            let terms = [arrival, departure]
                .map(|edge| {
                    let join = model
                        .choices
                        .iter()
                        .position(|choice| *choice == Choice::Measured(edge))
                        .unwrap();
                    (r.search.pool.len() + join, 1.)
                })
                .to_vec();
            assert_eq!(
                model
                    .master
                    .rows
                    .iter()
                    .any(|row| row.terms == terms && row.upper == Some(1.)),
                !can_extend
            );
            model.master.rows.push(Row {
                terms,
                lower: Some(2.),
                upper: Some(2.),
            });
            let solved = scip::solve(&model.master, Duration::from_secs(1), &cancel.0).unwrap();
            assert_eq!(solved.stop, expected_stop);
            if can_extend {
                let selected = solved.candidate.unwrap().selected;
                let join = model
                    .choices
                    .iter()
                    .position(|choice| *choice == Choice::Measured(successor))
                    .unwrap();
                assert!(selected.contains(&(r.search.pool.len() + join)));
            }
        }
        assert_eq!(r.search.stats.measured_windows, 0);
    }

    #[test]
    fn measured_only_capacity_reopens_cues_when_a_successor_is_admitted() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[1100, 1100, 1100, 1000]);
        let add = |r: &mut Refiner<'_>, from, to, exit, entry| {
            let id = clock_edge(r, from, to, exit, entry);
            let plan = &mut Arc::make_mut(&mut r.edges[id]).plan;
            plan.policy = policy.clone();
            plan.transitions.truncate(1);
            plan.transitions[0].from_track_id = plan.placements[0].track_id.clone();
            plan.transitions[0].to_track_id = plan.placements[1].track_id.clone();
            let outgoing = &mut plan.placements[0];
            outgoing.source_end_frame = outgoing.time_map.anchors.last().unwrap().source_frame;
            id
        };
        let arrival = add(&mut r, 0, 1, 1080, 350);
        let departure = add(&mut r, 1, 2, 730, 0);
        r.search.capacity_proof = r.search.directed_capacity();
        let solve_with_cues = |mut model: ProposalModel| {
            let variable = |choice| 4 + model.choices.iter().position(|&id| id == choice).unwrap();
            let terms = [arrival, departure].map(|edge| (variable(Choice::Measured(edge)), 1.));
            model.master.rows.push(Row {
                terms: terms.to_vec(),
                lower: Some(2.),
                upper: Some(2.),
            });
            let result = scip::solve(
                &model.master,
                Duration::from_secs(1),
                &Cancellation::default().0,
            )
            .unwrap();
            (
                result.stop,
                result.candidate.map(|candidate| {
                    candidate
                        .selected
                        .into_iter()
                        .filter_map(|i| {
                            i.checked_sub(4).and_then(|j| model.choices.get(j)).copied()
                        })
                        .collect::<Vec<_>>()
                }),
            )
        };
        let mut cp = CpSearch::default();
        let measured = cp
            .proposal_model(&r, ProposalDomain::MeasuredWithReplay)
            .unwrap();
        assert_eq!(cp.capacity_filter.measured_scope_pairs, Some(2));
        assert_eq!(cp.capacity_filter.excluded_measured_choices, 0);
        assert_eq!(cp.capacity_filter.short_measured_windows, 0);
        assert_eq!(cp.capacity_filter.short_adjacent_combinations, 1);
        assert_eq!(solve_with_cues(measured).0, scip::Stop::Infeasible);

        let mixed = cp
            .proposal_model(&r, ProposalDomain::WithUnresolved)
            .unwrap();
        assert_eq!(cp.capacity_filter.measured_scope_pairs, None);
        assert_eq!(cp.capacity_filter.short_adjacent_combinations, 0);
        let (stop, choices) = solve_with_cues(mixed);
        assert_eq!(stop, scip::Stop::Optimal);
        assert!(choices
            .unwrap()
            .iter()
            .any(|choice| matches!(choice, Choice::Unresolved(..))));

        let successor = add(&mut r, 2, 3, 1080, 0);
        let measured = cp
            .proposal_model(&r, ProposalDomain::MeasuredWithReplay)
            .unwrap();
        assert_eq!(cp.capacity_filter.measured_scope_pairs, Some(3));
        assert_eq!(cp.capacity_filter.short_adjacent_combinations, 0);
        let (stop, choices) = solve_with_cues(measured);
        assert_eq!(stop, scip::Stop::Optimal);
        assert!(choices.unwrap().contains(&Choice::Measured(successor)));
        assert_eq!(r.search.stats.measured_windows, 0);
    }

    #[test]
    fn measured_capacity_conflict_survives_resume_and_keeps_extendable_cues() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[1000, 910, 910, 910, 30, 260]);
        let early = clock_edge(&mut r, 0, 1, 60, 0);
        let late = clock_edge(&mut r, 0, 1, 980, 0);
        let middle = clock_edge(&mut r, 1, 2, 890, 0);
        let last = clock_edge(&mut r, 2, 3, 890, 0);
        for id in [early, late, middle, last] {
            let plan = &mut Arc::make_mut(&mut r.edges[id]).plan;
            plan.policy = policy.clone();
            plan.transitions.truncate(1);
            plan.transitions[0].from_track_id = plan.placements[0].track_id.clone();
            plan.transitions[0].to_track_id = plan.placements[1].track_id.clone();
            let outgoing = &mut plan.placements[0];
            outgoing.source_end_frame = outgoing.time_map.anchors.last().unwrap().source_frame;
        }
        r.search.excluded_transitions = (0..6)
            .flat_map(|from| (0..6).map(move |to| (from, to)))
            .filter(|pair| !matches!(pair, (4, 0) | (0, 1) | (1, 2) | (2, 3)))
            .collect();
        r.search.capacity_proof = r.search.directed_capacity();

        // The 30-second predecessor cannot rescue the early run even
        // before charging its overlap. A later cue leaves 3,670 seconds
        // and can reach the hour window with a musical bookend trim.
        let native = 80. + 3. * 910. - 3. * 20.;
        assert!((native + 30.) / 0.94 * 1.15 < r.search.request.min());
        let trimmed_late = native + (980. - 60.) - 90.;
        assert!((r.search.request.min()..=r.search.request.max()).contains(&trimmed_late));
        let cancel = Cancellation::default();
        let mut cp = CpSearch::default();
        cp.restore_conflicts(&mut r, &cancel).unwrap();
        cp.proposal_model(&r, ProposalDomain::WithUnresolved)
            .unwrap();
        assert_eq!(cp.capacity_filter.short_adjacent_combinations, 0);
        assert!(cp
            .learn_short_subpath(&r, &[Some(late), Some(middle), Some(last)])
            .is_none());
        assert!(cp
            .learn_short_subpath(&r, &[Some(early), None, Some(last)])
            .is_none());
        let (len, upper) = cp
            .learn_short_subpath(&r, &[None, Some(early), Some(middle), Some(last)])
            .unwrap();
        assert_eq!(len, 3);
        assert!(upper < r.search.request.min());
        cp.save_conflicts(&r).unwrap();

        let mut resumed = CpSearch::default();
        resumed.restore_conflicts(&mut r, &cancel).unwrap();
        assert_eq!(resumed.restored_capacity_subpaths, 1);
        assert_eq!(
            resumed.capacity_subpaths,
            BTreeSet::from([vec![early, middle, last]])
        );
        let model = resumed
            .proposal_model(&r, ProposalDomain::WithUnresolved)
            .unwrap();
        let variable = |choice| {
            r.search.pool.len() + model.choices.iter().position(|&id| id == choice).unwrap()
        };
        assert!(model.choices.contains(&Choice::Unresolved(4, 0)));
        assert!(model.master.rows.iter().any(|row| row.terms
            == [early, middle, last].map(|edge| (variable(Choice::Measured(edge)), 1.))
            && row.upper == Some(2.)));
        let solved = scip::solve(&model.master, Duration::from_secs(1), &cancel.0).unwrap();
        let selected = solved.candidate.unwrap().selected;
        for edge in [late, middle, last] {
            assert!(selected.contains(&variable(Choice::Measured(edge))));
        }

        // With another possible successor, the old row has no duration
        // witness. A restore must recheck the proof before using that row.
        r.search.excluded_transitions.remove(&(3, 5));
        r.search.capacity_proof = r.search.directed_capacity();
        let mut expanded = CpSearch::default();
        expanded.restore_conflicts(&mut r, &cancel).unwrap();
        assert!(expanded.capacity_subpaths.is_empty());
        assert_eq!(r.search.stats.measured_windows, 0);
    }

    #[test]
    fn exact_overlap_is_charged_once_before_the_replay_allowance() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[935; 3]);
        let long = clock_edge(&mut r, 0, 1, 915, 0);
        let short = clock_edge(&mut r, 0, 1, 931, 0);
        let rate = u64::from(policy.output_sample_rate_hz);
        for (edge, overlap) in [(long, 20), (short, 4)] {
            let plan = &mut Arc::make_mut(&mut r.edges[edge]).plan;
            plan.policy = policy.clone();
            plan.transitions.truncate(1);
            let [outgoing, incoming] = plan.placements.as_mut_slice() else {
                panic!("expected two placements");
            };
            let transition = &mut plan.transitions[0];
            transition.from_track_id = outgoing.track_id.clone();
            transition.to_track_id = incoming.track_id.clone();
            transition.end_output_frame = 935 * rate;
            outgoing.source_end_frame = 935 * rate;
            let last = outgoing.time_map.anchors.last_mut().unwrap();
            last.source_frame = outgoing.source_end_frame;
            last.output_frame = transition.end_output_frame;
            incoming.time_map.anchors[1].source_frame = overlap * rate;
            incoming.time_map.anchors[1].output_frame = transition.end_output_frame;
            incoming.source_end_frame = 935 * rate;
            plan.total_output_frames = incoming.time_map.anchors.last().unwrap().output_frame;
        }
        let proof = r.search.directed_capacity().unwrap();
        assert!(proof.permits_pair(0, 1));
        let upper = |edge: usize| {
            r.search
                .window_capacity_upper(&r.edges[edge].plan, &proof, 0, 1)
                .unwrap()
        };
        assert!((upper(short) - upper(long) - 16. * 1.15).abs() < 1e-8);
        assert!(upper(long) < r.search.request.min());
        assert!(upper(short) > r.search.request.min());
        let model = CpSearch::default()
            .proposal_model(&r, ProposalDomain::WithUnresolved)
            .unwrap();
        let fixed_off = |choice| {
            let variable = 3 + model.choices.iter().position(|&id| id == choice).unwrap();
            model
                .master
                .rows
                .iter()
                .any(|row| row.terms == [(variable, 1.)] && row.upper == Some(0.))
        };
        assert!(fixed_off(Choice::Measured(long)));
        assert!(!fixed_off(Choice::Measured(short)));
        assert!(!fixed_off(Choice::Unresolved(0, 1)));
        let mut unknown = r.edges[long].plan.clone();
        unknown.placements[1].time_map.output_sample_rate_hz = 44100;
        assert!(r
            .search
            .window_capacity_upper(&unknown, &proof, 0, 1)
            .is_none());
    }

    #[test]
    fn pinned_corrections_remove_only_the_forbidden_direction_and_tighten_the_bound() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[1200; 6]);
        let cancel = Cancellation::default();
        let ids = r
            .search
            .pool
            .iter()
            .map(|t| t.track_id.clone())
            .collect::<Vec<_>>();
        for (from, track) in r.search.pool.iter_mut().enumerate() {
            let mut overrides = store.overrides(track).unwrap();
            overrides.revision += 1;
            overrides.operations = ids
                .iter()
                .enumerate()
                .filter(|(to, _)| from / 3 != to / 3 && (from, *to) != (3, 2))
                .map(|(_, to)| Correction::ExcludeTransition {
                    to_track_id: to.clone(),
                })
                .collect();
            track.active_overrides = Some(
                store
                    .write(&format!("corrections/{from}.json"), &overrides)
                    .unwrap(),
            );
            track.override_revision = overrides.revision;
        }
        r.search.excluded_transitions =
            read_transition_exclusions(&r.search.snapshot, &r.search.pool, &cancel).unwrap();
        assert_eq!(r.search.excluded_transitions.len(), 17);
        assert!(r.search.permits_transition(3, 2));
        assert!(!r.search.permits_transition(2, 3));
        let mut cp = CpSearch::default();
        let model = cp
            .proposal_model(&r, ProposalDomain::WithUnresolved)
            .unwrap();
        assert!(model.choices.contains(&Choice::Unresolved(3, 2)));
        assert!(!model.choices.contains(&Choice::Unresolved(2, 3)));
        assert!(r.search.proposals(2, 3, 0, 1, &cancel).unwrap().is_empty());
        assert_eq!(r.search.stats.proposed_pairs, 0);

        let joined = r.search.score_coverage()["global_upper"].as_f64().unwrap();
        r.search.excluded_transitions.insert((3, 2));
        let split = r.search.score_coverage()["global_upper"].as_f64().unwrap();
        assert!(split < joined / 2., "split {split}, joined {joined}");
    }

    #[test]
    fn measured_joined_speed_filter_uses_the_renderer_time_map_limits() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[1800, 1250, 1800]);
        let enter = clock_edge(&mut r, 0, 1, 1780, 0);
        let leave = clock_edge(&mut r, 1, 2, 100, 0);
        let arrival = &mut Arc::make_mut(&mut r.edges[enter]).plan;
        retime_pair(arrival, 0.95);
        let departure = &mut Arc::make_mut(&mut r.edges[leave]).plan;
        retime_pair(departure, 1.05);
        assert!(chain::rules_out_joined_speed(
            &r.edges[enter].plan,
            &r.edges[leave].plan
        ));
        let mut cp = CpSearch::default();
        let cancel = Cancellation::default();
        cp.restore_conflicts(&mut r, &cancel).unwrap();
        let solve = |r: &Refiner<'_>, cp: &mut CpSearch| {
            cp.propose(
                r,
                ProposalDomain::OriginalMaterial,
                Instant::now() + Duration::from_secs(2),
                &cancel,
            )
            .unwrap()
        };
        assert!(solve(&r, &mut cp).is_none());
        assert_eq!(cp.joined_speed_rejected_proposals, 1);
        assert_eq!(cp.joined_speed_conflicts, 0);
        assert!(solve(&r, &mut cp).is_none());
        assert_eq!(cp.joined_speed_conflicts, 1);
        assert_eq!(cp.original_material_stops.get("infeasible"), Some(&1));
        cp.save_conflicts(&r).unwrap();
        let mut resumed = CpSearch::default();
        resumed.restore_conflicts(&mut r, &cancel).unwrap();
        assert_eq!(resumed.restored_joined_speeds, 1);
        assert!(solve(&r, &mut resumed).is_none());
        assert_eq!(resumed.joined_speed_conflicts, 1);
        assert_eq!(resumed.joined_speed_rejected_proposals, 0);

        Arc::make_mut(&mut r.edges[enter]).dense_quality_seconds = 12.;
        r.search.incumbent = Some(r.edges[enter].plan.clone());
        let mut quality = CpSearch::default();
        quality
            .step(&mut r, Instant::now() + Duration::from_secs(2), &cancel)
            .unwrap();
        assert_eq!(quality.proposal_turns, 1);
        assert_eq!(quality.attempts, 2);
        assert_eq!(quality.quality_speed_retries, 1);
        assert_eq!(quality.quality_proposals[1].stop, "infeasible");
        assert!(matches!(
            quality.quality_proposals[0].verification,
            Some(QualityVerification::JoinedSpeedConflict { .. })
        ));

        let departure = &mut Arc::make_mut(&mut r.edges[leave]).plan;
        retime_pair(departure, 0.95);
        assert!(!chain::rules_out_joined_speed(
            &r.edges[enter].plan,
            &r.edges[leave].plan
        ));
        assert_eq!(
            mock_proposal(&r, ProposalDomain::OriginalMaterial),
            Some((vec![0, 1, 2], vec![Some(enter), Some(leave)]))
        );
    }

    #[test]
    fn only_revalidated_measured_prefixes_restore_from_checkpoint() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut refiner = super::super::tests::fixture(&store, &policy, &[360; 4]);
        let reject = super::super::tests::edge(&mut refiner, 0, 1, 30., 0.);
        assert!(matches!(
            refiner.assemble_measured_for_closing(&[reject]).unwrap(),
            Err(AssemblyRejection::Prefix { len: 1, .. })
        ));
        let mut cp = CpSearch::default();
        let cancel = Cancellation::default();
        cp.restore_conflicts(&mut refiner, &cancel).unwrap();
        let key = cp.conflict_checkpoint_key.as_ref().unwrap();
        let edge = refiner.persistent_key(reject).unwrap();
        refiner
            .search
            .cache
            .write(
                &store,
                "cp-measured-conflicts",
                key,
                &SavedConflicts {
                    prefixes: vec![vec![edge], vec!["stale-edge".to_string()], vec![]],
                    joined_speeds: vec![],
                    capacity_subpaths: vec![],
                },
            )
            .unwrap();
        let mut resumed = CpSearch::default();
        resumed.restore_conflicts(&mut refiner, &cancel).unwrap();
        assert_eq!(resumed.rejected_prefixes, BTreeSet::from([vec![reject]]));
        assert_eq!(resumed.restored_prefixes, 1);
    }
}
