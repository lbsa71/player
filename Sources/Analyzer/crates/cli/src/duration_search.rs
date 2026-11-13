//! Shared measured-join library and approved-plan persistence for complete-set refinement.
use super::*;
use crate::{
    composition::{Context, Request},
    search_cache::Cache,
    tactics::MeasuredEdge,
};
use mixlib_planner::{
    BaselineCueIndex, CandidateEvidenceCache, EventCueIndex, StitchRequest, TrackPacing,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
    time::Instant,
};

#[path = "duration_measurement.rs"]
mod measurement;
use measurement::remove_ids;
pub(super) use measurement::{measure_candidate, window_key, Measurement};

#[path = "duration_proposals.rs"]
mod proposal;

#[path = "duration_continuations.rs"]
mod continuation;

#[path = "duration_oracle.rs"]
mod oracle;

#[derive(Default, Serialize)]
struct Stats {
    prepared_tracks: usize,
    pool_seconds: f64,
    proposed_pairs: usize,
    proposed_windows: usize,
    measured_windows: usize,
    cached_windows: usize,
    selector_refresh_attempts: usize,
    in_flight_pcm_overrun_seconds: f64,
    cached_proposals: usize,
    capacity_skipped_windows: usize,
    admitted_choices: usize,
    known_good_choices: usize,
    duplicate_measured_choices: usize,
    complete_candidates: usize,
    improvements: usize,
}

fn seconds(track: &Track, frames: u64) -> f64 {
    frames as f64 / f64::from(track.format.sample_rate_hz)
}

const MEASURED_CREDIT_CAP: f64 = mixlib_planner::MAX_TACTIC_CONTINUATION_SECONDS + 1e-6;
const OVERLAP_CREDIT_SLACK_SECONDS: f64 = 1e-6;

#[cfg(feature = "cp-search")]
struct MeasuredCoverage {
    assignment: mixlib_set_search::coverage::MeasuredAssignment,
    proof: mixlib_set_search::coverage::DirectedCapacity,
}

#[cfg(feature = "cp-search")]
enum MeasuredPrune {
    DurationCapacity,
    ScoreBound,
}

#[cfg(feature = "cp-search")]
enum StructuralInfeasibility {
    DirectedCapacity,
    RequiredChain,
}

fn complete_set_score_upper(request: Request, credit_upper: f64) -> Option<f64> {
    mixlib_set_search::bounds::ScoreEnvelope {
        min_duration_seconds: request.min(),
        max_duration_seconds: request.max(),
        sustained_quality_upper: credit_upper,
        transition_shortfall_lower: 0.,
        replay_seconds_lower: 0.,
        arc_error_lower: 0.,
        development_penalty_lower: 0.,
    }
    .upper_bound()
}

fn unrestricted_score_upper(tracks: usize, request: Request) -> Option<f64> {
    // Admission compares a rounded dense + handoff sum. An upward ULP at
    // that cap covers the underlying real sum before evaluator rounding.
    let credit = mixlib_set_search::coverage::unrestricted_credit_upper(
        tracks.saturating_sub(1),
        MEASURED_CREDIT_CAP.next_up(),
    )?;
    let per_join = complete_set_score_upper(request, credit)?;
    let clock = mixlib_set_search::coverage::overlap_clock_score_upper(
        request.min(),
        request.max(),
        tracks.saturating_sub(1),
        OVERLAP_CREDIT_SLACK_SECONDS,
    );
    Some(clock.map_or(per_join, |upper| per_join.min(upper)))
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
    excluded_transitions: BTreeSet<(usize, usize)>,
    #[cfg(feature = "cp-search")]
    capacity_proof: Option<mixlib_set_search::coverage::DirectedCapacity>,
    pacing: Vec<TrackPacing>,
    cache: Cache,
    facts: CandidateEvidenceCache,
    event_cues: EventCueIndex,
    baseline_cues: BaselineCueIndex,
    worker_facts: Vec<CandidateEvidenceCache>,
    measured_keys: BTreeSet<String>,
    oracle: oracle::Ledger,
    continuations: Option<continuation::Frontier>,
    stats: Stats,
    started: Instant,
    deadline: Option<Instant>,
    progress: Report,
    history: Vec<Value>,
    incumbent: Option<MixPlan>,
    graph: Vec<Vec<Arc<MeasuredEdge>>>,
    known: BTreeSet<String>,
    negative_windows: BTreeSet<String>,
}

#[derive(Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum CandidateDecision {
    Rejected {
        reason: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        development: Option<crate::composition::DevelopmentShortfall>,
    },
    Verified {
        score: f64,
        improved: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WindowMeasurement {
    Original,
    SelectorRefresh,
}

impl WindowMeasurement {
    fn cache_kind(self) -> &'static str {
        match self {
            Self::Original => "windows",
            Self::SelectorRefresh => "selector-refreshes",
        }
    }
}

impl CandidateDecision {
    fn improved(&self) -> bool {
        matches!(self, Self::Verified { improved: true, .. })
    }
}

