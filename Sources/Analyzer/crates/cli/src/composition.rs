//! Set-level policy over admitted transitions. This layer never relaxes tactic
//! safety or edits audio to hit a clock: it chooses different source passages.
use super::*;
use crate::{measured_beam::route_quality, tactics::MeasuredEdge};
use mixlib_planner::TrackPacing;
use mixlib_set_search::objective::{self, CompleteScore, PREFERRED_TRANSITION_FIT};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    time::Instant,
};

#[derive(Clone, Copy, Debug)]
pub(super) struct Request {
    pub minutes: u64,
    pub shape: Shape,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Shape {
    Story,
    Wave,
    Ramp,
    Chapters,
}
impl Shape {
    fn name(self) -> &'static str {
        match self {
            Self::Story => "story",
            Self::Wave => "wave",
            Self::Ramp => "ramp",
            Self::Chapters => "chapters",
        }
    }
    fn knots(self) -> &'static [(f64, f64)] {
        match self {
            Self::Story => &[
                (0., 0.3),
                (0.18, 0.5),
                (0.32, 0.7),
                (0.43, 0.4),
                (0.62, 0.72),
                (0.82, 0.92),
                (0.91, 0.7),
                (1., 0.4),
            ],
            Self::Wave => &[
                (0., 0.4),
                (0.22, 0.8),
                (0.35, 0.42),
                (0.55, 0.85),
                (0.68, 0.45),
                (0.87, 0.9),
                (1., 0.5),
            ],
            Self::Ramp => &[(0., 0.3), (0.3, 0.45), (0.6, 0.65), (1., 0.9)],
            Self::Chapters => &[
                (0., 0.3),
                (0.18, 0.75),
                (0.28, 0.4),
                (0.47, 0.85),
                (0.57, 0.42),
                (0.8, 0.95),
                (0.9, 0.7),
                (1., 0.4),
            ],
        }
    }
    fn target(self, progress: f64) -> f64 {
        let p = progress.clamp(0., 1.);
        let knots = self.knots();
        let w = knots.windows(2).find(|w| p <= w[1].0).unwrap();
        Self::target_on_segment(w, p)
    }

    fn target_on_segment(segment: &[(f64, f64)], progress: f64) -> f64 {
        let (x0, y0) = segment[0];
        let (x1, y1) = segment[1];
        y0 + (y1 - y0) * (progress - x0) / (x1 - x0)
    }

    #[cfg(feature = "cp-search")]
    fn target_range(self, start: f64, end: f64) -> [f64; 2] {
        let start = start.clamp(0., 1.);
        let end = end.clamp(0., 1.);
        let mut lower = f64::INFINITY;
        let mut upper = f64::NEG_INFINITY;
        for segment in self.knots().windows(2) {
            let a = start.max(segment[0].0);
            let b = end.min(segment[1].0);
            if a <= b {
                // Within a segment the floating interpolation is monotone.
                // At a knot consider both adjacent formulas.
                let a = Self::target_on_segment(segment, a);
                let b = Self::target_on_segment(segment, b);
                lower = lower.min(a.min(b));
                upper = upper.max(a.max(b));
            }
        }
        [lower, upper]
    }
}
impl Request {
    pub fn new(minutes: u64, shape: &str) -> Result<Self> {
        require(
            matches!(minutes, 60 | 120),
            "SET_DURATION",
            "sets must target 57–60 or 117–120 minutes",
        )?;
        let shape = match shape {
            "auto" if minutes == 120 => Shape::Chapters,
            "auto" | "story" => Shape::Story,
            "wave" => Shape::Wave,
            "ramp" => Shape::Ramp,
            "chapters" => Shape::Chapters,
            _ => return Err(Error::new("SET_STRUCTURE", "unknown set structure")),
        };
        Ok(Self { minutes, shape })
    }
    pub fn from_policy(policy: &MixingConfig) -> Result<Option<Self>> {
        policy
            .header
            .extensions
            .get("set_minutes")
            .map(|m| {
                let minutes = m
                    .as_u64()
                    .ok_or_else(|| Error::new("SET_DURATION", "set_minutes must be 60 or 120"))?;
                Self::new(
                    minutes,
                    policy
                        .header
                        .extensions
                        .get("set_structure")
                        .and_then(Value::as_str)
                        .unwrap_or("auto"),
                )
            })
            .transpose()
    }
    pub fn capacity_possible(self, source_seconds: f64, minimum_speed: f64) -> bool {
        source_seconds / minimum_speed.max(0.01) * (1. + MAX_REPLAY_FRACTION) >= self.min()
    }
    pub(super) fn min(self) -> f64 {
        (self.minutes - 3) as f64 * 60.
    }
    pub(super) fn max(self) -> f64 {
        self.minutes as f64 * 60.
    }
    pub(super) fn accepts(self, seconds: f64) -> bool {
        (self.min()..=self.max()).contains(&seconds)
    }
}
struct Profile {
    tiles: Vec<SegmentTile>,
    duration: f64,
    rate: f64,
    opening: mixlib_planner::ChainOpening,
    #[cfg(feature = "cp-search")]
    opening_accents: Vec<u64>,
}
pub(super) struct Context {
    replays: crate::set_replays::ReplayCache,
    request: Request,
    profiles: Vec<Profile>,
    ids: BTreeMap<String, usize>,
}
/// Counts the actual closing decisions, without turning a proposal into an
/// admitted ending or changing the composition and replay contracts.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub(super) struct CloseDiagnostics {
    pub no_duration_window: u64,
    pub duration_windows: u64,
    pub closing_errors: BTreeMap<String, u64>,
    pub closing_error_details: BTreeMap<String, u64>,
    pub replay_shortfall: u64,
    pub ending_retries: u64,
    pub replay_shortfall_min_raw_seconds: Option<f64>,
    pub replay_shortfall_max_raw_seconds: Option<f64>,
    pub composition_rejections: BTreeMap<String, u64>,
    pub approved_windows: u64,
}
impl CloseDiagnostics {
    pub fn merge(&mut self, other: &Self) {
        self.no_duration_window += other.no_duration_window;
        self.duration_windows += other.duration_windows;
        self.replay_shortfall += other.replay_shortfall;
        self.ending_retries += other.ending_retries;
        self.approved_windows += other.approved_windows;
        if let Some(seconds) = other.replay_shortfall_min_raw_seconds {
            self.replay_shortfall_min_raw_seconds = Some(
                self.replay_shortfall_min_raw_seconds
                    .map_or(seconds, |old| old.min(seconds)),
            );
        }
        if let Some(seconds) = other.replay_shortfall_max_raw_seconds {
            self.replay_shortfall_max_raw_seconds = Some(
                self.replay_shortfall_max_raw_seconds
                    .map_or(seconds, |old| old.max(seconds)),
            );
        }
        for (destination, source) in [
            (&mut self.closing_errors, &other.closing_errors),
            (
                &mut self.closing_error_details,
                &other.closing_error_details,
            ),
            (
                &mut self.composition_rejections,
                &other.composition_rejections,
            ),
        ] {
            for (reason, count) in source {
                *destination.entry(reason.clone()).or_default() += count;
            }
        }
    }
}
const MAX_CLOSING_ENDINGS: usize = 8;

/// A nicer early ending may need more measured loops than this route has. Keep
/// later musical endings alive, without extending the original clock window.
fn later_closing_window(
    window: mixlib_planner::ChainClosingWindow,
    previous_end: u64,
    attempt: usize,
) -> Option<mixlib_planner::ChainClosingWindow> {
    let earliest = previous_end.checked_add(1)?;
    (attempt + 1 < MAX_CLOSING_ENDINGS
        && previous_end >= window.earliest
        && earliest <= window.latest)
        .then_some(mixlib_planner::ChainClosingWindow {
            earliest,
            latest: window.latest,
        })
}
#[derive(Default)]
struct Passage {
    seconds: f64,
    error: f64,
    preferred: f64,
    minimum: f64,
    energy: f64,
    development: f64,
    coverage: f64,
}

impl Passage {
    fn dwell_penalty(&self, overlap_credit: f64) -> f64 {
        let development = self.seconds + overlap_credit;
        let shortage = ((self.preferred - development) / self.preferred.max(1.)).max(0.);
        shortage * shortage * (1. + self.development)
    }
}

#[cfg(feature = "cp-search")]
#[derive(Clone, Copy)]
pub(super) struct ReplayScoreBound {
    pub(super) upper: f64,
    pub(super) without_passage_penalties: f64,
}

const DEVELOPMENT_GRACE_SECONDS: f64 = 0.05;
const PREFERRED_DEVELOPMENT_SECONDS: f64 = 150.;

#[cfg(feature = "cp-search")]
pub(super) fn preferred_development_floor(track_seconds: f64) -> f64 {
    PREFERRED_DEVELOPMENT_SECONDS.min(track_seconds * 0.8)
}

#[derive(Clone, Debug, serde::Serialize)]
pub(super) struct DevelopmentShortfall {
    pub track_id: String,
    pub position: usize,
    pub solo_seconds: f64,
    pub overlap_credit_seconds: f64,
    pub minimum_seconds: f64,
}

#[derive(Debug)]
pub(super) struct PlanRejection {
    pub reason: &'static str,
    pub development: Option<DevelopmentShortfall>,
}

impl PlanRejection {
    fn new(reason: &'static str) -> Self {
        Self {
            reason,
            development: None,
        }
    }
}

fn minimum_development_seconds(track_seconds: f64, development: f64) -> f64 {
    (60. + 30. * development).min(track_seconds * 0.45)
}

/// A bridge endpoint from a pinned pair with a transition on a time-map anchor.
#[cfg(feature = "cp-search")]
#[derive(Clone, Copy)]
pub(super) struct DevelopmentPort {
    frame: u64,
    source_rate: u32,
    output_rate: u32,
    speed: f64,
    credit: f64,
}

#[cfg(feature = "cp-search")]
pub(super) fn measured_development_ports(edge: &MeasuredEdge) -> [Option<DevelopmentPort>; 2] {
    let pair = &edge.plan;
    let transition = &pair.transitions[0];
    let Some(credit) = read_tactics(pair)
        .ok()
        .filter(|tactics| tactics.len() == 1)
        .map(|tactics| overlap_credits(&tactics, 2, f64::from(pair.output.sample_rate_hz)))
    else {
        return [None, None];
    };
    [0, 1].map(|placement| {
        let map = &pair.placements[placement].time_map;
        let slopes = map.slopes();
        let (anchor, output, speed) = if placement == 0 {
            (
                map.anchors.get(1)?,
                transition.start_output_frame,
                *slopes.first()?,
            )
        } else {
            (
                map.anchors.iter().rev().nth(1)?,
                transition.end_output_frame,
                *slopes.last()?,
            )
        };
        (anchor.output_frame == output
            && anchor.source_frame < 1 << 53
            && speed.is_finite()
            && speed > 0.
            && map.source_sample_rate_hz != 0
            && map.output_sample_rate_hz == pair.output.sample_rate_hz
            && map.output_sample_rate_hz != 0)
            .then_some(DevelopmentPort {
                frame: anchor.source_frame,
                source_rate: map.source_sample_rate_hz,
                output_rate: map.output_sample_rate_hz,
                speed,
                credit: credit[placement],
            })
    })
}
#[derive(Clone, Copy)]
struct DurationEdge {
    to: usize,
    exit: u64,
    entry: u64,
}

/// Optimistic union of reachable unused songs. Mutually exclusive branches may
/// both contribute capacity, so this can overestimate, never authorize audio.
struct DurationReach<'a> {
    edges: Vec<Vec<DurationEdge>>,
    frames: Vec<u64>,
    rates: Vec<f64>,
    pacing: &'a [TrackPacing],
    minimum_speed: f64,
}
impl DurationReach<'_> {
    fn remaining_seconds(&self, used: u128, last: usize, entry: u64) -> f64 {
        let mut earliest = vec![None; self.edges.len()];
        earliest[last] = Some(entry);
        let mut queue = VecDeque::from([last]);
        while let Some(from) = queue.pop_front() {
            let at = earliest[from].unwrap();
            for edge in &self.edges[from] {
                if used & (1 << edge.to) != 0 || self.pacing[from].assess(at, edge.exit).is_none() {
                    continue;
                }
                if earliest[edge.to].is_none_or(|old| edge.entry < old) {
                    earliest[edge.to] = Some(edge.entry);
                    queue.push_back(edge.to);
                }
            }
        }
        let current = self.frames[last].saturating_sub(entry) as f64 / self.rates[last];
        let future: f64 = earliest
            .iter()
            .enumerate()
            .filter(|(i, e)| *i != last && e.is_some())
            .map(|(i, _)| self.frames[i] as f64 / self.rates[i])
            .sum();
        (current + future) / self.minimum_speed.max(0.01)
    }
}

pub(super) struct AssemblyResult {
    pub plan: Option<MixPlan>,
    pub bridges: Vec<(usize, u64, f64)>,
    pub stats: AssemblyStats,
}
#[derive(Default, serde::Serialize)]
pub(super) struct AssemblyStats {
    pub examined: usize,
    pub duration_rejections: usize,
    pub development_rejections: usize,
    pub closing_attempts: usize,
    pub closing_rejections: usize,
    pub deepest_tracks: usize,
    pub furthest_join_seconds: f64,
    pub time_budget_exhausted: bool,
}
fn assembly_result(
    mut plan: Option<MixPlan>,
    demands: BTreeMap<(usize, u64), f64>,
    stats: AssemblyStats,
) -> AssemblyResult {
    let mut bridges: Vec<_> = demands
        .into_iter()
        .map(|((track, entry), value)| (track, entry, value))
        .collect();
    bridges.sort_by(|a, b| b.2.total_cmp(&a.2));
    bridges.truncate(16);
    if let Some(p) = &mut plan {
        let evidence = p.header.extensions.get_mut("set_composition").unwrap();
        evidence["assembly"] = json!(stats);
    }
    AssemblyResult {
        plan,
        bridges,
        stats,
    }
}

impl Context {
    pub fn load(
        store: &Store,
        pool: &[Track],
        request: Request,
        cancel: &Cancellation,
    ) -> Result<Self> {
        let mut profiles = vec![];
        for t in pool {
            let a = store.analysis(t)?;
            let reference = a
                .artifacts
                .get("segments")
                .ok_or_else(|| Error::new("SET_ANALYSIS", "segment analysis required"))?;
            let segments: SegmentAnalysis = store.read_ref(reference)?;
            #[cfg(feature = "cp-search")]
            let (opening, opening_accents) =
                mixlib_planner::ChainOpening::read_with_candidates(store, &t.track_id, cancel)?;
            #[cfg(not(feature = "cp-search"))]
            let opening = mixlib_planner::read_chain_opening_cue(store, &t.track_id, cancel)?;
            profiles.push(Profile {
                tiles: segments.tiles.into_iter().filter(|t| t.bars == 1).collect(),
                duration: t.format.frames as f64 / f64::from(t.format.sample_rate_hz),
                rate: f64::from(t.format.sample_rate_hz),
                opening,
                #[cfg(feature = "cp-search")]
                opening_accents,
            });
        }
        // A library-relative acoustic intensity proxy, not emotion recognition.
        // Normalize across the whole pool, never separately within each song.
        let mut values: Vec<f64> = profiles
            .iter()
            .flat_map(|p| p.tiles.iter().map(intensity))
            .collect();
        values.sort_by(f64::total_cmp);
        require(
            !values.is_empty(),
            "SET_ANALYSIS",
            "no measured single-bar passages",
        )?;
        for profile in &mut profiles {
            for tile in &mut profile.tiles {
                let raw = intensity(tile);
                let lower = values.partition_point(|v| *v < raw);
                let upper = values.partition_point(|v| *v <= raw);
                tile.roles.insert(
                    "set_intensity_percentile".into(),
                    (lower + upper) as f64 / (2. * values.len() as f64),
                );
            }
        }
        Ok(Self {
            replays: Default::default(),
            request,
            profiles,
            ids: pool
                .iter()
                .enumerate()
                .map(|(i, t)| (t.track_id.clone(), i))
                .collect(),
        })
    }
    fn passage(&self, index: usize, map: &TimeMap, begin: u64, end: u64) -> Passage {
        self.passage_on_clock(index, map, begin, end, |frame| frame)
    }
    fn passage_on_clock(
        &self,
        index: usize,
        map: &TimeMap,
        begin: u64,
        end: u64,
        clock: impl Fn(f64) -> f64,
    ) -> Passage {
        let rate = f64::from(map.output_sample_rate_hz);
        self.passage_with_arc(index, map, begin, end, |middle, energy| {
            (energy
                - self
                    .request
                    .shape
                    .target(clock(middle) / rate / self.request.max()))
            .abs()
        })
    }

    fn passage_with_arc(
        &self,
        index: usize,
        map: &TimeMap,
        begin: u64,
        end: u64,
        arc_error: impl Fn(f64, f64) -> f64,
    ) -> Passage {
        let p = &self.profiles[index];
        let rate = f64::from(map.output_sample_rate_hz);
        let mut result = Passage {
            seconds: (end - begin) as f64 / rate,
            ..Passage::default()
        };
        // One-second quadrature follows the actual nonlinear playback map.
        // A slow passage therefore has its actual listening-time weight.
        let mut at = begin;
        while at < end {
            let next = (at + u64::from(map.output_sample_rate_hz)).min(end);
            let middle = (at as f64 + next as f64) / 2.;
            let source = map.source_at(middle);
            let tile = p
                .tiles
                .partition_point(|t| t.start_frame as f64 <= source)
                .checked_sub(1)
                .and_then(|i| p.tiles.get(i))
                .filter(|t| source < t.end_frame as f64);
            let weight = (next - at) as f64 / rate;
            if let Some(t) = tile {
                let energy = t.roles["set_intensity_percentile"];
                result.error += weight * arc_error(middle, energy);
                result.energy += weight * energy;
                result.development += weight
                    * t.roles
                        .get("sustained_texture")
                        .copied()
                        .unwrap_or(0.)
                        .max(t.roles.get("build").copied().unwrap_or(0.))
                        .max(t.harmonic_variation.min(1.));
                result.coverage += weight;
            } else {
                result.error += weight * 0.5;
                result.energy += weight * 0.5;
            }
            at = next;
        }
        let seconds = result.seconds.max(1e-9);
        result.error /= seconds;
        result.energy /= seconds;
        result.development /= seconds;
        result.coverage /= seconds;
        // Development-rich songs get a stronger preference for a longer stay.
        // Short recordings still get to contribute a substantial fraction.
        result.minimum = minimum_development_seconds(p.duration, result.development);
        result.preferred =
            (PREFERRED_DEVELOPMENT_SECONDS + 120. * result.development).min(p.duration * 0.8);
        result
    }
    pub(super) fn evaluate(&self, plan: &MixPlan, complete: bool) -> Option<Value> {
        self.evaluate_diagnosed(plan, complete, &mut None)
    }

    #[cfg(feature = "cp-search")]
    pub(super) fn rules_out_development(
        &self,
        track: usize,
        arrival: Option<DevelopmentPort>,
        departure: Option<DevelopmentPort>,
    ) -> bool {
        let (Some(a), Some(b)) = (arrival, departure) else {
            return false;
        };
        if a.source_rate != b.source_rate || a.output_rate != b.output_rate {
            return false;
        }
        let Some(gap) = b.frame.checked_sub(a.frame) else {
            return true;
        };
        // append_pair partitions this span into three intervals, each at
        // no less than the slower endpoint speed. Allow a full output frame
        // per interval for the three rounded lengths and floating arithmetic.
        let output_rate = f64::from(a.output_rate);
        let solo_frames =
            gap as f64 * (output_rate / f64::from(a.source_rate)) / a.speed.min(b.speed);
        if solo_frames >= (1_u64 << 48) as f64 {
            return false;
        }
        let solo_upper = (solo_frames + 3.) / output_rate;
        // All measured development features are nonnegative. Verified overlap
        // credits are fixed by these two tactics, and replay does not satisfy dwell.
        solo_upper + a.credit + b.credit + DEVELOPMENT_GRACE_SECONDS
            < minimum_development_seconds(self.profiles[track].duration, 0.)
    }

    #[cfg(feature = "cp-search")]
    pub(super) fn developed_bridge(
        &self,
        track: usize,
        bridge: &chain::MeasuredBridge,
        arrival: Option<DevelopmentPort>,
        departure: Option<DevelopmentPort>,
    ) -> bool {
        let (Some(a), Some(b)) = (arrival, departure) else {
            return false;
        };
        let credit = a.credit + b.credit + DEVELOPMENT_GRACE_SECONDS;
        let solo = (bridge.solo_end - bridge.solo_start) as f64
            / f64::from(bridge.map.output_sample_rate_hz);
        if solo + credit >= minimum_development_seconds(self.profiles[track].duration, 1.) {
            return true;
        }
        let passage = self.passage(track, &bridge.map, bridge.solo_start, bridge.solo_end);
        solo + credit >= passage.minimum
    }

    fn evaluate_diagnosed(
        &self,
        plan: &MixPlan,
        complete: bool,
        rejection: &mut Option<&'static str>,
    ) -> Option<Value> {
        let mut detail = None;
        let evidence = self.evaluate_plan(plan, complete, false, &mut detail);
        *rejection = detail.map(|d| d.reason);
        evidence
    }

    pub(super) fn evaluate_for_save(
        &self,
        plan: &MixPlan,
        replay_base: bool,
    ) -> std::result::Result<Value, PlanRejection> {
        let mut rejection = None;
        self.evaluate_plan(plan, true, replay_base, &mut rejection)
            .ok_or_else(|| rejection.unwrap_or_else(|| PlanRejection::new("invalid_evidence")))
    }