impl Search<'_> {
    fn permits_transition(&self, from: usize, to: usize) -> bool {
        from != to
            && !self.excluded_transitions.contains(&(from, to))
            && self.required.permits_edge(from, to)
    }

    fn permits_plan_transitions(&self, plan: &MixPlan) -> bool {
        plan.placements.windows(2).all(|pair| {
            let index = |p: &Placement| self.pool.iter().position(|t| t.track_id == p.track_id);
            index(&pair[0])
                .zip(index(&pair[1]))
                .is_some_and(|(from, to)| self.permits_transition(from, to))
        })
    }

    fn schedules_transition(&self, from: usize, to: usize) -> bool {
        #[cfg(feature = "cp-search")]
        if let Some(proof) = &self.capacity_proof {
            return proof.permits_pair(from, to);
        }
        self.permits_transition(from, to)
    }

    fn schedules_plan(&self, plan: &MixPlan) -> bool {
        #[cfg(feature = "cp-search")]
        if let Some(proof) = self.capacity_proof.as_ref().filter(|proof| {
            proof.excluded_pairs > 0 || proof.viable_tracks.count_ones() as usize != self.pool.len()
        }) {
            let index = |p: &Placement| self.pool.iter().position(|t| t.track_id == p.track_id);
            return match plan.placements.as_slice() {
                [single] => index(single).is_some_and(|i| proof.viable_tracks & (1_u128 << i) != 0),
                placements => placements.windows(2).all(|pair| {
                    index(&pair[0])
                        .zip(index(&pair[1]))
                        .is_some_and(|(from, to)| self.schedules_transition(from, to))
                }),
            };
        }
        let _ = plan;
        true
    }

    fn observe_window(&mut self, key: &str, measurement: &Measurement, kind: WindowMeasurement) {
        self.measured_keys.insert(key.to_owned());
        let reviewed_empty = match kind {
            WindowMeasurement::Original => self.oracle.observe(key, measurement),
            WindowMeasurement::SelectorRefresh => self.oracle.observe_refresh(key, measurement),
        };
        if reviewed_empty {
            self.negative_windows.insert(key.to_owned());
        } else {
            self.negative_windows.remove(key);
        }
    }

    fn cached_placement_track(
        &self,
        placement: &Placement,
        sources: &BTreeMap<String, String>,
    ) -> Option<usize> {
        self.pool.iter().position(|track| {
            track.track_id == placement.track_id
                && sources.get(&track.track_id).map_or(
                    // Old cache records predate the source map.
                    track.active_overrides.is_none() && track.override_revision == 0,
                    |key| self.cache.sources.get(&track.track_id) == Some(key),
                )
                && placement.audio_hash == track.audio_hash
                && placement.audio_revision == track.audio_revision
                && track.active_analysis.as_ref().is_some_and(|reference| {
                    reference.hash == placement.analysis.hash
                        && reference.path == placement.analysis.path
                })
                && placement.override_revision == track.override_revision
                && placement.time_map.source_sample_rate_hz == track.format.sample_rate_hz
                && placement.source_end_frame <= track.format.frames
        })
    }

    fn queue_continuations(
        &mut self,
        sources: &BTreeMap<String, String>,
        plans: Vec<MixPlan>,
        restored: bool,
        prioritized: bool,
    ) -> Result<()> {
        if self.continuations.is_none() {
            return Ok(());
        }
        for mut plan in plans {
            let pair = match plan.placements.as_slice() {
                [a, b] => self
                    .cached_placement_track(a, sources)
                    .zip(self.cached_placement_track(b, sources)),
                _ => None,
            };
            let Some((from, to)) = pair else {
                self.continuations.as_mut().unwrap().invalid();
                continue;
            };
            if !self.schedules_transition(from, to) {
                continue;
            }
            if crate::search_cache::rebind(&mut plan, self.policy).is_err() {
                self.continuations.as_mut().unwrap().invalid();
                continue;
            }
            let key = window_key(&self.cache, &plan)?;
            if !self.measured_keys.contains(&key) {
                self.continuations
                    .as_mut()
                    .unwrap()
                    .insert(key, plan, restored, prioritized);
            }
        }
        Ok(())
    }

    fn take_quality_seed(
        &mut self,
        ranked: &mut VecDeque<MixPlan>,
        scheduled: &BTreeSet<String>,
    ) -> Option<MixPlan> {
        match &mut self.continuations {
            Some(frontier) => frontier.take_quality_seed(ranked, &self.measured_keys, scheduled),
            None => ranked.pop_front(),
        }
    }

    fn register(&mut self, edge: &MeasuredEdge) -> Result<()> {
        if edge.last_resort {
            return Ok(());
        }
        let stored_credit = |key| {
            edge.plan
                .header
                .extensions
                .get(key)
                .and_then(Value::as_f64)
                .unwrap_or(0.)
        };
        let dense = edge.dense_quality_seconds;
        let handoff = edge.handoff_quality_seconds;
        if ![dense, handoff]
            .iter()
            .all(|credit| credit.is_finite() && *credit >= 0.)
            || dense + handoff > MEASURED_CREDIT_CAP
            || dense != stored_credit("dense_quality_seconds")
            || handoff != stored_credit("handoff_quality_seconds")
        {
            self.cache.invalid += 1;
            return Ok(());
        }
        let overlap_seconds = edge
            .plan
            .transitions
            .first()
            .and_then(|transition| {
                transition
                    .end_output_frame
                    .checked_sub(transition.start_output_frame)
            })
            .map(|frames| frames as f64 / f64::from(edge.plan.output.sample_rate_hz));
        require(
            overlap_seconds.is_some_and(|span| {
                span.is_finite() && dense + handoff <= span + OVERLAP_CREDIT_SLACK_SECONDS
            }),
            "LAZY_BOUND",
            "measured sustained credit exceeds its output overlap clock",
        )?;
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
        if !self.permits_transition(from, to) {
            return Ok(());
        }
        if !self.known.insert(measured_edge_key(edge)?) {
            self.stats.duplicate_measured_choices += 1;
            return Ok(());
        }
        self.graph[from * self.pool.len() + to].push(Arc::new(edge.clone()));
        self.stats.known_good_choices += 1;
        Ok(())
    }

    #[cfg(feature = "cp-search")]
    fn measured_credit_score_upper(&self) -> Option<f64> {
        use mixlib_set_search::coverage::{measured_credit_upper, JoinCredit};
        let tracks = self.pool.len();
        let credits = self.graph.iter().enumerate().flat_map(|(index, choices)| {
            choices.iter().map(move |edge| JoinCredit {
                from: index / tracks,
                to: index % tracks,
                dense: edge.dense_quality_seconds,
                handoff: edge.handoff_quality_seconds,
            })
        });
        complete_set_score_upper(self.request, measured_credit_upper(tracks, credits)?)
    }

    #[cfg(feature = "cp-search")]
    fn measured_assignment(&self) -> Option<MeasuredCoverage> {
        use mixlib_set_search::{coverage, objective::transition_shortfall};
        let n = self.pool.len();
        if !(2..=128).contains(&n) {
            return None;
        }
        let output_rate = self.policy.output_sample_rate_hz;
        let mut minimum_speed = vec![f64::INFINITY; n];
        let mut maximum_overlap = vec![0_u64; n];
        let mut joins = Vec::with_capacity(self.stats.known_good_choices);
        for (index, options) in self.graph.iter().enumerate() {
            let (from, to) = (index / n, index % n);
            if !self.permits_transition(from, to) {
                continue;
            }
            for edge in options {
                let [a, b] = edge.plan.placements.as_slice() else {
                    return None;
                };
                let [t] = edge.plan.transitions.as_slice() else {
                    return None;
                };
                let (depart, arrive) = (&a.time_map, &b.time_map);
                // These are the clocks used by apply_opening, splice_time_map
                // and apply_bookends. Other map layouts keep the weaker bound.
                let departure = depart.anchors.get(1)?;
                let arrival = arrive.anchors.iter().rev().nth(1)?;
                if depart.anchors.first()?.output_frame != 0
                    || departure.output_frame != t.start_output_frame
                    || arrive.anchors.first()?.output_frame != t.start_output_frame
                    || arrival.output_frame != t.end_output_frame
                    || edge.exit != departure.source_frame
                    || edge.entry_end == 0
                    || edge.entry_end != arrival.source_frame
                    || !edge.score.is_finite()
                {
                    return None;
                }
                for (track, map, speed) in [
                    (from, depart, *depart.slopes().first()?),
                    (to, arrive, *arrive.slopes().last()?),
                ] {
                    if map.source_sample_rate_hz != self.pool[track].format.sample_rate_hz
                        || map.output_sample_rate_hz != output_rate
                        || !(0.5..=2.).contains(&speed)
                    {
                        return None;
                    }
                    minimum_speed[track] = minimum_speed[track].min(speed);
                }
                let overlap = t.end_output_frame.checked_sub(t.start_output_frame)?;
                maximum_overlap[from] = maximum_overlap[from].max(overlap);
                joins.push(coverage::MeasuredJoin {
                    from,
                    to,
                    dense: edge.dense_quality_seconds,
                    handoff: edge.handoff_quality_seconds,
                    fit_shortfall: transition_shortfall(edge.score),
                });
            }
        }
        let capacities = self
            .pool
            .iter()
            .enumerate()
            .map(|(index, track)| {
                if minimum_speed[index].is_infinite() {
                    // A track without an incident join cannot be part of a
                    // nonempty measured chain.
                    return Some(0.);
                }
                let source = seconds(track, track.format.frames);
                if !(0. ..=86_400.).contains(&source) {
                    return None;
                }
                let original = (source.next_up() / minimum_speed[index]).next_up();
                // One outgoing overlap per track. Eight extra output frames
                // cover the three rounded bridge intervals or rounded bookends
                // and floating conversion on the bounded source span.
                let extra = maximum_overlap[index].checked_add(8)? as f64 / f64::from(output_rate);
                let base = (original + extra.next_up()).next_up();
                let replay = (base * MAX_REPLAY_FRACTION).next_up();
                Some((base + replay).next_up())
            })
            .collect::<Option<Vec<_>>>()?;
        // Only reachable predecessors and successors can contribute to a
        // measured-only route. Mixed routes still have unmeasured bridges.
        let mut adjacent = vec![0_u128; n];
        for join in &joins {
            adjacent[join.from] |= 1_u128 << join.to;
        }
        let proof = coverage::directed_capacity(
            self.request.min(),
            &capacities,
            self.required.required_runway().members,
            |from, to| adjacent[from] & (1_u128 << to) != 0,
        )?;
        let mut capacities = capacities;
        for (track, cap) in capacities.iter_mut().enumerate() {
            if proof.viable_tracks & (1_u128 << track) == 0 {
                *cap = 0.;
            }
        }
        let assignment = coverage::measured_assignment_upper(
            &capacities,
            self.request.min(),
            self.request.max(),
            self.incumbent.as_ref().map(crate::composition::score),
            joins
                .into_iter()
                .filter(|join| proof.permits_pair(join.from, join.to)),
        )?;
        Some(MeasuredCoverage { assignment, proof })
    }

    #[cfg(feature = "cp-search")]
    fn source_duration_capacity(&self, track: &Track, frames: u64) -> Option<f64> {
        let deviation = self.policy.max_speed_deviation;
        if !(0. ..=0.2).contains(&deviation) {
            return None;
        }
        // A validated map's derivative has a 1e-7 tolerance. Leave a wider
        // margin for cubic arithmetic and round conversions outward.
        let minimum_speed = (1. - deviation - 0.000001).next_down();
        let source = seconds(track, frames);
        if !(0. ..=86_400.).contains(&source) {
            return None;
        }
        let original = (source.next_up() / minimum_speed).next_up();
        let replay = (original * MAX_REPLAY_FRACTION).next_up();
        Some((original + replay).next_up())
    }

    #[cfg(feature = "cp-search")]
    fn track_duration_capacities(&self) -> Option<Vec<f64>> {
        self.pool
            .iter()
            .map(|track| self.source_duration_capacity(track, track.format.frames))
            .collect()
    }

    /// Pinned departing end, arriving start, and overlap on the output clock.
    #[cfg(feature = "cp-search")]
    fn window_span(&self, plan: &MixPlan, from: usize, to: usize) -> Option<(u64, u64, u64)> {
        let [outgoing, incoming] = plan.placements.as_slice() else {
            return None;
        };
        let [transition] = plan.transitions.as_slice() else {
            return None;
        };
        let left = &self.pool[from];
        let right = &self.pool[to];
        let rate = plan.output.sample_rate_hz;
        let out_end = outgoing.time_map.anchors.last()?;
        let in_start = incoming.time_map.anchors.first()?;
        if rate != self.policy.output_sample_rate_hz
            || !matches!(rate, 44100 | 48000)
            || plan.policy.max_speed_deviation > self.policy.max_speed_deviation
            || outgoing.track_id != left.track_id
            || incoming.track_id != right.track_id
            || transition.from_track_id != left.track_id
            || transition.to_track_id != right.track_id
            || outgoing.time_map.source_sample_rate_hz != left.format.sample_rate_hz
            || incoming.time_map.source_sample_rate_hz != right.format.sample_rate_hz
            || outgoing.time_map.output_sample_rate_hz != rate
            || incoming.time_map.output_sample_rate_hz != rate
            || out_end.output_frame != transition.end_output_frame
            || in_start.output_frame != transition.start_output_frame
            || out_end.source_frame != outgoing.source_end_frame
            || in_start.source_frame != incoming.source_start_frame
            || out_end.source_frame > left.format.frames
            || in_start.source_frame > right.format.frames
        {
            return None;
        }
        let overlap = transition
            .end_output_frame
            .checked_sub(transition.start_output_frame)?;
        (overlap > 0 && overlap <= MAX_FRAME).then_some((
            out_end.source_frame,
            in_start.source_frame,
            overlap,
        ))
    }

    #[cfg(feature = "cp-search")]
    fn retained_clock_upper(
        &self,
        retained: &[(usize, u64)],
        mut others: u128,
        overlap_frames: u64,
    ) -> Option<f64> {
        let mut upper = 0.0_f64;
        for &(track, frames) in retained {
            upper = (upper + self.source_duration_capacity(&self.pool[track], frames)?).next_up();
        }
        while others != 0 {
            let index = others.trailing_zeros() as usize;
            let track = &self.pool[index];
            upper = (upper + self.source_duration_capacity(track, track.format.frames)?).next_up();
            others &= others - 1;
        }
        if overlap_frames > MAX_FRAME {
            return None;
        }
        // source_duration_capacity already adds replay to each summand.
        // Remove the duplicated overlap and its duplicated replay allowance.
        let overlap =
            (overlap_frames as f64 / f64::from(self.policy.output_sample_rate_hz)).next_down();
        let duplicate = (overlap + (overlap * MAX_REPLAY_FRACTION).next_down()).next_down();
        Some((upper - duplicate).next_up().max(0.))
    }

    /// Allow bookends to reclaim the full outside source spans.
    #[cfg(feature = "cp-search")]
    fn window_capacity_upper(
        &self,
        plan: &MixPlan,
        proof: &mixlib_set_search::coverage::DirectedCapacity,
        from: usize,
        to: usize,
    ) -> Option<f64> {
        let (end, start, overlap) = self.window_span(plan, from, to)?;
        self.retained_clock_upper(
            &[(from, end), (to, self.pool[to].format.frames - start)],
            proof.other_tracks_at_pair(from, to),
            overlap,
        )
    }

    #[cfg(feature = "cp-search")]
    fn adjacent_windows_capacity_upper(
        &self,
        arrival: &MixPlan,
        departure: &MixPlan,
        proof: &mixlib_set_search::coverage::DirectedCapacity,
        [from, middle, to]: [usize; 3],
    ) -> Option<f64> {
        self.committed_windows_capacity_upper(
            [(from, middle, arrival), (middle, to, departure)],
            proof,
        )
    }

    /// Count only the source material retained by all joins in a contiguous
    /// subpath. Other songs must come before its head or after its tail.
    #[cfg(feature = "cp-search")]
    fn committed_windows_capacity_upper<'a>(
        &self,
        windows: impl IntoIterator<Item = (usize, usize, &'a MixPlan)>,
        proof: &mixlib_set_search::coverage::DirectedCapacity,
    ) -> Option<f64> {
        let mut windows = windows.into_iter();
        let (first, mut tail, mut previous) = windows.next()?;
        let (end, mut incoming_start, mut overlaps) = self.window_span(previous, first, tail)?;
        let mut used = (1_u128 << first) | (1_u128 << tail);
        let mut retained = Vec::with_capacity(windows.size_hint().0 + 2);
        retained.push((first, end));
        for (from, to, plan) in windows {
            let bit = 1_u128 << to;
            if from != tail || used & bit != 0 {
                return None;
            }
            let (end, next_start, overlap) = self.window_span(plan, from, to)?;
            // Splicing preserves these two overlap endpoints. Without the
            // exact anchors the combined source span is unconfirmed.
            if previous.placements[1]
                .time_map
                .anchors
                .iter()
                .rev()
                .nth(1)?
                .output_frame
                != previous.transitions[0].end_output_frame
                || plan.placements[0].time_map.anchors.get(1)?.output_frame
                    != plan.transitions[0].start_output_frame
            {
                return None;
            }
            retained.push((from, end.checked_sub(incoming_start)?));
            incoming_start = next_start;
            overlaps = overlaps.checked_add(overlap)?;
            used |= bit;
            tail = to;
            previous = plan;
        }
        retained.push((tail, self.pool[tail].format.frames - incoming_start));
        self.retained_clock_upper(&retained, proof.outside_tracks(first, tail, used), overlaps)
    }

    #[cfg(feature = "cp-search")]
    fn window_cannot_fit(
        &self,
        plan: &MixPlan,
        proof: &mixlib_set_search::coverage::DirectedCapacity,
        from: usize,
        to: usize,
    ) -> bool {
        self.window_capacity_upper(plan, proof, from, to)
            .is_some_and(|upper| upper < self.request.min())
    }

    #[cfg(feature = "cp-search")]
    fn measured_window_capacity_counts(
        &self,
        proof: &mixlib_set_search::coverage::DirectedCapacity,
    ) -> Value {
        let n = self.pool.len();
        let mut checked = 0;
        let mut excluded = 0;
        let mut unresolved = 0;
        for (pair, edges) in self.graph.iter().enumerate() {
            let (from, to) = (pair / n, pair % n);
            if !proof.permits_pair(from, to) {
                continue;
            }
            for edge in edges {
                if let Some(upper) = self.window_capacity_upper(&edge.plan, proof, from, to) {
                    checked += 1;
                    excluded += usize::from(upper < self.request.min());
                } else {
                    unresolved += 1;
                }
            }
        }
        json!({"checked": checked, "excluded": excluded, "unresolved_geometry": unresolved})
    }

    #[cfg(feature = "cp-search")]
    fn directed_capacity(&self) -> Option<mixlib_set_search::coverage::DirectedCapacity> {
        let capacities = self.track_duration_capacities()?;
        let n = capacities.len();
        let propagate = |permits: &dyn Fn(usize, usize) -> bool| {
            mixlib_set_search::coverage::directed_capacity(
                self.request.min(),
                &capacities,
                self.required.required_runway().members,
                permits,
            )
        };
        let mut proof = propagate(&|from, to| self.permits_transition(from, to))?;
        let original_pairs = proof.retained_pairs() + proof.excluded_pairs;
        while proof.viable_tracks != 0 {
            let Ok(forced) = self
                .required
                .forced_chain_edges(|from, to| proof.permits_pair(from, to))
            else {
                // Keep the graph that witnessed the blocked orientations for
                // the separate required-adjacency infeasibility report.
                break;
            };
            if forced.is_empty() {
                break;
            }
            let mut successor = vec![None; n];
            let mut predecessor = vec![None; n];
            for (from, to) in forced {
                successor[from] = Some(to);
                predecessor[to] = Some(from);
            }
            let matches_chain = |from: usize, to: usize| {
                successor[from].is_none_or(|next| next == to)
                    && predecessor[to].is_none_or(|previous| previous == from)
            };
            if !(0..n).any(|from| {
                (0..n).any(|to| proof.permits_pair(from, to) && !matches_chain(from, to))
            }) {
                break;
            }
            let tightened =
                propagate(&|from, to| proof.permits_pair(from, to) && matches_chain(from, to))?;
            let iterations = proof.iterations + tightened.iterations;
            proof = tightened;
            proof.iterations = iterations;
            proof.excluded_pairs = original_pairs - proof.retained_pairs();
        }
        Some(proof)
    }

    #[cfg(feature = "cp-search")]
    fn duration_domain_empty(&self) -> bool {
        self.capacity_proof
            .as_ref()
            .is_some_and(|proof| proof.viable_tracks == 0)
    }

    #[cfg(feature = "cp-search")]
    fn blocked_required_chain(&self) -> Option<Vec<usize>> {
        self.required
            .blocked_chain(|from, to| self.schedules_transition(from, to))
    }

    #[cfg(feature = "cp-search")]
    fn structural_infeasibility(&self) -> Option<StructuralInfeasibility> {
        if self.duration_domain_empty() {
            Some(StructuralInfeasibility::DirectedCapacity)
        } else {
            self.blocked_required_chain()
                .map(|_| StructuralInfeasibility::RequiredChain)
        }
    }

    #[cfg(feature = "cp-search")]
    fn required_chain_infeasibility_evidence(&self, chain: &[usize]) -> Value {
        let blocked = |reverse| {
            chain.windows(2).filter_map(|pair| {
                let (from, to) = if reverse { (pair[1], pair[0]) } else { (pair[0], pair[1]) };
                if self.schedules_transition(from, to) {
                    return None;
                }
                Some(json!({
                    "from": self.pool[from].track_id,
                    "to": self.pool[to].track_id,
                    "reason": if self.permits_transition(from, to) { "capacity_or_required_orientation" }
                        else { "pinned_transition_constraints" },
                }))
            }).collect::<Vec<_>>()
        };
        json!({
            "kind": "required_adjacency_chain",
            "scope": "All unique-song routes in the pinned pool, including unmeasured joins",
            "mode": self.required.mode,
            "chain_track_ids": chain.iter().map(|&i| &self.pool[i].track_id).collect::<Vec<_>>(),
            "forward_blockers": blocked(false),
            "reverse_blockers": (self.required.mode == required_pairs::Mode::Either).then(|| blocked(true)),
            "track_capacity_seconds_upper_in_manifest_order": self.track_duration_capacities(),
        })
    }

    #[cfg(feature = "cp-search")]
    fn duration_infeasibility_evidence(&self) -> Option<Value> {
        let proof = self
            .capacity_proof
            .as_ref()
            .filter(|proof| proof.viable_tracks == 0)?;
        Some(json!({
            "kind": "directed_source_capacity",
            "scope": "All unique-song routes in the pinned pool, including unmeasured joins and maximum replay",
            "minimum_seconds": self.request.min(),
            "maximum_replay_fraction": MAX_REPLAY_FRACTION,
            "track_capacity_seconds_upper_in_manifest_order": self.track_duration_capacities()?,
            "required_track_ids": self.pool.iter().enumerate()
                .filter(|(i, _)| self.required.required_runway().members & (1_u128 << i) != 0)
                .map(|(_, track)| &track.track_id).collect::<Vec<_>>(),
            "propagation_iterations": proof.iterations,
            "retained_tracks": 0,
            "retained_pairs": proof.retained_pairs(),
        }))
    }

    #[cfg(feature = "cp-search")]
    fn paced_clock_upper(
        &self,
        directed_capacity: Option<&mixlib_set_search::coverage::DirectedCapacity>,
    ) -> Option<f64> {
        let rate = f64::from(self.policy.output_sample_rate_hz);
        let speed = 1. + self.policy.max_speed_deviation;
        if !matches!(self.policy.output_sample_rate_hz, 44100 | 48000)
            || !(1. ..=2.).contains(&speed)
            || self.pacing.len() != self.pool.len()
        {
            return None;
        }
        // The bridge uses the two pair endpoint speeds and their mean.
        // Pad the policy limit beyond the map validator's 1e-7 tolerance.
        let speed = (speed + 0.001).next_up();
        let runway = self
            .pool
            .iter()
            .zip(&self.pacing)
            .map(|(track, pacing)| {
                let source_rate = track.format.sample_rate_hz;
                if !(0. ..=86_400.).contains(&seconds(track, track.format.frames)) {
                    return None;
                }
                let source = pacing.minimum_internal_source_frames(source_rate)?;
                // The exact solo clock has three rounded intervals. Reserve
                // four frames for their rounding and floating conversions.
                let output = (source as f64 * rate / f64::from(source_rate) / speed).floor() as u64;
                Some((output.saturating_sub(4) as f64 / rate).next_down().max(0.))
            })
            .collect::<Option<Vec<_>>>()?;
        use mixlib_set_search::coverage::TrackClockBounds;
        let capacities = self.track_duration_capacities();
        let preferred = self
            .pool
            .iter()
            .map(|track| {
                crate::composition::preferred_development_floor(seconds(track, track.format.frames))
            })
            .collect::<Vec<_>>();
        mixlib_set_search::coverage::component_paced_overlap_score_upper(
            self.request.min(),
            self.request.max(),
            MEASURED_CREDIT_CAP.next_up(),
            OVERLAP_CREDIT_SLACK_SECONDS,
            TrackClockBounds {
                solo_seconds_lower: &runway,
                duration_seconds_upper: capacities.as_deref(),
                preferred_development_lower: Some(&preferred),
            },
            self.required.required_runway(),
            |from, to| {
                directed_capacity.map_or_else(
                    || self.permits_transition(from, to),
                    |capacity| capacity.permits_pair(from, to),
                )
            },
        )
    }

    #[cfg(feature = "cp-search")]
    fn measured_scope(
        &self,
        assignment: Option<mixlib_set_search::coverage::MeasuredAssignment>,
    ) -> mixlib_set_search::coverage::RegionCeiling {
        use mixlib_set_search::coverage::RegionCeiling;
        let credit = self.measured_credit_score_upper();
        match assignment.map(|bound| bound.ceiling) {
            Some(RegionCeiling::Empty) => RegionCeiling::Empty,
            Some(RegionCeiling::Upper(upper)) => {
                RegionCeiling::Upper(credit.map_or(upper, |other| upper.min(other)))
            }
            _ => credit.map_or(RegionCeiling::Unknown, RegionCeiling::Upper),
        }
    }

    #[cfg(feature = "cp-search")]
    fn measured_prune(&self) -> Option<MeasuredPrune> {
        use mixlib_set_search::coverage::RegionCeiling;
        let assignment = self
            .measured_assignment()
            .map(|coverage| coverage.assignment);
        if assignment.is_some_and(|bound| bound.ceiling == RegionCeiling::Empty) {
            return Some(MeasuredPrune::DurationCapacity);
        }
        let incumbent = self.incumbent.as_ref().map(crate::composition::score)?;
        if !incumbent.is_finite() {
            return None;
        }
        if assignment
            .and_then(|bound| bound.surplus_upper)
            .is_some_and(|upper| upper <= 0.)
        {
            return Some(MeasuredPrune::ScoreBound);
        }
        matches!(self.measured_scope(assignment), RegionCeiling::Upper(upper) if upper <= incumbent)
            .then_some(MeasuredPrune::ScoreBound)
    }

    #[cfg(feature = "cp-search")]
    fn score_coverage(&self) -> Value {
        use mixlib_set_search::coverage::{covered_upper, RegionCeiling};
        let measured_coverage = self.measured_assignment();
        let assignment = measured_coverage
            .as_ref()
            .map(|coverage| coverage.assignment);
        let mut measured = self.measured_scope(assignment);
        let tracks = self.pool.len();
        let fallback = unrestricted_score_upper(tracks, self.request);
        // Pinned generators put the paced source endpoints on the exact
        // overlap anchors. Use the same check on restored measured pairs;
        // a foreign geometry leaves only the universal clock bound.
        let computed = self
            .capacity_proof
            .is_none()
            .then(|| self.directed_capacity())
            .flatten();
        let directed_capacity = self.capacity_proof.as_ref().or(computed.as_ref());
        let paced_clock = assignment.and_then(|_| self.paced_clock_upper(directed_capacity));
        let mut unmeasured = match (fallback, paced_clock) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let duration_domain_empty = directed_capacity.is_some_and(|proof| proof.viable_tracks == 0);
        let blocked_chain = self.blocked_required_chain();
        let domain_empty = duration_domain_empty || blocked_chain.is_some();
        let unmeasured_scope = if domain_empty {
            measured = RegionCeiling::Empty;
            unmeasured = None;
            RegionCeiling::Empty
        } else {
            unmeasured.map_or(RegionCeiling::Unknown, RegionCeiling::Upper)
        };
        let incumbent = self.incumbent.as_ref().map(crate::composition::score);
        let upper = covered_upper(incumbent, &[measured, unmeasured_scope]);
        let gap = upper.zip(incumbent).map(|(upper, incumbent)| {
            if upper == incumbent {
                0.
            } else {
                (upper - incumbent).next_up()
            }
        });
        json!({
            "scope": "Unique-song duration sets under the current measured-credit admission and score contract",
            "measured_routes_upper": match measured { RegionCeiling::Upper(score) => Some(score), _ => None },
            "measured_routes_empty": measured == RegionCeiling::Empty,
            "measured_assignment": assignment.map(|bound| json!({
                "minimum_joins": bound.minimum_joins,
                "clock_buckets": bound.clock_buckets,
                "incumbent_surplus_upper": bound.surplus_upper,
                "scope": "Nonempty measured chains, including all compatible endings and replays",
            })),
            "measured_directed_capacity": measured_coverage.as_ref().map(|coverage| json!({
                "retained_tracks": coverage.proof.viable_tracks.count_ones(),
                "retained_pairs": coverage.proof.retained_pairs(),
                "excluded_pairs": coverage.proof.excluded_pairs,
                "iterations": coverage.proof.iterations,
            })),
            "routes_with_unmeasured_joins_upper": unmeasured,
            "routes_with_unmeasured_joins_empty": domain_empty,
            "unmeasured_bound_basis": if duration_domain_empty {
                "No permitted directed route has sufficient optimistic source capacity with all required songs"
            } else if blocked_chain.is_some() {
                "A required adjacency chain has no permitted orientation"
            } else if paced_clock.is_some() {
                "Per-join admission cap, disjoint overlaps, phrase-pacing runway and minimum development shortage within each permitted transition component"
            } else {
                "Per-join admission cap and disjoint output overlap clock"
            },
            "unmeasured_paced_clock_upper": paced_clock,
            "minimum_capacity_tracks": directed_capacity.and_then(|capacity| capacity.minimum_tracks),
            "directed_capacity": directed_capacity.map(|capacity| json!({
                "retained_tracks": capacity.viable_tracks.count_ones(),
                "excluded_tracks": tracks - capacity.viable_tracks.count_ones() as usize,
                "iterations": capacity.iterations,
                "retained_pairs": capacity.retained_pairs(),
                "excluded_pairs": capacity.excluded_pairs,
            })),
            "measured_window_capacity": directed_capacity
                .map(|proof| self.measured_window_capacity_counts(proof)),
            "required_runway_members": self.required.required_runway().members.count_ones(),
            "required_internal_runways": self.required.required_runway().internal.count_ones(),
            "verified_incumbent": incumbent,
            "global_upper": upper,
            "absolute_gap_upper": gap,
        })
    }
    fn load_known(&mut self, cancel: &Cancellation) -> Result<()> {
        self.load_window_records(WindowMeasurement::Original, cancel)?;
        if self.continuations.is_some() {
            self.load_window_records(WindowMeasurement::SelectorRefresh, cancel)?;
        }
        if self.budget_open() {
            self.restore_endpoint_expansions(cancel)?;
        }
        if let Some(frontier) = &mut self.continuations {
            frontier.finish_restore(&self.measured_keys);
        }
        eprintln!(
            "set search: loaded {} known good cue/tactic choices from the persistent cache",
            self.stats.known_good_choices
        );
        Ok(())
    }

    fn load_window_records(
        &mut self,
        kind: WindowMeasurement,
        cancel: &Cancellation,
    ) -> Result<()> {
        if !self.budget_open() {
            return Ok(());
        }
        for key in self.cache.keys(self.store, kind.cache_kind())? {
            cancel.check()?;
            if !self.budget_open() {
                break;
            }
            let Some(measured) =
                self.cache
                    .read::<Measurement>(self.store, kind.cache_kind(), &key)?
            else {
                continue;
            };
            if (kind == WindowMeasurement::SelectorRefresh && measured.sources.len() != 2)
                || measured
                    .sources
                    .iter()
                    .any(|(id, pin)| self.cache.sources.get(id) != Some(pin))
            {
                continue;
            }
            self.observe_window(&key, &measured, kind);
            self.queue_continuations(&measured.sources, measured.continuations, true, false)?;
            for mut edge in measured.edges.into_iter().filter(|e| !e.last_resort) {
                let current = edge
                    .plan
                    .placements
                    .iter()
                    .all(|p| self.cached_placement_track(p, &measured.sources).is_some());
                if current {
                    crate::search_cache::rebind(&mut edge.plan, self.policy)?;
                    self.register(&edge)?;
                }
            }
        }
        Ok(())
    }

    fn budget_open(&self) -> bool {
        self.deadline
            .is_none_or(|deadline| Instant::now() < deadline)
    }
    fn evidence(&self, status: &str) -> Value {
        json!({"method":"complete-set-refinement/1","status":status,
            "phase":if self.incumbent.is_some(){"refinement"}else{"baseline"},"stats":self.stats,
            "prepared_tracks":self.pool.len(),"elapsed_seconds":self.started.elapsed().as_secs_f64(),
            "first_valid_seconds":self.history.first().and_then(|h|h.get("elapsed_seconds")),
            "best_plan_id":self.incumbent.as_ref().map(|p|&p.plan_id),"improvements":self.history,
            "cache":{"mode":self.cache.mode,"hits":self.cache.hits,"misses":self.cache.misses,"invalid":self.cache.invalid},
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
    fn save(&mut self, plan: MixPlan, origin: &str, cancel: &Cancellation) -> Result<bool> {
        Ok(self.save_candidate(plan, origin, false, cancel)?.improved())
    }

    fn save_candidate(
        &mut self,
        mut plan: MixPlan,
        origin: &str,
        replay_base: bool,
        cancel: &Cancellation,
    ) -> Result<CandidateDecision> {
        if !self.permits_plan_transitions(&plan) {
            return Ok(CandidateDecision::Rejected {
                reason: "excluded_transition",
                development: None,
            });
        }
        self.required.annotate(&mut plan)?;
        let evidence = match self.composer.evaluate_for_save(&plan, replay_base) {
            Ok(evidence) => evidence,
            Err(rejection) => {
                return Ok(CandidateDecision::Rejected {
                    reason: rejection.reason,
                    development: rejection.development,
                })
            }
        };
        plan.header
            .extensions
            .insert("set_composition".into(), evidence);
        let score = crate::composition::score(&plan);
        if !score.is_finite() {
            return Ok(CandidateDecision::Rejected {
                reason: "invalid_evidence",
                development: None,
            });
        }
        if self
            .incumbent
            .as_ref()
            .is_some_and(|p| !crate::composition::better(&plan, p))
        {
            return Ok(CandidateDecision::Verified {
                score,
                improved: false,
            });
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
        Ok(CandidateDecision::Verified {
            score,
            improved: true,
        })
    }
    fn measure(&mut self, seed: &MixPlan, cancel: &Cancellation) -> Result<Measurement> {
        let key = window_key(&self.cache, seed)?;
        if self.negative_windows.contains(&key) {
            // The selector ran without recording an admitted choice or a child.
            // This memoizes that review and leaves other realizations open.
            self.cache.hits += 1;
            self.stats.cached_windows += 1;
            return Ok(Measurement::default());
        }
        let mut kind = WindowMeasurement::Original;
        let mut cached = self
            .cache
            .read::<Measurement>(self.store, kind.cache_kind(), &key)?;
        if cached.is_none() && self.continuations.is_some() && self.measured_keys.contains(&key) {
            kind = WindowMeasurement::SelectorRefresh;
            cached = self
                .cache
                .read::<Measurement>(self.store, kind.cache_kind(), &key)?;
        }
        if let Some(mut measured) = cached {
            for e in &mut measured.edges {
                crate::search_cache::rebind(&mut e.plan, self.policy)?;
            }
            for p in &mut measured.continuations {
                crate::search_cache::rebind(p, self.policy)?;
            }
            self.stats.cached_windows += 1;
            self.observe_window(&key, &measured, kind);
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
        self.commit_measurement(&key, &measured, WindowMeasurement::Original)?;
        Ok(measured)
    }

    fn commit_measurement(
        &mut self,
        key: &str,
        measurement: &Measurement,
        kind: WindowMeasurement,
    ) -> Result<()> {
        self.stats.measured_windows += 1;
        self.stats.admitted_choices += measurement.edges.len();
        self.cache
            .write(self.store, kind.cache_kind(), key, measurement)?;
        self.observe_window(key, measurement, kind);
        for edge in &measurement.edges {
            self.register(edge)?;
        }
        Ok(())
    }

    fn defer_window_failure(&mut self, key: String, error: Error) -> Result<()> {
        if self.continuations.is_none() || matches!(error.code.as_str(), "CANCELLED" | "LAZY_BOUND")
        {
            return Err(error);
        }
        self.stats.measured_windows += 1;
        self.oracle.fail(key, error);
        Ok(())
    }
    /// Independent PCM windows share four persistent worker caches. Registration
    /// and durable writes remain serial after the scoped workers finish.
    fn measure_batch(
        &mut self,
        seeds: &[MixPlan],
        cancel: &Cancellation,
    ) -> Result<Vec<Measurement>> {
        let mut result = vec![Measurement::default(); seeds.len()];
        let mut fresh = vec![];
        let mut seen = BTreeSet::new();
        let mut selector_refresh_scheduled = false;
        for (i, seed) in seeds.iter().enumerate() {
            if !self.budget_open() {
                break;
            }
            if !self.permits_plan_transitions(seed) {
                continue;
            }
            if !self.schedules_plan(seed) {
                self.stats.capacity_skipped_windows += 1;
                continue;
            }
            let key = window_key(&self.cache, seed)?;
            if !seen.insert(key.clone()) {
                continue;
            }
            if !selector_refresh_scheduled
                && self.continuations.is_some()
                && self.oracle.needs_selector_refresh(&key)
            {
                fresh.push((i, key, seed, WindowMeasurement::SelectorRefresh));
                selector_refresh_scheduled = true;
            } else if self.measured_keys.contains(&key) || self.negative_windows.contains(&key) {
                match self.measure(seed, cancel) {
                    Ok(measurement) => result[i] = measurement,
                    Err(error) => self.defer_window_failure(key, error)?,
                }
            } else {
                fresh.push((i, key, seed, WindowMeasurement::Original));
            }
        }
        if fresh.is_empty() || !self.budget_open() {
            return Ok(result);
        }
        if self.worker_facts.is_empty() {
            self.worker_facts = (0..4).map(|_| CandidateEvidenceCache::new()).collect();
        }
        let n = self.worker_facts.len();
        let snapshot = &self.snapshot;
        let policy = self.policy;
        let cache = &self.cache;
        let context = self.context;
        let deadline = self.deadline;
        let started = Instant::now();
        let (measured, worker_failed) = std::thread::scope(|scope| {
            let tasks = self
                .worker_facts
                .iter_mut()
                .enumerate()
                .map(|(worker, facts)| {
                    let fresh = &fresh;
                    scope.spawn(move || -> Vec<_> {
                        fresh
                            .iter()
                            .skip(worker)
                            .step_by(n)
                            .take_while(|_| deadline.is_none_or(|at| Instant::now() < at))
                            .map(|(i, key, seed, kind)| {
                                (
                                    *i,
                                    key.clone(),
                                    *kind,
                                    cancel.check().and_then(|()| {
                                        measure_candidate(
                                            snapshot, policy, cache, facts, seed, context, cancel,
                                        )
                                    }),
                                )
                            })
                            .collect()
                    })
                })
                .collect::<Vec<_>>();
            let mut all = vec![];
            let mut worker_failed = false;
            for task in tasks {
                match task.join() {
                    Ok(windows) => all.extend(windows),
                    Err(_) => worker_failed = true,
                }
            }
            (all, worker_failed)
        });
        if let Some(deadline) = deadline {
            self.stats.in_flight_pcm_overrun_seconds += Instant::now()
                .saturating_duration_since(deadline.max(started))
                .as_secs_f64();
        }
        self.commit_batch_results(measured, &mut result)?;
        require(!worker_failed, "TACTIC_WORKER", "measurement worker failed")?;
        Ok(result)
    }

    fn commit_batch_results(
        &mut self,
        measured: Vec<(usize, String, WindowMeasurement, Result<Measurement>)>,
        result: &mut [Measurement],
    ) -> Result<()> {
        let mut terminal = None;
        for (i, key, kind, outcome) in measured {
            self.stats.selector_refresh_attempts +=
                usize::from(kind == WindowMeasurement::SelectorRefresh);
            match outcome {
                Ok(measurement) => {
                    self.commit_measurement(&key, &measurement, kind)?;
                    result[i] = measurement;
                }
                Err(error) => {
                    if let Err(error) = self.defer_window_failure(key, error) {
                        terminal.get_or_insert(error);
                    }
                }
            }
        }
        if let Some(error) = terminal {
            return Err(error);
        }
        Ok(())
    }
}

fn route_key(plan: &MixPlan) -> Result<String> {
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
    value.as_object_mut().unwrap().remove("policy");
    value.as_object_mut().unwrap().remove("policy_hash");
    remove_ids(&mut value);
    normalize(&mut value);
    Ok(canonical_hash(&value)?.digest)
}

/// Both phrase-pacing and score calculations read facts outside the plan.
/// They must participate in identity before a repeat can be discarded.
fn measured_edge_key(edge: &MeasuredEdge) -> Result<String> {
    Ok(canonical_hash(&json!({
        "plan": route_key(&edge.plan)?,
        "exit": edge.exit,
        "entry_end": edge.entry_end,
        "score": edge.score.to_bits(),
        "last_resort": edge.last_resort,
        "dense_quality_seconds": edge.dense_quality_seconds.to_bits(),
        "handoff_quality_seconds": edge.handoff_quality_seconds.to_bits(),
    }))?
    .digest)
}

/// Prepare immutable source, composition and measurement context.
fn new_search<'a>(
    store: &'a Store,
    policy: &'a MixingConfig,
    context: usize,
    pool: Option<Vec<Track>>,
    request: Request,
    started: Instant,
    cancel: &Cancellation,
) -> Result<Search<'a>> {
    let snapshot = store.read_snapshot();
    let pool = match pool {
        Some(pool) => pool,
        None => measured_beam::prepared_pool(&snapshot)?,
    };
    let pool_seconds = started.elapsed().as_secs_f64();
    require(
        (3..=128).contains(&pool.len()),
        "CHAIN_POOL",
        "duration search needs 3–128 distinct prepared songs",
    )?;
    let cp_enabled = cfg!(feature = "cp-search")
        && policy
            .header
            .extensions
            .get(crate::set_config::ENGINE_KEY)
            .and_then(Value::as_str)
            == Some("cp");
    if !cp_enabled {
        let capacity = pool.iter().map(|t| seconds(t, t.format.frames)).sum();
        require(
            request.capacity_possible(capacity, 1. - policy.max_speed_deviation),
            "SET_POOL_DURATION",
            "prepared source capacity is shorter than the requested set",
        )?;
    }
    let required = required_pairs::RequiredPairs::resolve(&snapshot, policy, &pool, pool.len())?;
    let excluded_transitions = read_transition_exclusions(&snapshot, &pool, cancel)?;
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
    let graph = vec![vec![]; pool.len() * pool.len()];
    let mut search = Search {
        store,
        snapshot,
        policy,
        context,
        request,
        composer,
        required,
        excluded_transitions,
        #[cfg(feature = "cp-search")]
        capacity_proof: None,
        pacing,
        cache,
        facts: CandidateEvidenceCache::new(),
        event_cues: EventCueIndex::new(context),
        baseline_cues: BaselineCueIndex::new(context),
        worker_facts: vec![],
        measured_keys: BTreeSet::new(),
        oracle: oracle::Ledger::default(),
        continuations: (cfg!(feature = "cp-search")
            && policy
                .header
                .extensions
                .get(crate::set_config::ENGINE_KEY)
                .and_then(Value::as_str)
                == Some("cp"))
        .then(continuation::Frontier::default),
        stats: Stats {
            prepared_tracks: pool.len(),
            pool_seconds,
            ..Stats::default()
        },
        started,
        deadline: None,
        progress: Report::new("duration_set_search"),
        history: vec![],
        incumbent: None,
        graph,
        known: BTreeSet::new(),
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

fn read_transition_exclusions(
    store: &Store,
    pool: &[Track],
    cancel: &Cancellation,
) -> Result<BTreeSet<(usize, usize)>> {
    let indices: BTreeMap<_, _> = pool
        .iter()
        .enumerate()
        .map(|(i, t)| (t.track_id.as_str(), i))
        .collect();
    let mut excluded = BTreeSet::new();
    for (from, track) in pool.iter().enumerate() {
        cancel.check()?;
        let overrides = store.overrides(track)?;
        require(
            overrides.operations.is_empty() || overrides.base_audio_hash == track.audio_hash,
            "OVERRIDE_UNRESOLVED",
            "audio changed; review anchored corrections",
        )?;
        for operation in overrides.operations {
            if let Correction::ExcludeTransition { to_track_id } = operation {
                if let Some(&to) = indices.get(to_track_id.as_str()) {
                    excluded.insert((from, to));
                }
            }
        }
    }
    Ok(excluded)
}

#[path = "complete_refinement.rs"]
mod complete_refinement;

pub(super) fn refine_set(
    store: &Store,
    policy: &MixingConfig,
    context: usize,
    pool: Option<Vec<Track>>,
    request: Request,
    started: Instant,
    cancel: &Cancellation,
) -> Result<Value> {
    require(
        cfg!(feature = "cp-search")
            || policy
                .header
                .extensions
                .get(crate::set_config::ENGINE_KEY)
                .and_then(Value::as_str)
                != Some("cp"),
        "CP_BACKEND_UNAVAILABLE",
        "the cp engine requires a mixlib build with the cp-search feature",
    )?;
    complete_refinement::run(store, policy, context, pool, request, started, cancel)
}

pub(super) fn choose(
    store: &Store,
    policy: &MixingConfig,
    context: usize,
    pool: Vec<Track>,
    request: Request,
    cancel: &Cancellation,
) -> Result<MixPlan> {
    let result = refine_set(
        store,
        policy,
        context,
        Some(pool),
        request,
        Instant::now(),
        cancel,
    )?;
    cancel.check()?;
    result
        .get("plan")
        .filter(|p| !p.is_null())
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .ok_or_else(|| {
            let (code, message) = match result["status"].as_str() {
                Some("infeasible") => (
                    "SET_DURATION_INFEASIBLE",
                    "The pinned source and transition constraints prove the recipe infeasible.",
                ),
                Some("draft") => (
                    "SET_REFINEMENT_PENDING",
                    "Full-duration draft saved; approved joins remain unresolved.",
                ),
                _ => (
                    "SET_REFINEMENT_PENDING",
                    "Search stopped before finding a full-duration draft.",
                ),
            };
            Error::new(code, format!("{message} {}", result["draft_path"]))
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