    fn evaluate_plan(
        &self,
        plan: &MixPlan,
        complete: bool,
        development_first: bool,
        rejection: &mut Option<PlanRejection>,
    ) -> Option<Value> {
        let credits = overlap_credits(
            &read_tactics(plan).ok()?,
            plan.placements.len(),
            f64::from(plan.output.sample_rate_hz),
        );
        let rate = f64::from(plan.output.sample_rate_hz);
        let duration = plan.total_output_frames as f64 / rate;
        let arrangement = read_arrangement(plan).ok()?;
        let base_end = arrangement
            .as_ref()
            .map_or(plan.total_output_frames, |a| a.base_output_frames);
        let outside_clock = complete && !self.request.accepts(duration);
        if outside_clock && !development_first {
            *rejection = Some(PlanRejection::new("duration_outside_window"));
            return None;
        }
        let mut error = 0.;
        let mut dwell = 0.;
        let mut seconds = 0.;
        let mut rows = vec![];
        // Score solo ownership; transitions retain their own measured objective.
        // The provisional last track is unscored until its exit/bookend is known.
        let count = plan.placements.len() - usize::from(!complete);
        for (i, p) in plan.placements.iter().take(count).enumerate() {
            let begin = if i == 0 {
                0
            } else {
                plan.transitions[i - 1].end_output_frame
            };
            let end = plan
                .transitions
                .get(i)
                .map_or(base_end, |t| t.start_output_frame);
            if end <= begin {
                *rejection = Some(PlanRejection::new("no_solo_runway"));
                return None;
            }
            let a = p.time_map.source_at(begin as f64).round() as u64;
            let b = p.time_map.source_at(end as f64).round() as u64;
            let index = self.ids[&p.track_id];
            // Only the last, previously unfinished song changes on extension.
            // Reuse immutable closed passages when all their scoring inputs match.
            let cache_key=canonical_hash(&json!({"map":p.time_map,"range":[begin,end],"credit":credits[i],
                "analysis":p.analysis.hash,"structure":self.request.shape.name(),"minutes":self.request.minutes})).ok()?.digest;
            if let Some(cached) = plan
                .header
                .extensions
                .get("set_composition")
                .and_then(|c| c["passages"].as_array())
                .and_then(|r| r.get(i))
                .filter(|r| {
                    arrangement.is_none()
                        && r["track_id"] == p.track_id
                        && r["cache_key"] == cache_key
                })
            {
                let duration = cached["solo_seconds"].as_f64()?;
                error += cached["arc_error"].as_f64()? * duration;
                seconds += duration;
                dwell += cached["dwell_penalty"].as_f64()?;
                rows.push(cached.clone());
                continue;
            }
            let passage = if let Some(a) = &arrangement {
                self.passage_on_clock(index, &p.time_map, begin, end, |frame| {
                    a.output_at(frame.round() as u64) as f64
                })
            } else {
                self.passage(index, &p.time_map, begin, end)
            };
            let mut replay_seconds = 0.;
            if let Some(a) = &arrangement {
                for r in a.replays.iter().filter(|r| r.placement == i) {
                    let insertion = a.output_at(r.insert_before) - r.added_frames();
                    for repetition in 0..r.repetitions {
                        let output =
                            insertion + u64::from(repetition) * (r.capture_end - r.capture_start);
                        let repeated = self.passage_on_clock(
                            index,
                            &p.time_map,
                            r.capture_start,
                            r.capture_end,
                            |frame| output as f64 + frame - r.capture_start as f64,
                        );
                        error += repeated.error * repeated.seconds;
                        seconds += repeated.seconds;
                        replay_seconds += repeated.seconds;
                    }
                }
            }
            // Bookends are checked only after choosing their musical endpoints.
            let development_seconds = passage.seconds + credits[i];
            if development_seconds + DEVELOPMENT_GRACE_SECONDS < passage.minimum {
                *rejection = Some(PlanRejection {
                    reason: "insufficient_development",
                    development: Some(DevelopmentShortfall {
                        track_id: p.track_id.clone(),
                        position: i + 1,
                        solo_seconds: passage.seconds,
                        overlap_credit_seconds: credits[i],
                        minimum_seconds: passage.minimum,
                    }),
                });
                return None;
            }
            error += passage.error * passage.seconds;
            seconds += passage.seconds;
            let penalty = passage.dwell_penalty(credits[i]);
            dwell += penalty;
            rows.push(json!({"track_id":p.track_id,"solo_seconds":passage.seconds,"replay_seconds":replay_seconds,"overlap_credit_seconds":credits[i],"development_seconds":development_seconds,"minimum_seconds":passage.minimum,
                "preferred_seconds":passage.preferred,"intensity_percentile":passage.energy,"arc_error":passage.error,"development":passage.development,"measured_coverage":passage.coverage,"cache_key":cache_key,"dwell_penalty":penalty,
                "source_start_seconds":a as f64/self.profiles[index].rate,"source_end_seconds":b as f64/self.profiles[index].rate}));
        }
        if outside_clock {
            *rejection = Some(PlanRejection::new("duration_outside_window"));
            return None;
        }
        let arc_error = objective::arc_error(error, seconds);
        let dwell_penalty = dwell / count.max(1) as f64;
        let q = route_quality(plan);
        // A partial route contributes toward the full clock. Dividing by its
        // current length rewards rushing through songs before a set exists.
        // Charge only the heard fraction of arc/development costs until closing.
        let horizon = if complete {
            duration
        } else {
            self.request.max()
        };
        let heard_fraction = if complete {
            1.
        } else {
            (seconds / horizon).clamp(0., 1.)
        };
        let replay_frames = arrangement.as_ref().map_or(0, |a| a.added_frames());
        let replay_fraction = replay_frames as f64 / plan.total_output_frames as f64;
        let transition_shortfall = transition_shortfall(plan);
        // An admitted tactic is feasible even when its fit is below the preferred
        // score. Penalize shortfall continuously instead of deleting the route.
        let transition_fit_penalty = transition_shortfall / (horizon / 60.).max(1.);
        let objective = if complete {
            CompleteScore {
                duration_seconds: duration,
                sustained_quality_seconds: q.sustained(),
                transition_shortfall,
                replay_seconds: replay_frames as f64 / rate,
                arc_error,
                development_penalty: dwell_penalty,
            }
            .value()
        } else {
            q.sustained() / (horizon / 60.).max(1.)
                - transition_fit_penalty
                - (8. * arc_error + 2. * dwell_penalty) * heard_fraction
                - 2. * replay_fraction
        };
        Some(
            json!({"method":"set-composition/3","structure":self.request.shape.name(),"target_minutes":self.request.minutes,
            "minimum_seconds":self.request.min(),"maximum_seconds":self.request.max(),"duration_seconds":duration,
            "complete":complete,"objective":objective,"transition_shortfall":transition_shortfall,"transition_fit_penalty":transition_fit_penalty,"ranking_horizon_seconds":horizon,"scored_fraction":heard_fraction,"replay_fraction":replay_fraction,"arc_error":arc_error,"dwell_penalty":dwell_penalty,"passages":rows,
            "semantics":"Acoustic intensity and development proxies; not measured emotion. Duration includes all overlaps, tempo maps and bookends."}),
        )
    }
    fn closing_window(&self, plan: &MixPlan) -> Option<mixlib_planner::ChainClosingWindow> {
        self.closing_window_from(plan, self.request.min())
    }
    /// The same passage/clock admission for both graph assembly and lazy routes.
    pub(super) fn extend(
        &self,
        prefix: Option<&MixPlan>,
        edge: &MeasuredEdge,
        from: usize,
        entry: u64,
        pacing: &TrackPacing,
    ) -> Result<Option<MixPlan>> {
        self.extend_diagnosed(prefix, edge, from, entry, pacing, &mut None)
    }

    pub(super) fn extend_diagnosed(
        &self,
        prefix: Option<&MixPlan>,
        edge: &MeasuredEdge,
        from: usize,
        entry: u64,
        pacing: &TrackPacing,
        rejection: &mut Option<String>,
    ) -> Result<Option<MixPlan>> {
        let Some(mut plan) = Self::append_measured(prefix, edge, entry, pacing, rejection)? else {
            return Ok(None);
        };
        if prefix.is_none() {
            let opening = self.profiles[from]
                .opening
                .before(plan.placements[0].source_start_frame);
            chain::open_measured(&mut plan, opening)?;
        }
        let end = plan.transitions.last().unwrap().end_output_frame as f64
            / f64::from(plan.output.sample_rate_hz);
        if end >= self.request.max() {
            *rejection = Some("join_end_at_or_after_max_duration".into());
            return Ok(None);
        }
        let mut composition_rejection = None;
        let Some(evidence) = self.evaluate_diagnosed(&plan, false, &mut composition_rejection)
        else {
            *rejection = Some(composition_rejection.unwrap_or("invalid_evidence").into());
            return Ok(None);
        };
        plan.header
            .extensions
            .insert("set_composition".into(), evidence);
        Ok(Some(plan))
    }

    pub(super) fn append_measured(
        prefix: Option<&MixPlan>,
        edge: &MeasuredEdge,
        entry: u64,
        pacing: &TrackPacing,
        rejection: &mut Option<String>,
    ) -> Result<Option<MixPlan>> {
        if edge.last_resort {
            *rejection = Some("last_resort_join".into());
            return Ok(None);
        }
        let Some(paced) = pacing.assess(entry, edge.exit) else {
            *rejection = Some("phrase_pacing".into());
            return Ok(None);
        };
        let mut plan =
            match chain::extend_measured(prefix, &edge.plan, edge.score, edge.last_resort) {
                Ok(p) => p,
                Err(e) if measured_beam::route_local(&e) => {
                    *rejection = Some(e.code);
                    return Ok(None);
                }
                Err(e) => return Err(e),
            };
        if let Some(j) = plan
            .header
            .extensions
            .get_mut("chain_joins")
            .and_then(Value::as_array_mut)
            .and_then(|j| j.last_mut())
        {
            j["pacing"] = serde_json::to_value(paced)?;
        }
        Ok(Some(plan))
    }

    #[cfg(feature = "cp-search")]
    pub(super) fn opening_choices(&self, plan: &MixPlan) -> Vec<u64> {
        let first = &plan.placements[0];
        let profile = &self.profiles[self.ids[&first.track_id]];
        profile
            .opening
            .candidates_before(first.source_start_frame, &profile.opening_accents)
    }

    /// Closing is a real musical endpoint, optionally followed by verified loops.
    pub(super) fn close_with_diagnostics(
        &self,
        store: &Store,
        plan: &MixPlan,
        cancel: &Cancellation,
    ) -> Result<(Option<MixPlan>, CloseDiagnostics)> {
        self.close_in_scope(store, plan, true, cancel)
    }

    #[cfg(feature = "cp-search")]
    pub(super) fn close_original(
        &self,
        store: &Store,
        plan: &MixPlan,
        cancel: &Cancellation,
    ) -> Result<(Option<MixPlan>, CloseDiagnostics)> {
        self.close_in_scope(store, plan, false, cancel)
    }

    fn close_in_scope(
        &self,
        store: &Store,
        plan: &MixPlan,
        allow_replay: bool,
        cancel: &Cancellation,
    ) -> Result<(Option<MixPlan>, CloseDiagnostics)> {
        let mut diagnostics = CloseDiagnostics::default();
        let mut best = None;
        let direct = std::iter::once(self.closing_window(plan));
        let replay = allow_replay.then(|| {
            self.closing_window_from(plan, self.request.min() / (1. + MAX_REPLAY_FRACTION))
        });
        for window in direct.chain(replay) {
            let Some(mut window) = window else {
                diagnostics.no_duration_window += 1;
                continue;
            };
            diagnostics.duration_windows += 1;
            for attempt in 0..MAX_CLOSING_ENDINGS {
                cancel.check()?;
                diagnostics.ending_retries += u64::from(attempt > 0);
                match chain::finish_measured_in_window(store, plan.clone(), Some(window), cancel) {
                    Ok(finished) => {
                        let raw_seconds = finished.total_output_frames as f64
                            / f64::from(finished.output.sample_rate_hz);
                        let ending = finished.placements.last().unwrap().source_end_frame;
                        let candidate = if allow_replay {
                            self.replays.arrange(
                                store,
                                &finished,
                                self.request.min(),
                                self.request.max(),
                                cancel,
                            )?
                        } else {
                            self.request.accepts(raw_seconds).then_some(finished)
                        };
                        if let Some(mut finished) = candidate {
                            let mut rejection = None;
                            if let Some(evidence) =
                                self.evaluate_diagnosed(&finished, true, &mut rejection)
                            {
                                diagnostics.approved_windows += 1;
                                finished
                                    .header
                                    .extensions
                                    .insert("set_composition".into(), evidence);
                                if best.as_ref().is_none_or(|p| better(&finished, p)) {
                                    best = Some(finished);
                                }
                            } else {
                                *diagnostics
                                    .composition_rejections
                                    .entry(rejection.unwrap_or("invalid_evidence").into())
                                    .or_default() += 1;
                            }
                        } else if allow_replay {
                            diagnostics.replay_shortfall += 1;
                            diagnostics.replay_shortfall_min_raw_seconds = Some(
                                diagnostics
                                    .replay_shortfall_min_raw_seconds
                                    .map_or(raw_seconds, |old| old.min(raw_seconds)),
                            );
                            diagnostics.replay_shortfall_max_raw_seconds = Some(
                                diagnostics
                                    .replay_shortfall_max_raw_seconds
                                    .map_or(raw_seconds, |old| old.max(raw_seconds)),
                            );
                            if let Some(later) = later_closing_window(window, ending, attempt) {
                                window = later;
                                continue;
                            }
                        } else {
                            *diagnostics
                                .composition_rejections
                                .entry("duration_outside_window".into())
                                .or_default() += 1;
                        }
                    }
                    Err(e) if measured_beam::route_local(&e) => {
                        *diagnostics.closing_errors.entry(e.code).or_default() += 1;
                        *diagnostics
                            .closing_error_details
                            .entry(e.message)
                            .or_default() += 1;
                    }
                    Err(e) => return Err(e),
                }
                break;
            }
        }
        Ok((best, diagnostics))
    }
    /// Bound every completion of these exact joins. Bookending and replays
    /// preserve their fit, measured quality, and pacing charges.
    #[cfg(all(feature = "cp-search", test))]
    pub(super) fn closing_score_upper(&self, plan: &MixPlan, fixed_ending: bool) -> Option<f64> {
        self.closing_score_envelope(plan, fixed_ending)
            .upper_bound()
    }

    #[cfg(feature = "cp-search")]
    fn closing_score_envelope(
        &self,
        plan: &MixPlan,
        fixed_ending: bool,
    ) -> mixlib_set_search::bounds::ScoreEnvelope {
        let mut minimum = self.request.min();
        let mut maximum = self.request.max();
        let mut replay_lower = 0.;
        if fixed_ending {
            let rate = f64::from(plan.output.sample_rate_hz);
            let base = plan.total_output_frames;
            let budget = (base as f64 * MAX_REPLAY_FRACTION).floor() as u64;
            minimum = minimum.max(base as f64 / rate);
            maximum = maximum.min(base.saturating_add(budget) as f64 / rate);
            let required = ((self.request.min() * rate).ceil() as u64).saturating_sub(base);
            replay_lower = required as f64 / rate;
        }
        mixlib_set_search::bounds::ScoreEnvelope {
            min_duration_seconds: minimum,
            max_duration_seconds: maximum,
            sustained_quality_upper: route_quality(plan).sustained(),
            transition_shortfall_lower: transition_shortfall(plan),
            replay_seconds_lower: replay_lower,
            arc_error_lower: 0.,
            development_penalty_lower: 0.,
        }
    }

    #[cfg(feature = "cp-search")]
    pub(super) fn replay_score_bound(&self, plan: &MixPlan) -> Option<ReplayScoreBound> {
        let envelope = self.closing_score_envelope(plan, true);
        let penalties = self.replay_passage_penalty_bounds(plan, envelope.max_duration_seconds);
        Self::penalty_adjusted_bound(envelope, penalties)
    }

    /// Match the rounded source-to-output extension in chain::apply_bookends.
    /// Every eligible closing cue is at or before source_frames.
    #[cfg(feature = "cp-search")]
    pub(super) fn latest_ending_output_frame(&self, plan: &MixPlan) -> Option<u64> {
        let first = plan.placements.first()?;
        let last = plan.placements.last()?;
        if plan
            .header
            .extensions
            .get("selected_opening_source_frame")?
            .as_u64()
            != Some(first.source_start_frame)
            || !matches!(plan.output.sample_rate_hz, 44100 | 48000)
            || read_arrangement(plan).ok()?.is_some()
        {
            return None;
        }
        let profile = self.profiles.get(*self.ids.get(&last.track_id)?)?;
        let map = &last.time_map;
        let tail = map.anchors.iter().rev().nth(1)?;
        let speed = *map.slopes().last()?;
        if !(0.5..=2.).contains(&speed)
            || !(0. ..=86_400.).contains(&profile.duration)
            || profile.rate != f64::from(map.source_sample_rate_hz)
            || map.output_sample_rate_hz != plan.output.sample_rate_hz
        {
            return None;
        }
        let source_tail = self.source_frames(last).checked_sub(tail.source_frame)?;
        let output_per_source = f64::from(plan.output.sample_rate_hz) / profile.rate / speed;
        tail.output_frame
            .checked_add((source_tail as f64 * output_per_source).round() as u64)
    }

    /// The opening and all joins are fixed. A final fade can only alter the
    /// last song, and legal replays may shift the existing narrative clock.
    #[cfg(feature = "cp-search")]
    pub(super) fn fixed_opening_score_bound(
        &self,
        plan: &MixPlan,
        latest_base: Option<u64>,
    ) -> Option<ReplayScoreBound> {
        let coarse = self.closing_score_envelope(plan, false);
        let refine = || {
            let latest_base = latest_base?;
            let last_join_end = plan.transitions.last()?.end_output_frame;
            let tail = plan
                .placements
                .last()?
                .time_map
                .anchors
                .iter()
                .rev()
                .nth(1)?;
            if tail.output_frame != last_join_end {
                return None;
            }
            let rate = f64::from(plan.output.sample_rate_hz);
            let high = (self.request.max() * rate).floor() as u64;
            let base_cap = latest_base.min(high);
            let replay_cap = ((base_cap as f64 * MAX_REPLAY_FRACTION).floor() as u64)
                .min(high.checked_sub(tail.output_frame)?);
            let mut envelope = coarse;
            envelope.max_duration_seconds = envelope
                .max_duration_seconds
                .min(latest_base.checked_add(replay_cap)? as f64 / rate);
            let minimum = (self.request.min() * rate).ceil() as u64;
            envelope.replay_seconds_lower = minimum.saturating_sub(latest_base) as f64 / rate;
            let penalties = self.original_passage_penalty_bounds(
                plan,
                None,
                replay_cap,
                envelope.max_duration_seconds,
            );
            Self::penalty_adjusted_bound(envelope, penalties)
        };
        let coarse_upper = coarse.upper_bound()?;
        Some(ReplayScoreBound {
            upper: refine().map_or(coarse_upper, |bound| bound.upper.min(coarse_upper)),
            without_passage_penalties: coarse_upper,
        })
    }

    #[cfg(feature = "cp-search")]
    fn penalty_adjusted_bound(
        mut envelope: mixlib_set_search::bounds::ScoreEnvelope,
        penalties: Option<(f64, f64)>,
    ) -> Option<ReplayScoreBound> {
        let coarse = envelope.upper_bound()?;
        if let Some((arc, dwell)) = penalties {
            envelope.arc_error_lower = arc;
            envelope.development_penalty_lower = dwell;
        }
        Some(ReplayScoreBound {
            upper: envelope
                .upper_bound()
                .map_or(coarse, |upper| upper.min(coarse)),
            without_passage_penalties: coarse,
        })
    }

    #[cfg(feature = "cp-search")]
    fn replay_passage_penalty_bounds(&self, plan: &MixPlan, maximum: f64) -> Option<(f64, f64)> {
        let rate = f64::from(plan.output.sample_rate_hz);
        let base = plan.total_output_frames;
        let high = (self.request.max() * rate).floor() as u64;
        let budget =
            ((base as f64 * MAX_REPLAY_FRACTION).floor() as u64).min(high.checked_sub(base)?);
        self.original_passage_penalty_bounds(plan, Some(base), budget, maximum)
    }

    #[cfg(feature = "cp-search")]
    fn original_passage_penalty_bounds(
        &self,
        plan: &MixPlan,
        fixed_end: Option<u64>,
        budget: u64,
        maximum: f64,
    ) -> Option<(f64, f64)> {
        let count = plan.placements.len();
        let rate = f64::from(plan.output.sample_rate_hz);
        if !(1..=128).contains(&count)
            || plan.transitions.len() + 1 != count
            || !matches!(plan.output.sample_rate_hz, 44100 | 48000)
            || maximum < self.request.min()
            || !maximum.is_finite()
            || read_arrangement(plan).ok()?.is_some()
        {
            return None;
        }
        let credits = overlap_credits(&read_tactics(plan).ok()?, count, rate);
        let mut weighted_error = 0.;
        let mut dwell = 0.;
        let fixed_passages = if fixed_end.is_some() {
            count
        } else {
            count - 1
        };
        for (i, placement) in plan.placements.iter().take(fixed_passages).enumerate() {
            let begin = i
                .checked_sub(1)
                .map_or(0, |prev| plan.transitions[prev].end_output_frame);
            let end = plan
                .transitions
                .get(i)
                .map(|next| next.start_output_frame)
                .or(fixed_end)?;
            if begin >= end
                || end as f64 / rate > maximum
                || placement.time_map.output_sample_rate_hz != plan.output.sample_rate_hz
            {
                return None;
            }
            let index = *self.ids.get(&placement.track_id)?;
            let passage =
                self.passage_with_arc(index, &placement.time_map, begin, end, |middle, energy| {
                    // The original midpoint can move by any amount from zero to
                    // the entire remaining replay budget. Include the scorer's
                    // integer midpoint rounding as well as the base evaluation.
                    let rounded = middle.round() as u64;
                    let first = middle.min(rounded as f64);
                    let last = middle.max((rounded + budget) as f64);
                    let [lower, upper] = self.request.shape.target_range(
                        first / rate / self.request.max(),
                        last / rate / self.request.max(),
                    );
                    (lower - energy).max(energy - upper).max(0.)
                });
            weighted_error += passage.error * passage.seconds;
            dwell += passage.dwell_penalty(credits[i]);
        }
        // Original contributions sum in the same order as the scorer.
        // Omitted final passages and replays contribute nonnegative error.
        // All scored intervals fit in
        // the output clock, with one division and addition per original or
        // replay contribution (at most four per placement). Use an ULP at
        // twice the maximum clock for those conversions and accumulations.
        let headroom = 2. * maximum;
        let ulp = headroom.next_up() - headroom;
        let denominator = (maximum + (8 * count + 1) as f64 * ulp).next_up();
        Some((
            objective::arc_error(weighted_error, denominator),
            dwell / count as f64,
        ))
    }

    #[cfg(feature = "cp-search")]
    pub(super) fn ending_choices(
        &self,
        store: &Store,
        plan: &MixPlan,
        cancel: &Cancellation,
    ) -> Result<(
        Option<mixlib_planner::ChainClosingWindow>,
        Vec<mixlib_planner::ChainBookends>,
    )> {
        let last = plan.placements.last().unwrap();
        let map = &last.time_map;
        let solo = &map.anchors[map.anchors.len() - 2];
        let rate = f64::from(map.output_sample_rate_hz);
        let source_per_output =
            map.slopes().last().unwrap() * f64::from(map.source_sample_rate_hz) / rate;
        let to_source = |output: f64| {
            solo.source_frame as f64 + (output - solo.output_frame as f64) * source_per_output
        };
        // Expand past the rounding boundaries. Frame validation and musical
        // development happen for each concrete ending after assembly.
        let earliest_output = (self.request.min() * rate / (1. + MAX_REPLAY_FRACTION))
            .floor()
            .max(solo.output_frame as f64);
        let earliest = (to_source(earliest_output - 1.).floor() as u64).max(solo.source_frame + 1);
        let latest = to_source(self.request.max() * rate + 1.).ceil() as u64;
        let latest = latest.min(self.source_frames(last));
        if earliest > latest {
            return Ok((None, Vec::new()));
        }
        let window = mixlib_planner::ChainClosingWindow { earliest, latest };
        let endings = mixlib_planner::chain_ending_candidates(
            store,
            plan.placements[0].source_start_frame,
            last,
            Some(window),
            cancel,
        )?;
        Ok((Some(window), endings))
    }

    #[cfg(feature = "cp-search")]
    fn source_frames(&self, p: &Placement) -> u64 {
        let profile = &self.profiles[self.ids[&p.track_id]];
        (profile.rate * profile.duration).round() as u64
    }

    #[cfg(feature = "cp-search")]
    pub(super) fn prepare_replay(&self, plan: &MixPlan) -> crate::set_replays::ReplayPreparation {
        crate::set_replays::ReplayPreparation::new(plan, self.request.min(), self.request.max())
    }

    #[cfg(feature = "cp-search")]
    pub(super) fn advance_replay(
        &self,
        store: &Store,
        plan: &MixPlan,
        prep: &mut crate::set_replays::ReplayPreparation,
        cancel: &Cancellation,
    ) -> Result<Option<crate::set_replays::ReplayDomain>> {
        self.replays.advance_preparation(store, plan, prep, cancel)
    }

    fn closing_window_from(
        &self,
        plan: &MixPlan,
        minimum: f64,
    ) -> Option<mixlib_planner::ChainClosingWindow> {
        let last = plan.placements.last()?;
        let index = self.ids[&last.track_id];
        let profile = &self.profiles[index];
        let map = &last.time_map;
        let rate = f64::from(map.output_sample_rate_hz);
        let solo = &map.anchors[map.anchors.len() - 2];
        let source_per_output = map.slopes().last()? * profile.rate / rate;
        let source_end = (profile.duration * profile.rate).round() as u64;
        let to_source = |output: u64| {
            solo.source_frame as f64
                + (output as f64 - solo.output_frame as f64) * source_per_output
        };
        let source_limit = solo.output_frame as f64
            + source_end.saturating_sub(solo.source_frame) as f64 / source_per_output;
        let latest = (self.request.max() * rate).min(source_limit).floor() as u64;
        let begin = plan.transitions.last()?.end_output_frame;
        let credit =
            *overlap_credits(&read_tactics(plan).ok()?, plan.placements.len(), rate).last()?;
        let mut earliest = ((minimum * rate).ceil() as u64).max(begin + 1);
        if earliest > latest {
            return None;
        }
        let developed = |end: u64| {
            let mut extended = map.clone();
            *extended.anchors.last_mut().unwrap() = Anchor {
                source_frame: to_source(end).round() as u64,
                output_frame: end,
            };
            let passage = self.passage(index, &extended, begin, end);
            passage.seconds + credit >= passage.minimum
        };
        if !developed(latest) {
            return None;
        }
        // The minimum grows more slowly than elapsed solo time. Find its first
        // satisfied output second, then keep every later musical ending alive.
        if !developed(earliest) {
            let mut high = latest;
            while high - earliest > map.output_sample_rate_hz as u64 {
                let middle = earliest + (high - earliest) / 2;
                if developed(middle) {
                    high = middle;
                } else {
                    earliest = middle;
                }
            }
            earliest = high;
        }
        Some(mixlib_planner::ChainClosingWindow {
            earliest: to_source(earliest).ceil() as u64,
            latest: to_source(latest).floor().min(source_end as f64) as u64,
        })
    }
    #[allow(clippy::too_many_arguments)]
    pub fn assemble(
        &self,
        store: &Store,
        graph: &[Vec<MeasuredEdge>],
        pool: &[Track],
        pacing: &[TrackPacing],
        policy: &MixingConfig,
        deadline: Instant,
        cancel: &Cancellation,
    ) -> Result<AssemblyResult> {
        struct Route {
            used: u128,
            last: usize,
            entry: u64,
            plan: Option<MixPlan>,
        }
        let n = pool.len();
        let required = crate::required_pairs::RequiredPairs::resolve(store, policy, pool, n)?;
        let width = (policy.beam_width * 4).clamp(32, 256);
        let anchors = strongest_anchors(graph);
        let reach = DurationReach {
            edges: (0..n)
                .map(|from| {
                    (0..n)
                        .flat_map(|to| {
                            graph[from * n + to]
                                .iter()
                                .filter(|e| !e.last_resort)
                                .map(move |e| DurationEdge {
                                    to,
                                    exit: e.exit,
                                    entry: e.entry_end,
                                })
                        })
                        .collect()
                })
                .collect(),
            frames: pool.iter().map(|t| t.format.frames).collect(),
            rates: pool
                .iter()
                .map(|t| f64::from(t.format.sample_rate_hz))
                .collect(),
            pacing,
            minimum_speed: graph
                .iter()
                .flatten()
                .flat_map(|e| e.plan.placements.iter())
                .map(|p| p.time_map.speed_ratio_limits[0])
                .fold(1. - policy.max_speed_deviation, f64::min),
        };
        let mut stats = AssemblyStats::default();
        let mut demands = BTreeMap::<(usize, u64), f64>::new();
        let mut beam: Vec<_> = (0..n)
            .filter(|&i| required.start_allowed(i))
            .map(|i| Route {
                used: 1 << i,
                last: i,
                entry: 0,
                plan: None,
            })
            .collect();
        let mut best = None;
        for _depth in 2..=n {
            let mut next = vec![];
            for route in &beam {
                if Instant::now() >= deadline {
                    stats.time_budget_exhausted = true;
                    return Ok(assembly_result(best, demands, stats));
                }
                cancel.check()?;
                for to in 0..n {
                    if !required.allows(
                        route.used,
                        route.last,
                        to,
                        n - route.used.count_ones() as usize - 1,
                    ) {
                        continue;
                    }
                    for e in &graph[route.last * n + to] {
                        if stats.examined % 64 == 0 {
                            cancel.check()?;
                            if Instant::now() >= deadline {
                                stats.time_budget_exhausted = true;
                                return Ok(assembly_result(best, demands, stats));
                            }
                        }
                        // Emergency joins remain inadmissible; ordinary fit is scored.
                        if e.last_resort {
                            continue;
                        }
                        stats.examined += 1;
                        let Some(plan) = self.extend(
                            route.plan.as_ref(),
                            e,
                            route.last,
                            route.entry,
                            &pacing[route.last],
                        )?
                        else {
                            stats.development_rejections += 1;
                            continue;
                        };
                        let join_end = plan.transitions.last().unwrap().end_output_frame as f64
                            / f64::from(plan.output.sample_rate_hz);
                        stats.deepest_tracks = stats.deepest_tracks.max(plan.placements.len());
                        stats.furthest_join_seconds = stats.furthest_join_seconds.max(join_end);
                        let remaining =
                            reach.remaining_seconds(route.used | (1 << to), to, e.entry_end);
                        if (join_end + remaining) * (1. + MAX_REPLAY_FRACTION) < self.request.min()
                        {
                            stats.duration_rejections += 1;
                            let anchor_value = plan
                                .header
                                .extensions
                                .get("chain_joins")
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten()
                                .map(|j| {
                                    j["dense_quality_seconds"].as_f64().unwrap_or(0.)
                                        + j["handoff_quality_seconds"].as_f64().unwrap_or(0.)
                                })
                                .fold(0., f64::max);
                            if anchor_value > 0. {
                                demands
                                    .entry((to, e.entry_end))
                                    .and_modify(|v| *v = v.max(anchor_value))
                                    .or_insert(anchor_value);
                            }
                            continue;
                        }
                        let closing = [
                            self.closing_window(&plan),
                            self.closing_window_from(
                                &plan,
                                self.request.min() / (1. + MAX_REPLAY_FRACTION),
                            ),
                        ];
                        for window in closing
                            .into_iter()
                            .flatten()
                            .filter(|_| required.satisfied(&plan))
                        {
                            stats.closing_attempts += 1;
                            // Failure to close this song must not discard a
                            // prefix that can still continue to another song.
                            match chain::finish_measured_in_window(
                                store,
                                plan.clone(),
                                Some(window),
                                cancel,
                            ) {
                                Ok(finished) => {
                                    let finished = self.replays.arrange(
                                        store,
                                        &finished,
                                        self.request.min(),
                                        self.request.max(),
                                        cancel,
                                    )?;
                                    if let Some(mut finished) = finished {
                                        required.annotate(&mut finished)?;
                                        if let Some(evidence) = self.evaluate(&finished, true) {
                                            finished
                                                .header
                                                .extensions
                                                .insert("set_composition".into(), evidence);
                                            if best.as_ref().is_none_or(|p| better(&finished, p)) {
                                                best = Some(finished);
                                            }
                                        }
                                    }
                                }
                                Err(err) if measured_beam::route_local(&err) => {
                                    stats.closing_rejections += 1;
                                }
                                Err(err) => return Err(err),
                            }
                        }
                        next.push(Route {
                            used: route.used | (1 << to),
                            last: to,
                            entry: e.entry_end,
                            plan: Some(plan),
                        });
                        if next.len() >= width * 16 {
                            prune_routes(
                                &mut next,
                                width * 4,
                                |r| r.plan.as_ref().unwrap(),
                                |r| (r.last, r.entry, r.used),
                                &anchors,
                                &required,
                            );
                        }
                    }
                }
            }
            prune_routes(
                &mut next,
                width,
                |r| r.plan.as_ref().unwrap(),
                |r| (r.last, r.entry, r.used),
                &anchors,
                &required,
            );
            if next.is_empty() {
                break;
            }
            beam = next;
        }
        Ok(assembly_result(best, demands, stats))
    }
}
fn transition_shortfall(plan: &MixPlan) -> f64 {
    plan.header
        .extensions
        .get("chain_joins")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|j| {
            objective::transition_shortfall(j["score"].as_f64().unwrap_or(PREFERRED_TRANSITION_FIT))
        })
        .sum()
}

fn overlap_credits(tactics: &[TacticPlan], count: usize, rate: f64) -> Vec<f64> {
    let mut credit = vec![0.; count];
    for t in tactics {
        let seconds = t.hold_end_frame.saturating_sub(t.start_frame) as f64 / rate;
        // Only measured defining tactics earn a conservative half-time credit.
        // A nominal lane envelope alone is not proof of audible song identity.
        if matches!(t.tactic_id, TacticId::M01 | TacticId::S05 | TacticId::S06) {
            credit[t.outgoing_placement] += 0.5 * seconds;
        }
        if t.tactic_id == TacticId::M01 {
            credit[t.incoming_placement] += 0.5 * seconds;
        }
    }
    credit
}

fn intensity(t: &SegmentTile) -> f64 {
    0.45 * t.band_occupancy.iter().sum::<f64>() / 3.
        + 0.25 * (t.onset_rate_hz / 8.).min(1.)
        + 0.15 * t.band_occupancy[2]
        + 0.15 * (t.rms * 5.).min(1.)
}
/// Protect a few defining measured cue/tactic families during partial ranking.
/// Long lead-ins must not erase an excellent anchor before a full set is scored.
struct HeldAnchor {
    from: String,
    to: String,
    exit: u64,
    entry: u64,
    tactic: Value,
    value: f64,
}
impl HeldAnchor {
    fn occurs_in(&self, plan: &MixPlan) -> bool {
        plan.header
            .extensions
            .get("chain_joins")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|j| {
                j["from"] == self.from
                    && j["to"] == self.to
                    && j["outgoing_cue_frame"] == self.exit
                    && j["incoming_cue_frame"] == self.entry
                    && j["choice"] == self.tactic
            })
    }
}
fn strongest_anchors(graph: &[Vec<MeasuredEdge>]) -> Vec<HeldAnchor> {
    let mut anchors: Vec<_> = graph
        .iter()
        .flatten()
        .filter(|e| {
            !e.last_resort
                && e.score >= 25.
                && e.dense_quality_seconds + e.handoff_quality_seconds > 0.
        })
        .filter_map(|e| {
            Some(HeldAnchor {
                from: e.plan.placements[0].track_id.clone(),
                to: e.plan.placements[1].track_id.clone(),
                exit: e.exit,
                entry: e.plan.placements[1].source_start_frame,
                tactic: serde_json::to_value(read_tactics(&e.plan).ok()?.first()?.tactic_id)
                    .ok()?,
                value: e.dense_quality_seconds + e.handoff_quality_seconds,
            })
        })
        .collect();
    anchors.sort_by(|a, b| b.value.total_cmp(&a.value));
    let mut seen = std::collections::BTreeSet::new();
    anchors.retain(|a| {
        seen.insert((
            a.from.clone(),
            a.to.clone(),
            a.exit,
            a.entry,
            a.tactic.to_string(),
        ))
    });
    anchors.truncate(8);
    anchors
}
fn prune_routes<T>(
    routes: &mut Vec<T>,
    limit: usize,
    plan: impl Fn(&T) -> &MixPlan,
    identity: impl Fn(&T) -> (usize, u64, u128),
    anchors: &[HeldAnchor],
    required: &crate::required_pairs::RequiredPairs,
) {
    routes.sort_by(|a, b| rank(plan(b)).total_cmp(&rank(plan(a))));
    let mut reserved = std::collections::BTreeSet::new();
    for anchor in anchors {
        if reserved.len() >= (limit / 4).max(1) {
            break;
        }
        if let Some((index, _)) = routes
            .iter()
            .enumerate()
            .find(|(_, r)| anchor.occurs_in(plan(r)))
        {
            reserved.insert(index);
        }
    }
    // Deduplicating time bins inside each used-song identity does not preserve
    // global clock diversity. Reserve the best route in evenly spread elapsed
    // bins as well, including the furthest developed prefix.
    let mut elapsed = BTreeMap::new();
    for (index, route) in routes.iter().enumerate() {
        let p = plan(route);
        let committed = p
            .transitions
            .last()
            .map_or(p.total_output_frames, |t| t.end_output_frame);
        let bucket = committed / (120 * u64::from(p.output.sample_rate_hz));
        elapsed.entry(bucket).or_insert(index);
    }
    let time_representatives: Vec<_> = elapsed.into_values().collect();
    let slots = (limit / 4).max(1).min(time_representatives.len());
    for slot in 0..slots {
        let index = if slots == 1 {
            time_representatives.len() - 1
        } else {
            slot * (time_representatives.len() - 1) / (slots - 1)
        };
        reserved.insert(time_representatives[index]);
    }
    // Keep different fulfilled-requirement prefixes alive while cue choices
    // still compete. A higher unconstrained score cannot erase every must-pair.
    let mut progress = BTreeMap::new();
    for (index, route) in routes.iter().enumerate() {
        let mask = required.progress(identity(route).2);
        if mask != 0 {
            progress.entry(mask).or_insert(index);
        }
    }
    let mut groups: Vec<_> = progress.into_iter().collect();
    groups.sort_by_key(|(mask, index)| (std::cmp::Reverse(mask.count_ones()), *index));
    for (_, index) in groups.into_iter().take((limit / 4).max(1)) {
        reserved.insert(index);
    }
    let mut ordered: Vec<_> = routes.drain(..).enumerate().collect();
    ordered.sort_by_key(|(index, _)| (!reserved.contains(index), *index));
    let mut regions = BTreeMap::new();
    let mut kept = vec![];
    for (_, r) in ordered {
        let p = plan(&r);
        let (last, entry, used) = identity(&r);
        let key = (
            used,
            last,
            entry / (30 * u64::from(p.placements.last().unwrap().time_map.source_sample_rate_hz)),
            p.total_output_frames / (120 * u64::from(p.output.sample_rate_hz)),
        );
        let count = regions.entry(key).or_insert(0usize);
        if *count < 2 {
            *count += 1;
            kept.push(r);
            if kept.len() == limit {
                break;
            }
        }
    }
    *routes = kept;
}
fn rank(p: &MixPlan) -> f64 {
    p.header
        .extensions
        .get("set_composition")
        .and_then(|v| v["objective"].as_f64())
        .unwrap_or(f64::NEG_INFINITY)
}
pub(super) fn score(p: &MixPlan) -> f64 {
    rank(p)
}
pub(super) fn better(a: &MixPlan, b: &MixPlan) -> bool {
    if a.header.extensions.contains_key("set_composition") {
        rank(a) > rank(b)
    } else {
        route_quality(a).best_first(route_quality(b)).is_lt()
    }
}

#[cfg(test)]
impl Context {
    /// Metadata-only fixture for orchestration tests. Real searches always load
    /// measured profiles through Context::load.
    pub(super) fn fixture(pool: &[Track], request: Request) -> Self {
        Self {
            replays: Default::default(),
            request,
            profiles: pool
                .iter()
                .map(|t| Profile {
                    tiles: vec![],
                    duration: t.format.frames as f64 / f64::from(t.format.sample_rate_hz),
                    rate: f64::from(t.format.sample_rate_hz),
                    opening: Default::default(),
                    #[cfg(feature = "cp-search")]
                    opening_accents: vec![],
                })
                .collect(),
            ids: pool
                .iter()
                .enumerate()
                .map(|(i, t)| (t.track_id.clone(), i))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn passage_map(start: u64, end: u64, seconds: u64) -> TimeMap {
        let plan: MixPlan =
            serde_json::from_str(include_str!("../../../examples/mix-plan.json")).unwrap();
        let mut map = plan.placements[0].time_map.clone();
        map.source_sample_rate_hz = 100;
        map.output_sample_rate_hz = 100;
        map.anchors = vec![
            Anchor {
                source_frame: start,
                output_frame: 0,
            },
            Anchor {
                source_frame: end,
                output_frame: seconds * 100,
            },
        ];
        map
    }
    fn long_plan(count: usize) -> (Context, MixPlan) {
        let mut plan: MixPlan =
            serde_json::from_str(include_str!("../../../examples/mix-plan.json")).unwrap();
        let template = plan.placements[0].clone();
        let transition = plan.transitions[0].clone();
        let rate = u64::from(plan.output.sample_rate_hz);
        plan.placements.clear();
        plan.transitions.clear();
        let mut profiles = vec![];
        let mut ids = BTreeMap::new();
        for i in 0..count {
            let mut p = template.clone();
            p.track_id = format!("song-{i}");
            ids.insert(p.track_id.clone(), i);
            let start = i as u64 * 170 * rate;
            p.source_start_frame = 0;
            p.source_end_frame = 180 * rate;
            p.time_map.anchors = vec![
                Anchor {
                    source_frame: 0,
                    output_frame: start,
                },
                Anchor {
                    source_frame: 180 * rate,
                    output_frame: start + 180 * rate,
                },
            ];
            plan.placements.push(p);
            if i > 0 {
                let mut t = transition.clone();
                t.start_output_frame = start;
                t.end_output_frame = start + 10 * rate;
                plan.transitions.push(t);
            }
            profiles.push(Profile {
                tiles: vec![],
                duration: 180.,
                rate: rate as f64,
                opening: Default::default(),
                #[cfg(feature = "cp-search")]
                opening_accents: vec![],
            });
        }
        plan.total_output_frames = (count as u64 * 170 + 10) * rate;
        (
            Context {
                replays: Default::default(),
                request: Request::new(if count > 30 { 120 } else { 60 }, "auto").unwrap(),
                profiles,
                ids,
            },
            plan,
        )
    }
    #[test]
    fn an_admitted_join_below_preferred_fit_remains_feasible_with_a_continuous_penalty() {
        let (context, mut plan) = long_plan(21);
        plan.header
            .extensions
            .insert("chain_joins".into(), json!([{"score":25.0}]));
        let preferred = context.evaluate(&plan, true).unwrap();
        plan.header.extensions.get_mut("chain_joins").unwrap()[0]["score"] = json!(24.28);
        let acceptable = context
            .evaluate(&plan, true)
            .expect("quality shortfall does not invalidate a full duration set");
        assert!(
            acceptable["objective"].as_f64().unwrap() < preferred["objective"].as_f64().unwrap()
        );
        assert!((acceptable["transition_shortfall"].as_f64().unwrap() - 0.72).abs() < 1e-9);
        assert!(acceptable["transition_fit_penalty"].as_f64().unwrap() < 0.02);
    }
    #[cfg(feature = "cp-search")]
    #[test]
    fn development_propagation_keeps_slow_bridges_and_audible_overlap_credit() {
        let (context, _) = long_plan(3);
        let arrive = DevelopmentPort {
            frame: 100_000,
            source_rate: 1_000,
            output_rate: 48_000,
            speed: 1.,
            credit: 0.,
        };
        let leave = DevelopmentPort {
            frame: 150_000,
            speed: 0.9,
            ..arrive
        };
        assert!(context.rules_out_development(1, Some(arrive), Some(leave)));
        assert!(!context.rules_out_development(
            1,
            Some(DevelopmentPort {
                credit: 5.,
                ..arrive
            }),
            Some(leave),
        ));
        assert!(!context.rules_out_development(
            1,
            Some(arrive),
            Some(DevelopmentPort {
                frame: 154_000,
                ..leave
            }),
        ));
        assert!(!context.rules_out_development(1, None, Some(leave)));
    }

    #[cfg(feature = "cp-search")]
    #[test]
    fn closing_bounds_cover_signed_scores_and_required_replay_time() {
        let (context, mut plan) = long_plan(20);
        let rate = u64::from(plan.output.sample_rate_hz);
        plan.header.extensions.insert(
            "chain_joins".into(),
            json!(vec![
                json!({"score": 5., "pacing": {"penalty_quality_seconds": 3.}});
                19
            ]),
        );
        let broad = context.closing_score_upper(&plan, false).unwrap();
        let fixed = context.closing_score_upper(&plan, true).unwrap();
        assert!(fixed < broad && broad < -6.);
        for length in [10, 20, 60] {
            let replay = PassageReplay {
                placement: 0,
                capture_start: 20 * rate,
                capture_end: (20 + length) * rate,
                insert_before: 90 * rate,
                repetitions: 1,
                bars: 8,
                declick_frames: rate / 200,
                evidence: json!({}),
            };
            let arrangement = SetArrangement {
                version: 1,
                base_output_frames: plan.total_output_frames,
                replays: vec![replay],
            };
            let mut candidate = plan.clone();
            candidate.total_output_frames += arrangement.added_frames();
            candidate
                .header
                .extensions
                .insert(ARRANGEMENT_EXTENSION.into(), json!(arrangement));
            let actual = context.evaluate(&candidate, true).unwrap();
            assert!(actual["objective"].as_f64().unwrap() <= fixed);
        }
        // A stronger measured join can put the numerator on the positive
        // side, where the shortest permitted duration supplies the ceiling.
        for row in plan
            .header
            .extensions
            .get_mut("chain_joins")
            .unwrap()
            .as_array_mut()
            .unwrap()
        {
            row["dense_quality_seconds"] = json!(40.);
        }
        assert!(context.closing_score_upper(&plan, true).unwrap() > 0.);
    }

    #[cfg(feature = "cp-search")]
    #[test]
    fn fixed_opening_bound_covers_every_mock_ending_and_replay() {
        let (mut context, mut plan) = long_plan(20);
        let rate = u64::from(plan.output.sample_rate_hz);
        let last_join_end = plan.transitions.last().unwrap().end_output_frame;
        context.profiles.last_mut().unwrap().duration = 195.;
        plan.placements.last_mut().unwrap().time_map.anchors.insert(
            1,
            Anchor {
                source_frame: 10 * rate,
                output_frame: last_join_end,
            },
        );
        plan.header
            .extensions
            .insert("selected_opening_source_frame".into(), json!(0));

        for fit in [5., 40.] {
            plan.header.extensions.insert(
                "chain_joins".into(),
                json!(vec![json!({"score": fit, "dense_quality_seconds": 6.}); 19]),
            );
            for shape in [Shape::Story, Shape::Wave, Shape::Ramp, Shape::Chapters] {
                context.request.shape = shape;
                let bound = context
                    .fixed_opening_score_bound(&plan, context.latest_ending_output_frame(&plan))
                    .unwrap();
                assert!(bound.upper < bound.without_passage_penalties - 0.5);
                let mut valid = 0;
                for solo in [150, 170, 185] {
                    for repeat in [0, 40, 60] {
                        let mut candidate = plan.clone();
                        let base = last_join_end + solo * rate;
                        let last = candidate.placements.last_mut().unwrap();
                        last.source_end_frame = (10 + solo) * rate;
                        *last.time_map.anchors.last_mut().unwrap() = Anchor {
                            source_frame: last.source_end_frame,
                            output_frame: base,
                        };
                        candidate.total_output_frames = base + repeat * rate;
                        if repeat > 0 {
                            let arrangement = SetArrangement {
                                version: 1,
                                base_output_frames: base,
                                replays: vec![PassageReplay {
                                    placement: 0,
                                    capture_start: 20 * rate,
                                    capture_end: (20 + repeat) * rate,
                                    insert_before: 90 * rate,
                                    repetitions: 1,
                                    bars: 8,
                                    declick_frames: rate / 200,
                                    evidence: json!({}),
                                }],
                            };
                            candidate
                                .header
                                .extensions
                                .insert(ARRANGEMENT_EXTENSION.into(), json!(arrangement));
                        }
                        if let Some(actual) = context.evaluate(&candidate, true) {
                            valid += 1;
                            assert!(actual["objective"].as_f64().unwrap() <= bound.upper);
                        }
                    }
                }
                assert_eq!(valid, 7);
            }
        }
        plan.header
            .extensions
            .remove("selected_opening_source_frame");
        let bound = context
            .fixed_opening_score_bound(&plan, context.latest_ending_output_frame(&plan))
            .unwrap();
        assert_eq!(bound.upper, bound.without_passage_penalties);
    }

    #[cfg(feature = "cp-search")]
    #[test]
    fn fixed_ending_bounds_arc_shifts_and_keeps_original_development_cost() {
        let (mut context, mut plan) = long_plan(20);
        let rate = u64::from(plan.output.sample_rate_hz);
        for (i, profile) in context.profiles.iter_mut().enumerate() {
            if i % 3 == 0 {
                continue;
            }
            let tile = |start, end, intensity| {
                serde_json::from_value::<SegmentTile>(json!({
                    "start_frame": start * rate, "end_frame": end * rate,
                    "bars": 1, "pulses_per_bar": 4, "pulse_score_min": 1.,
                    "local_bpm": 120., "rms": 0.1, "peak": 0.3, "crest_factor_db": 3.,
                    "band_occupancy": [0.3, 0.3, 0.3], "onset_rate_hz": 2., "chroma": vec![0.; 12],
                    "level_variation": 0., "harmonic_variation": 0.,
                    "roles": {"set_intensity_percentile": intensity, "sustained_texture": 1.},
                    "vocal_activity": null, "separation_quality": null,
                }))
                .unwrap()
            };
            profile.tiles = vec![tile(0_u64, 90_u64, 0.1), tile(90, 180, 0.95)];
            profile.duration = 600.;
        }
        for fit in [5., 40.] {
            plan.header.extensions.insert(
                "chain_joins".into(),
                json!(vec![
                    json!({"score": fit, "dense_quality_seconds": 20.});
                    19
                ]),
            );
            for shape in [Shape::Story, Shape::Wave, Shape::Ramp, Shape::Chapters] {
                context.request.shape = shape;
                let envelope = context.closing_score_envelope(&plan, true);
                let bound = context.replay_score_bound(&plan).unwrap();
                let (arc, dwell) = context
                    .replay_passage_penalty_bounds(&plan, envelope.max_duration_seconds)
                    .unwrap();
                assert!(arc > 0.1 && dwell > 0.1);
                assert!(bound.upper < bound.without_passage_penalties - 1.);
                for lengths in [[10, 10], [15, 40], [40, 25]] {
                    let arrangement = SetArrangement {
                        version: 1,
                        base_output_frames: plan.total_output_frames,
                        replays: [0, 13]
                            .into_iter()
                            .zip(lengths)
                            .map(|(placement, length)| {
                                let offset = if placement == 0 {
                                    0
                                } else {
                                    plan.transitions[placement - 1].end_output_frame
                                };
                                PassageReplay {
                                    placement,
                                    capture_start: offset + 20 * rate,
                                    capture_end: offset + (20 + length) * rate,
                                    insert_before: offset + 90 * rate,
                                    repetitions: 1,
                                    bars: 8,
                                    declick_frames: rate / 200,
                                    evidence: json!({}),
                                }
                            })
                            .collect(),
                    };
                    let mut candidate = plan.clone();
                    candidate.total_output_frames += arrangement.added_frames();
                    candidate
                        .header
                        .extensions
                        .insert(ARRANGEMENT_EXTENSION.into(), json!(arrangement));
                    let actual = context.evaluate(&candidate, true).unwrap();
                    assert_eq!(actual["dwell_penalty"].as_f64().unwrap(), dwell);
                    assert!(actual["arc_error"].as_f64().unwrap() >= arc);
                    assert!(actual["objective"].as_f64().unwrap() <= bound.upper);
                }
            }
        }
    }

    #[test]
    fn full_sets_have_variable_counts_and_use_overlap_adjusted_output_duration() {
        for count in [21, 42] {
            let (context, mut plan) = long_plan(count);
            let evidence = context.evaluate(&plan, true).unwrap();
            assert_eq!(evidence["passages"].as_array().unwrap().len(), count);
            assert_eq!(
                evidence["duration_seconds"].as_f64().unwrap(),
                (count * 170 + 10) as f64
            );
            plan.total_output_frames =
                (context.request.max() as u64 + 1) * u64::from(plan.output.sample_rate_hz);
            assert!(
                context.evaluate(&plan, true).is_none(),
                "an extended closing passage cannot exceed the clock"
            );
        }
        let (context, plan) = long_plan(20);
        assert!(
            context.evaluate(&plan, true).is_none(),
            "source lengths sum to 60 minutes, but overlaps make this undersized"
        );
    }
    #[test]
    fn closing_diagnostics_distinguish_clock_and_development_rejections() {
        let (context, mut plan) = long_plan(20);
        let mut rejection = None;
        assert!(context
            .evaluate_diagnosed(&plan, true, &mut rejection)
            .is_none());
        assert_eq!(rejection, Some("duration_outside_window"));
        // Reaching the requested clock cannot legitimize an undeveloped song.
        plan.total_output_frames = 57 * 60 * u64::from(plan.output.sample_rate_hz);
        plan.transitions[0].start_output_frame = 40 * u64::from(plan.output.sample_rate_hz);
        rejection = None;
        assert!(context
            .evaluate_diagnosed(&plan, true, &mut rejection)
            .is_none());
        assert_eq!(rejection, Some("insufficient_development"));
        assert!(context.evaluate(&plan, true).is_none());
    }

    #[test]
    fn replay_base_checks_original_development_even_below_the_duration_floor() {
        let (context, mut plan) = long_plan(20);
        assert_eq!(
            context.evaluate_for_save(&plan, true).unwrap_err().reason,
            "duration_outside_window"
        );
        let rate = u64::from(plan.output.sample_rate_hz);
        plan.transitions[0].start_output_frame = 40 * rate;
        assert_eq!(
            context.evaluate_for_save(&plan, false).unwrap_err().reason,
            "duration_outside_window"
        );
        let rejection = context.evaluate_for_save(&plan, true).unwrap_err();
        assert_eq!(rejection.reason, "insufficient_development");
        let shortfall = rejection.development.unwrap();
        assert_eq!(shortfall.track_id, plan.placements[0].track_id);
        assert_eq!(shortfall.position, 1);
        assert_eq!(shortfall.solo_seconds, 40.);
        assert!(
            shortfall.solo_seconds + shortfall.overlap_credit_seconds + DEVELOPMENT_GRACE_SECONDS
                < shortfall.minimum_seconds
        );
        let solo = plan.transitions[0].end_output_frame;
        let arrangement = SetArrangement {
            version: 1,
            base_output_frames: plan.total_output_frames,
            replays: vec![PassageReplay {
                placement: 1,
                capture_start: solo + 20 * rate,
                capture_end: solo + 40 * rate,
                insert_before: solo + 60 * rate,
                repetitions: 1,
                bars: 8,
                declick_frames: rate / 200,
                evidence: json!({}),
            }],
        };
        plan.total_output_frames += arrangement.added_frames();
        plan.header
            .extensions
            .insert(ARRANGEMENT_EXTENSION.into(), json!(arrangement));
        let replay = context.evaluate_for_save(&plan, false).unwrap_err();
        assert_eq!(replay.reason, "insufficient_development");
        assert_eq!(json!(replay.development.unwrap()), json!(shortfall));
    }

    #[test]
    fn exhaustive_mock_endings_separate_replay_clock_from_closing_track_development() {
        let (context, mut plan) = long_plan(20);
        let rate = u64::from(plan.output.sample_rate_hz);
        let solo = plan.transitions[0].end_output_frame;
        let p = &mut plan.placements[1];
        let mut flat = p.automation[0].clone();
        flat.output_frame = solo;
        flat.gain_linear = 1.;
        flat.mute = false;
        p.automation = vec![flat.clone(), flat];
        p.automation[1].output_frame = solo + 60 * rate;
        let last_join = plan.transitions.last().unwrap();
        // These are two possible endings after the same admitted route.
        // The early ending leaves 42.6s of solo, below the 60s minimum.
        // The later ending gives 170s of solo but needs 10s of replay to fit.
        let expected = [
            ["insufficient_development", "insufficient_development"],
            ["duration_outside_window", "accepted"],
        ];
        for (i, solo_frames) in [426 * rate / 10, 170 * rate].into_iter().enumerate() {
            let mut base = plan.clone();
            base.total_output_frames = last_join.end_output_frame + solo_frames;
            let last = base.placements.last_mut().unwrap();
            last.source_end_frame = base.total_output_frames - last_join.start_output_frame;
            *last.time_map.anchors.last_mut().unwrap() = Anchor {
                source_frame: last.source_end_frame,
                output_frame: base.total_output_frames,
            };
            let early = context.evaluate_for_save(&base, true).unwrap_err();
            if i == 0 {
                let shortfall = early.development.as_ref().unwrap();
                assert_eq!(shortfall.position, 20);
                assert_eq!(shortfall.solo_seconds, 42.6);
            }
            assert_eq!(early.reason, expected[i][0]);

            for (j, replay_seconds) in [0, 144].into_iter().enumerate() {
                let mut candidate = base.clone();
                if replay_seconds > 0 {
                    let arrangement = SetArrangement {
                        version: 1,
                        base_output_frames: base.total_output_frames,
                        replays: vec![PassageReplay {
                            placement: 1,
                            capture_start: solo + 2 * rate,
                            capture_end: solo + 50 * rate,
                            insert_before: solo + 52 * rate,
                            repetitions: 3,
                            bars: 16,
                            declick_frames: rate / 200,
                            evidence: json!({}),
                        }],
                    };
                    assert_eq!(arrangement.added_frames(), replay_seconds * rate);
                    candidate.total_output_frames += arrangement.added_frames();
                    candidate.renderer_version = ARRANGEMENT_RENDERER.into();
                    arrangement.validate_for(&candidate).unwrap();
                    candidate
                        .header
                        .extensions
                        .insert(ARRANGEMENT_EXTENSION.into(), json!(arrangement));
                    assert!(context
                        .request
                        .accepts(candidate.total_output_frames as f64 / rate as f64));
                }
                let outcome = context.evaluate_for_save(&candidate, true);
                let status = outcome
                    .as_ref()
                    .map_or_else(|rejection| rejection.reason, |_| "accepted");
                assert_eq!(status, expected[i][j]);
                if let Some(detail) = outcome.err().and_then(|r| r.development) {
                    assert_eq!(json!(detail), json!(early.development.as_ref().unwrap()));
                }
            }
        }
    }
    #[test]
    fn replay_shortfall_keeps_later_measured_endings_inside_the_original_window() {
        let mut window = mixlib_planner::ChainClosingWindow {
            earliest: 100,
            latest: 200,
        };
        // Only these endpoints passed the musical-ending gate. The first two
        // cannot reach the target using the route's measured replay material.
        let measured_endings = [120, 150, 190, 240];
        let mut heard = vec![];
        for attempt in 0..MAX_CLOSING_ENDINGS {
            let end = measured_endings
                .into_iter()
                .find(|end| *end >= window.earliest && *end <= window.latest)
                .unwrap();
            heard.push(end);
            if end >= 180 {
                break;
            }
            window = later_closing_window(window, end, attempt).unwrap();
            assert_eq!(window.latest, 200);
        }
        assert_eq!(heard, [120, 150, 190]);
        assert!(later_closing_window(window, 200, 0).is_none());
        assert!(later_closing_window(window, window.earliest - 1, 0).is_none());
    }
    #[test]
    fn ending_alternatives_stop_after_eight_distinct_candidates() {
        let mut window = mixlib_planner::ChainClosingWindow {
            earliest: 100,
            latest: 1000,
        };
        let mut attempts = 0;
        loop {
            let end = window.earliest;
            let next = later_closing_window(window, end, attempts);
            attempts += 1;
            let Some(next) = next else { break };
            assert!(next.earliest > end);
            window = next;
        }
        assert_eq!(attempts, 8);
    }
    #[test]
    fn repeats_fill_clock_but_do_not_earn_development_credit() {
        let (context, mut plan) = long_plan(20);
        let rate = u64::from(plan.output.sample_rate_hz);
        assert!(context.evaluate(&plan, true).is_none());
        let replay = PassageReplay {
            placement: 0,
            capture_start: 30 * rate,
            capture_end: 50 * rate,
            insert_before: 50 * rate,
            repetitions: 1,
            bars: 8,
            declick_frames: rate / 200,
            evidence: json!({}),
        };
        let arrangement = SetArrangement {
            version: 1,
            base_output_frames: plan.total_output_frames,
            replays: vec![replay],
        };
        plan.total_output_frames += arrangement.added_frames();
        plan.header.extensions.insert(
            ARRANGEMENT_EXTENSION.into(),
            serde_json::to_value(arrangement).unwrap(),
        );
        let evidence = context.evaluate(&plan, true).unwrap();
        assert_eq!(evidence["duration_seconds"], 3430.);
        assert_eq!(evidence["passages"][0]["solo_seconds"], 170.);
        assert_eq!(evidence["passages"][0]["development_seconds"], 170.);
        assert_eq!(evidence["passages"][0]["replay_seconds"], 20.);
        // Repeats cannot rescue a route that cuts a song off before its minimum.
        plan.transitions[0].start_output_frame = 40 * rate;
        assert!(context.evaluate(&plan, true).is_none());
    }
    #[test]
    fn closing_window_extends_past_demo_tail_and_respects_actual_clock() {
        let (context, mut plan) = long_plan(21);
        let rate = u64::from(plan.output.sample_rate_hz);
        // The last pair demo only retained 20 seconds after the incoming join.
        let last = plan.placements.last_mut().unwrap();
        last.source_end_frame = 30 * rate;
        last.time_map.anchors.last_mut().unwrap().source_frame = 30 * rate;
        last.time_map.anchors.last_mut().unwrap().output_frame = 3430 * rate;
        plan.total_output_frames = 3430 * rate;
        let window = context.closing_window(&plan).unwrap();
        assert!(
            window.earliest >= 70 * rate,
            "closing must allow sixty solo seconds after the ten-second join"
        );
        assert!(window.earliest < 72 * rate);
        assert_eq!(
            window.latest,
            180 * rate,
            "the source tail precedes the 60-minute ceiling"
        );
        let (context, plan) = long_plan(20);
        assert!(
            context.closing_window(&plan).is_none(),
            "no extension can invent missing source duration"
        );
    }
    #[test]
    fn closed_passage_cache_is_reused_only_for_unchanged_timing() {
        let (context, mut plan) = long_plan(21);
        let first = context.evaluate(&plan, true).unwrap();
        plan.header
            .extensions
            .insert("set_composition".into(), first.clone());
        assert_eq!(context.evaluate(&plan, true).unwrap(), first);
        let rate = u64::from(plan.output.sample_rate_hz);
        plan.transitions[0].start_output_frame -= 10 * rate;
        let changed = context.evaluate(&plan, true).unwrap();
        assert_ne!(
            changed["passages"][0]["cache_key"],
            first["passages"][0]["cache_key"]
        );
        assert_eq!(changed["passages"][1], first["passages"][1]);
        assert_eq!(changed["passages"][0]["solo_seconds"], 160.);
    }
    #[test]
    fn strong_anchor_survives_temporary_arc_or_lead_in_penalty() {
        let (_, template) = long_plan(21);
        let mut routes: Vec<_> = (0..8).map(|i| {
            let mut p = template.clone();
            p.header.extensions.insert("set_composition".into(), json!({"objective":i as f64}));
            p.header.extensions.insert("chain_joins".into(), json!([{
                "from":"A","to":"B","outgoing_cue_frame":i,"incoming_cue_frame":12,"choice":"M01"}]));
            (i, p)
        }).collect();
        let anchor = HeldAnchor {
            from: "A".into(),
            to: "B".into(),
            exit: 0,
            entry: 12,
            tactic: json!("M01"),
            value: 20.,
        };
        prune_routes(
            &mut routes,
            4,
            |r| &r.1,
            |r| (r.0, 0, 1 << r.0),
            &[anchor],
            &Default::default(),
        );
        assert!(
            routes.iter().any(|r| r.0 == 0),
            "keep the measured defining join while its surrounding story is unfinished"
        );
        assert_eq!(routes.len(), 4);
        assert!(
            routes.iter().any(|r| r.0 == 7),
            "ordinary best routes retain most of the beam"
        );
    }
    #[test]
    fn provisional_unheard_tail_does_not_change_partial_route_rank() {
        let (context, mut plan) = long_plan(21);
        plan.header.extensions.insert("chain_joins".into(), json!([{
            "score":50.,"last_resort":false,"dense_quality_seconds":20.,"handoff_quality_seconds":0.}]));
        let before = context.evaluate(&plan, false).unwrap();
        plan.total_output_frames -= 100 * u64::from(plan.output.sample_rate_hz);
        let after = context.evaluate(&plan, false).unwrap();
        assert_eq!(before["objective"], after["objective"]);
        assert_eq!(after["ranking_horizon_seconds"], 3600.);
    }
    #[test]
    fn longer_development_is_not_outscored_just_because_a_prefix_is_shorter() {
        let (mut context, mut long) = long_plan(2);
        let rate = u64::from(long.output.sample_rate_hz);
        let tile: SegmentTile = serde_json::from_value(json!({
            "start_frame":0,"end_frame":180*rate,"bars":1,"pulses_per_bar":4,"pulse_score_min":1.,
            "local_bpm":120.,"rms":0.1,"peak":0.3,"crest_factor_db":3.,"band_occupancy":[0.3,0.3,0.3],
            "onset_rate_hz":2.,"chroma":[0.,0.,0.,0.,0.,0.,0.,0.,0.,0.,0.,0.],
            "level_variation":0.,"harmonic_variation":0.,"roles":{"set_intensity_percentile":0.3},
            "vocal_activity":null,"separation_quality":null
        })).unwrap();
        context.profiles[0].tiles = vec![tile];
        long.header.extensions.insert("chain_joins".into(), json!([{
            "score":50.,"last_resort":false,"dense_quality_seconds":20.,"handoff_quality_seconds":0.}]));
        let mut short = long.clone();
        short.transitions[0].start_output_frame = 60 * rate;
        short.transitions[0].end_output_frame = 70 * rate;
        short.placements[0].source_end_frame = 70 * rate;
        *short.placements[0].time_map.anchors.last_mut().unwrap() = Anchor {
            source_frame: 70 * rate,
            output_frame: 70 * rate,
        };
        for a in &mut short.placements[1].time_map.anchors {
            a.output_frame -= 110 * rate;
        }
        short.total_output_frames -= 110 * rate;
        assert!(
            context.evaluate(&long, false).unwrap()["objective"]
                .as_f64()
                .unwrap()
                > context.evaluate(&short, false).unwrap()["objective"]
                    .as_f64()
                    .unwrap()
        );
    }
    #[test]
    fn elapsed_diversity_keeps_a_longer_route_even_when_short_prefixes_rank_higher() {
        let (_, template) = long_plan(2);
        let rate = u64::from(template.output.sample_rate_hz);
        let mut routes: Vec<_> = (0..8)
            .map(|i| {
                let mut p = template.clone();
                p.header
                    .extensions
                    .insert("set_composition".into(), json!({"objective":i as f64}));
                p.transitions[0].end_output_frame = if i == 0 { 900 * rate } else { 100 * rate };
                (i, p)
            })
            .collect();
        prune_routes(
            &mut routes,
            4,
            |r| &r.1,
            |r| (r.0, 0, 1 << r.0),
            &[],
            &Default::default(),
        );
        assert!(
            routes.iter().any(|r| r.0 == 0),
            "the clock's furthest valid prefix must remain available"
        );
        assert!(routes.iter().any(|r| r.0 == 7));
    }
    #[test]
    fn cli_duration_modes_reject_conflicting_track_count() {
        assert!(Args::try_parse_from([
            "mixlib",
            "search-set",
            "--set-minutes",
            "120",
            "--structure",
            "chapters"
        ])
        .is_ok());
        assert!(Args::try_parse_from([
            "mixlib",
            "search-set",
            "--set-minutes",
            "60",
            "--tracks",
            "8"
        ])
        .is_err());
        assert!(Args::try_parse_from(["mixlib", "search-set", "--set-minutes", "90"]).is_err());
    }
    #[test]
    fn strong_held_music_counts_toward_development_but_incoming_drums_do_not() {
        let mut t = TacticPlan {
            policy_version: TACTICS_POLICY_VERSION.into(),
            tactic_id: TacticId::M01,
            modifiers: vec![],
            arrangement: TacticArrangement::Mashup,
            character: TacticCharacter::Subtle,
            permission: TacticId::M01.permission(),
            outgoing_placement: 0,
            incoming_placement: 1,
            start_frame: 0,
            hold_end_frame: 60,
            exit_start_frame: 60,
            end_frame: 70,
            phrase_exit: None,
            bar_frames: vec![0, 15, 30, 45, 60, 70],
            timing_basis: TacticTimingBasis::Bars,
            bar_bpm: None,
            pulse_ratio: None,
            lanes: vec![],
            stem_assets: vec![],
            effects: vec![],
            source_edits: vec![],
            edit_speed_ratio_limits: [0.94, 1.06],
            vocal_occupancy: vec![],
            vocal_guard_frames: 0,
            evidence: vec!["measured hold fixture".into()],
            selection_reason: "development accounting fixture".into(),
            emergency: None,
        };
        let solo = 40.;
        let minimum = 60.;
        assert!(
            solo + overlap_credits(&[t.clone()], 2, 1.)[1] >= minimum,
            "a long admitted dense mashup must not look like only forty seconds of music"
        );
        t.tactic_id = TacticId::S06;
        let credit = overlap_credits(&[t], 2, 1.);
        assert!(
            solo + credit[0] >= minimum,
            "outgoing musical identity continues through the handoff"
        );
        assert!(
            solo + credit[1] < minimum,
            "incoming drums alone must not satisfy development"
        );
    }
    #[test]
    fn duration_bound_excludes_disconnected_songs_and_respects_entry_runway() {
        let pacing = vec![TrackPacing::unknown(1); 4];
        let reach = DurationReach {
            edges: vec![
                vec![DurationEdge {
                    to: 1,
                    exit: 70,
                    entry: 20,
                }],
                vec![DurationEdge {
                    to: 2,
                    exit: 70,
                    entry: 20,
                }],
                vec![],
                vec![],
            ],
            frames: vec![120, 120, 120, 9999],
            rates: vec![1.; 4],
            pacing: &pacing,
            minimum_speed: 1.,
        };
        assert_eq!(reach.remaining_seconds(1, 0, 20), 340.);
        assert_eq!(
            reach.remaining_seconds(1, 0, 80),
            40.,
            "a late cue cannot reach the only onward join"
        );
        assert_eq!(
            reach.remaining_seconds(3, 0, 20),
            100.,
            "a previously used song is not a duration bridge"
        );
    }
    #[test]
    fn only_requested_duration_windows_are_complete() {
        for minutes in [60, 120] {
            let r = Request::new(minutes, "auto").unwrap();
            assert!(r.accepts(r.min()));
            assert!(r.accepts(r.max()));
            assert!(!r.accepts(r.min() - 0.01));
            assert!(!r.accepts(r.max() + 0.01));
        }
        assert!(Request::new(90, "story").is_err());
        assert!(!Request::new(120, "auto")
            .unwrap()
            .capacity_possible(3600., 0.94));
        assert!(Request::new(60, "auto")
            .unwrap()
            .capacity_possible(3600., 0.94));
    }
    #[test]
    fn narrative_shapes_have_contrast_and_resolution() {
        for shape in [Shape::Story, Shape::Wave, Shape::Chapters] {
            assert!(shape.target(0.82) > shape.target(0.));
            assert!(shape.target(1.) < shape.target(0.82));
        }
        assert!(Shape::Ramp.target(0.8) > Shape::Ramp.target(0.2));
        assert_eq!(Request::new(120, "auto").unwrap().shape, Shape::Chapters);
    }
    #[test]
    fn later_development_is_weighted_without_requiring_a_whole_song() {
        let t = SegmentTile {
            start_frame: 0,
            end_frame: 10000,
            bars: 1,
            pulses_per_bar: 4,
            pulse_score_min: 1.,
            local_bpm: 120.,
            rms: 0.2,
            peak: 0.5,
            crest_factor_db: 3.,
            band_occupancy: [0.5; 3],
            onset_rate_hz: 2.,
            chroma: [0.; 12],
            level_variation: 0.,
            harmonic_variation: 0.,
            roles: BTreeMap::from([
                ("set_intensity_percentile".into(), 0.5),
                ("sustained_texture".into(), 1.),
            ]),
            vocal_activity: None,
            separation_quality: None,
        };
        let c = Context {
            replays: Default::default(),
            request: Request::new(60, "story").unwrap(),
            profiles: vec![Profile {
                tiles: vec![t],
                duration: 400.,
                rate: 100.,
                opening: Default::default(),
                #[cfg(feature = "cp-search")]
                opening_accents: vec![],
            }],
            ids: BTreeMap::new(),
        };
        let p = c.passage(0, &passage_map(0, 10000, 100), 0, 10000);
        assert_eq!(p.minimum, 90.);
        assert_eq!(p.preferred, 270.);
        let mut c = c;
        c.profiles[0].tiles[0]
            .roles
            .insert("set_intensity_percentile".into(), 0.95);
        let mut later = c.profiles[0].tiles[0].clone();
        later.start_frame = 20000;
        later.end_frame = 30000;
        later.roles.insert("set_intensity_percentile".into(), 0.3);
        c.profiles[0].tiles.push(later);
        assert!(c.passage(0,&passage_map(20000,30000,100),0,10000).error < c.passage(0,&passage_map(0,10000,100),0,10000).error,
            "the same song's later passage can be a better opening; whole-track energy cannot decide this");
        c.profiles[0].tiles[0].start_frame = 0;
        c.profiles[0].tiles[0].end_frame = 100;
        c.profiles[0].tiles[0]
            .roles
            .insert("set_intensity_percentile".into(), 0.9);
        c.profiles[0].tiles[1].start_frame = 100;
        c.profiles[0].tiles[1].end_frame = 200;
        c.profiles[0].tiles[1]
            .roles
            .insert("set_intensity_percentile".into(), 0.1);
        let mut map = passage_map(0, 200, 3);
        map.anchors.insert(
            1,
            Anchor {
                source_frame: 100,
                output_frame: 100,
            },
        );
        let heard = c.passage(0, &map, 0, 300);
        assert!(
            (heard.energy - (0.9 + 0.1 + 0.1) / 3.).abs() < 1e-9,
            "slower bars must have more listening-time weight than faster bars"
        );
        assert_eq!(heard.coverage, 1.);
    }
}
