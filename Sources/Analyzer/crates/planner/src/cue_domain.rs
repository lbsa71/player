//! Finite, paged domain of pinned bar cue windows.
//! The ordinal space includes timing failures. Visiting a cell accounts for
//! exactly one template and one pair of source starts.

use super::*;

pub const BAR_CUE_PROFILE: &str = "pinned-bar-windows/1";
pub const STRUCTURAL_CUE_PROFILE: &str = "pinned-structural-windows/2";

fn domain_id(
    tracks: &[Analyzed; 2],
    profile: &str,
    context: usize,
    policy: &MixingConfig,
) -> Result<String> {
    let pins = tracks.each_ref().map(|a| {
        serde_json::json!({
            "track_id": a.track.track_id,
            "audio_hash": a.track.audio_hash,
            "audio_revision": a.track.audio_revision,
            "analysis": a.track.active_analysis,
            "overrides": a.track.active_overrides,
            "override_revision": a.track.override_revision,
        })
    });
    Ok(canonical_hash(&serde_json::json!({
        "profile": profile, "pins": pins, "context_beats": context,
        "timing": {
            "output_sample_rate_hz": policy.output_sample_rate_hz,
            "max_speed_deviation": policy.max_speed_deviation,
            "max_speed_change_per_second": policy.max_speed_change_per_second,
        },
    }))?
    .digest)
}

fn excluded([a, b]: [&Analyzed; 2]) -> bool {
    a.track.track_id == b.track.track_id
        || a.track.audio_hash == b.track.audio_hash
        || a.overrides.operations.iter().any(|op| matches!(
            op, Correction::ExcludeTransition { to_track_id } if to_track_id == &b.track.track_id
        ))
}

struct TemplateBlock {
    overlap: usize,
    incoming_overlap: usize,
    end: u64,
    timing_impossible: bool,
}

/// A range is scoped to a pinned pair, context length and timing policy.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BarCueRange {
    pub domain_id: String,
    pub start: u64,
    pub end: u64,
}

impl BarCueRange {
    pub fn split(self) -> Option<(Self, Self)> {
        if self.end.checked_sub(self.start)? < 2 {
            return None;
        }
        let mid = self.start + (self.end - self.start) / 2;
        Some((
            Self {
                end: mid,
                ..self.clone()
            },
            Self { start: mid, ..self },
        ))
    }
}

pub struct BarCuePage {
    /// Half-open range visited by this call, including timing failures.
    pub scanned: BarCueRange,
    pub remainder: BarCueRange,
    pub plans: Vec<MixPlan>,
    pub block_pruned_cells: u64,
    /// Ordered disjoint partition of scanned, with one cell per emitted plan.
    pub outcomes: Vec<BarCueSpan>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarCueOutcome {
    BlockTimingRejected,
    RowTimingRejected,
    CellTimingRejected,
    PinnedMapRejected,
    TimingWindow,
}

impl BarCueOutcome {
    pub fn label(self) -> &'static str {
        match self {
            Self::BlockTimingRejected => "block_timing_rejected",
            Self::RowTimingRejected => "row_timing_rejected",
            Self::CellTimingRejected => "cell_timing_rejected",
            Self::PinnedMapRejected => "pinned_map_rejected",
            Self::TimingWindow => "timing_window",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BarCueSpan {
    pub start: u64,
    pub end: u64,
    pub outcome: BarCueOutcome,
}

fn record_span(outcomes: &mut Vec<BarCueSpan>, start: u64, end: u64, outcome: BarCueOutcome) {
    if outcome != BarCueOutcome::TimingWindow {
        if let Some(previous) = outcomes
            .last_mut()
            .filter(|span| span.outcome == outcome && span.end == start)
        {
            previous.end = end;
            return;
        }
    }
    outcomes.push(BarCueSpan {
        start,
        end,
        outcome,
    });
}

pub struct BarCueDomain {
    id: String,
    tracks: [Analyzed; 2],
    context_beats: usize,
    policy: MixingConfig,
    outgoing: BTreeMap<usize, Vec<Passage>>,
    incoming: BTreeMap<usize, IncomingPassages>,
    templates: Vec<TemplateBlock>,
}

fn lengths(track: &Analyzed) -> BTreeSet<usize> {
    let ids: BTreeSet<_> = track.beats.downbeat_ids.iter().collect();
    let indices: Vec<_> = track
        .beats
        .events
        .iter()
        .enumerate()
        .filter_map(|(i, b)| ids.contains(&b.event_id).then_some(i))
        .collect();
    indices
        .windows(2)
        .map(|w| w[1] - w[0])
        .filter(|m| (2..=12).contains(m))
        .flat_map(|meter| (1..=16).map(move |bars| meter * bars))
        .collect()
}

#[derive(Clone, Copy)]
struct TimingBounds {
    min: f64,
    max: f64,
    meters: u128,
}

impl TimingBounds {
    const EMPTY: Self = Self {
        min: f64::INFINITY,
        max: 0.,
        meters: 0,
    };

    fn passage(p: &Passage) -> Self {
        Self {
            min: p.seconds,
            max: p.seconds,
            // Keep an unknown or unrepresentable meter compatible with all meters.
            meters: 1_u128.checked_shl(p.bar_beats as u32).unwrap_or(u128::MAX),
        }
    }

    fn union(self, other: Self) -> Self {
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
            meters: self.meters | other.meters,
        }
    }
}

struct IncomingPassages {
    passages: Vec<Passage>,
    // A binary tree of contiguous intervals in the original ordinal order.
    bounds: Vec<TimingBounds>,
    leaves: usize,
}

impl IncomingPassages {
    fn new(passages: Vec<Passage>) -> Self {
        let leaves = passages.len().next_power_of_two();
        let mut bounds = vec![TimingBounds::EMPTY; 2 * leaves];
        for (slot, passage) in bounds[leaves..].iter_mut().zip(&passages) {
            *slot = TimingBounds::passage(passage);
        }
        for node in (1..leaves).rev() {
            bounds[node] = bounds[2 * node].union(bounds[2 * node + 1]);
        }
        Self {
            passages,
            bounds,
            leaves,
        }
    }

    fn rejected_end(
        &self,
        start: usize,
        outgoing: TimingBounds,
        mapped: bool,
        speed_limit: f64,
    ) -> Option<usize> {
        let (mut node, mut left, mut right) = (1, 0, self.leaves);
        loop {
            if impossible_block(outgoing, self.bounds[node], mapped, speed_limit) {
                return Some(right.min(self.passages.len()));
            }
            if node >= self.leaves {
                return None;
            }
            let middle = left + (right - left) / 2;
            node *= 2;
            if start < middle {
                right = middle;
            } else {
                node += 1;
                left = middle;
            }
        }
    }
}

/// Reject an interval only when its most permissive ratio and meter bounds
/// fail. The duration and ratio operations match mapped_pair_timing.
fn impossible_block(a: TimingBounds, b: TimingBounds, mapped: bool, speed_limit: f64) -> bool {
    let duration_min = (a.min + b.min) * 0.5;
    let duration_max = (a.max + b.max) * 0.5;
    let outside = |lo, hi| {
        let upper: f64 = hi / duration_min;
        let lower: f64 = lo / duration_max;
        (upper < 1. && (upper - 1.).abs() > speed_limit)
            || (lower > 1. && (lower - 1.).abs() > speed_limit)
    };
    a.meters & b.meters == 0
        || duration_min > MAX_TACTIC_CONTINUATION_SECONDS
        || outside(a.min, a.max)
        || (mapped && outside(b.min, b.max))
}

impl BarCueDomain {
    /// Enumerate 1–16 reliable outgoing bars and the 1:1, 2:1 and 1:2 pulse
    /// correspondences. Prioritize longer holds and later exits within each
    /// template. Every raw start pair remains reachable through the cursor.
    pub fn open(
        store: &Store,
        from: &str,
        to: &str,
        context_beats: usize,
        policy: &MixingConfig,
        cancel: &Cancellation,
    ) -> Result<Self> {
        cancel.check()?;
        policy.validate()?;
        require(
            (1..=64).contains(&context_beats),
            "STITCH_BEATS",
            "invalid bar cue context length",
        )?;
        let tracks = [
            load_analyzed(store, store.track(from)?)?,
            load_analyzed(store, store.track(to)?)?,
        ];
        let id = domain_id(&tracks, BAR_CUE_PROFILE, context_beats, policy)?;
        let mut domain = Self {
            id,
            tracks,
            context_beats,
            policy: policy.clone(),
            outgoing: BTreeMap::new(),
            incoming: BTreeMap::new(),
            templates: Vec::new(),
        };
        let [a, b] = domain.tracks.each_ref();
        if excluded([a, b]) {
            return Ok(domain);
        }
        let mut end = 0u64;
        for overlap in lengths(a).into_iter().rev() {
            let mut exits =
                passages_for_clock(a, overlap, context_beats, true, false, false, cancel)?;
            exits.reverse();
            if exits.is_empty() {
                continue;
            }
            let exit_bounds = exits
                .iter()
                .map(TimingBounds::passage)
                .fold(TimingBounds::EMPTY, TimingBounds::union);
            for incoming_overlap in [overlap, overlap * 2, overlap / 2] {
                if !(2..=256).contains(&incoming_overlap) {
                    continue;
                }
                if let std::collections::btree_map::Entry::Vacant(slot) =
                    domain.incoming.entry(incoming_overlap)
                {
                    slot.insert(IncomingPassages::new(passages_for_clock(
                        b,
                        incoming_overlap,
                        context_beats,
                        false,
                        false,
                        false,
                        cancel,
                    )?));
                }
                let entries = &domain.incoming[&incoming_overlap];
                if entries.passages.is_empty() {
                    continue;
                }
                end = (exits.len() as u64)
                    .checked_mul(entries.passages.len() as u64)
                    .and_then(|cells| end.checked_add(cells))
                    .ok_or_else(|| {
                        Error::new("CUE_DOMAIN", "bar cue domain exceeds ordinal capacity")
                    })?;
                domain.templates.push(TemplateBlock {
                    overlap,
                    incoming_overlap,
                    end,
                    timing_impossible: impossible_block(
                        exit_bounds,
                        entries.bounds[1],
                        incoming_overlap != overlap,
                        domain.policy.max_speed_deviation,
                    ),
                });
            }
            domain.outgoing.insert(overlap, exits);
        }
        Ok(domain)
    }

    pub fn range(&self) -> BarCueRange {
        BarCueRange {
            domain_id: self.id.clone(),
            start: 0,
            end: self.templates.last().map_or(0, |b| b.end),
        }
    }

    pub fn page(
        &self,
        store: &Store,
        range: &BarCueRange,
        scan_limit: usize,
        plan_limit: usize,
        cancel: &Cancellation,
    ) -> Result<BarCuePage> {
        require(
            range.domain_id == self.id
                && range.start <= range.end
                && range.end <= self.range().end
                && scan_limit > 0
                && plan_limit > 0,
            "CUE_DOMAIN",
            "invalid bar cue range or page limits",
        )?;
        let mut plans = Vec::new();
        let mut outcomes = Vec::new();
        let mut block_pruned_cells = 0;
        let mut next = range.start;
        let mut checked = 0;
        while next < range.end && checked < scan_limit && plans.len() < plan_limit {
            cancel.check()?;
            let index = self.templates.partition_point(|b| b.end <= next);
            let template = &self.templates[index];
            if template.timing_impossible {
                let end = range.end.min(template.end);
                record_span(&mut outcomes, next, end, BarCueOutcome::BlockTimingRejected);
                block_pruned_cells += end - next;
                next = end;
                continue;
            }
            let first = index.checked_sub(1).map_or(0, |i| self.templates[i].end);
            let entries = &self.incoming[&template.incoming_overlap];
            let cell = next - first;
            let count = entries.passages.len() as u64;
            let a = &self.outgoing[&template.overlap][(cell / count) as usize];
            checked += 1;
            let column = (cell % count) as usize;
            if let Some(end) = entries.rejected_end(
                column,
                TimingBounds::passage(a),
                template.incoming_overlap != template.overlap,
                self.policy.max_speed_deviation,
            ) {
                let end = range.end.min(next + (end - column) as u64);
                record_span(&mut outcomes, next, end, BarCueOutcome::RowTimingRejected);
                block_pruned_cells += end - next;
                next = end;
                continue;
            }
            let b = &entries.passages[column];
            next += 1;

            if mapped_pair_timing(a, b, &self.policy)
                .is_none_or(|timing| timing.0 > MAX_TACTIC_CONTINUATION_SECONDS)
                || template.overlap / a.bar_beats > 16
            {
                record_span(
                    &mut outcomes,
                    next - 1,
                    next,
                    BarCueOutcome::CellTimingRejected,
                );
                continue;
            }
            let request = StitchRequest {
                from: &self.tracks[0].track.track_id,
                to: &self.tracks[1].track.track_id,
                overlap_beats: template.overlap,
                context_beats: self.context_beats,
                outgoing_after_frame: 0,
            };
            let plan = build_stitch_from_tracks(
                store,
                self.tracks.each_ref(),
                &request,
                &self.policy,
                None,
                None,
                StitchStyle::PinnedTactic {
                    outgoing: a.start,
                    incoming: b.start,
                    incoming_overlap: template.incoming_overlap,
                },
                cancel,
            );
            let mut plan = match plan {
                Ok((plan, _)) => plan,
                Err(e)
                    if matches!(
                        e.code.as_str(),
                        "STITCH_INFEASIBLE" | "TIME_MAP_SPEED" | "TIME_MAP_ACCELERATION"
                    ) =>
                {
                    record_span(
                        &mut outcomes,
                        next - 1,
                        next,
                        BarCueOutcome::PinnedMapRejected,
                    );
                    continue;
                }
                Err(e) => return Err(e),
            };
            plan.header.extensions.insert("tactic_cue_candidate".into(), serde_json::json!({
                "outgoing_source_frame": a.start, "incoming_source_frame": b.start,
                "overlap_beats": template.overlap, "incoming_overlap_beats": template.incoming_overlap,
                "pulse_ratio": template.incoming_overlap as f64 / template.overlap as f64,
                "timing_basis": "bars", "bar_beats": a.bar_beats,
            }));
            plans.push(plan);
            record_span(&mut outcomes, next - 1, next, BarCueOutcome::TimingWindow);
        }
        Ok(BarCuePage {
            scanned: BarCueRange {
                end: next,
                ..range.clone()
            },
            remainder: BarCueRange {
                start: next,
                ..range.clone()
            },
            plans,
            block_pruned_cells,
            outcomes,
        })
    }
}

/// Natural endings paired with measured starts, followed by the Cartesian
/// product of measured sparse windows. Cell order does not depend on ranking.
pub struct StructuralCueDomain {
    root: BarCueRange,
    tracks: [Analyzed; 2],
    windows: crate::tactic_cues::StructuralWindows,
    /// Ordered half-open sparse-cell ranges that still need a native map.
    possible_sparse: Vec<std::ops::Range<u64>>,
    context: usize,
    policy: MixingConfig,
}

fn structural_seconds((start, end): (u64, u64), rate: u32) -> f64 {
    (end - start) as f64 / f64::from(rate)
}

fn compatible_incoming(
    outgoing_seconds: f64,
    incoming_seconds: &[f64],
    speed_limit: f64,
) -> std::ops::Range<usize> {
    // The native builder uses the mean of both durations. With the outgoing
    // duration fixed, its speed is nonincreasing across sorted incoming
    // durations, including under the builder's floating-point operations.
    // Use only this necessary speed check here. The native builder still
    // checks the incoming speed and all other timing and map constraints.
    let speed = |incoming| outgoing_seconds / ((outgoing_seconds + incoming) * 0.5);
    let first = incoming_seconds.partition_point(|&incoming| {
        let speed = speed(incoming);
        speed > 1. && (speed - 1.).abs() > speed_limit
    });
    let count = incoming_seconds[first..].partition_point(|&incoming| {
        let speed = speed(incoming);
        speed >= 1. || (speed - 1.).abs() <= speed_limit
    });
    first..first + count
}

impl StructuralCueDomain {
    /// Missing structural analysis leaves the pair unresolved.
    pub fn open(
        store: &Store,
        from: &str,
        to: &str,
        context: usize,
        policy: &MixingConfig,
        cancel: &Cancellation,
    ) -> Result<Option<Self>> {
        cancel.check()?;
        policy.validate()?;
        require(
            (1..=64).contains(&context),
            "STITCH_BEATS",
            "invalid structural cue context length",
        )?;
        let tracks = [
            load_analyzed(store, store.track(from)?)?,
            load_analyzed(store, store.track(to)?)?,
        ];
        let mut windows = if excluded(tracks.each_ref()) {
            crate::tactic_cues::StructuralWindows {
                tails: vec![],
                sparse: [vec![], vec![]],
                complete: true,
            }
        } else {
            let Some(windows) = crate::tactic_cues::structural_windows(store, tracks.each_ref())?
                .filter(|windows| windows.complete)
            else {
                return Ok(None);
            };
            windows
        };
        windows.tails.sort_unstable();
        windows.tails.dedup();
        for side in &mut windows.sparse {
            side.sort_unstable();
            side.dedup();
        }
        let [a, b] = &mut windows.sparse;
        let incoming_format = &tracks[1].track.format;
        // Keep no-context windows in the raw domain as a rejected suffix.
        // Stable sorting retains frame order within each equal duration.
        b.sort_by(|&left, &right| {
            (left.1 == incoming_format.frames)
                .cmp(&(right.1 == incoming_format.frames))
                .then_with(|| (left.1 - left.0).cmp(&(right.1 - right.0)))
        });
        let usable = b.partition_point(|&(_, end)| end < incoming_format.frames);
        let incoming_seconds = b[..usable]
            .iter()
            .map(|&window| structural_seconds(window, incoming_format.sample_rate_hz))
            .collect::<Vec<_>>();
        let end = (a.len() as u64)
            .checked_mul(b.len() as u64)
            .and_then(|n| n.checked_add(windows.tails.len() as u64))
            .ok_or_else(|| {
                Error::new(
                    "CUE_DOMAIN",
                    "structural cue domain exceeds ordinal capacity",
                )
            })?;
        let mut possible_sparse = Vec::new();
        for (row, &window) in a.iter().enumerate() {
            cancel.check()?;
            if window.0 == 0 {
                // A native overlap needs a real outgoing lead-in.
                continue;
            }
            let outgoing_seconds =
                structural_seconds(window, tracks[0].track.format.sample_rate_hz);
            let compatible = compatible_incoming(
                outgoing_seconds,
                &incoming_seconds,
                policy.max_speed_deviation,
            );
            if !compatible.is_empty() {
                let base = row as u64 * b.len() as u64;
                possible_sparse.push(base + compatible.start as u64..base + compatible.end as u64);
            }
        }
        let root = BarCueRange {
            domain_id: domain_id(&tracks, STRUCTURAL_CUE_PROFILE, context, policy)?,
            start: 0,
            end,
        };
        Ok(Some(Self {
            root,
            tracks,
            windows,
            possible_sparse,
            context,
            policy: policy.clone(),
        }))
    }

    pub fn range(&self) -> BarCueRange {
        self.root.clone()
    }

    pub fn page(
        &self,
        range: &BarCueRange,
        scan_limit: usize,
        plan_limit: usize,
        cancel: &Cancellation,
    ) -> Result<BarCuePage> {
        require(
            range.domain_id == self.root.domain_id
                && range.start <= range.end
                && range.end <= self.root.end
                && scan_limit > 0
                && plan_limit > 0,
            "CUE_DOMAIN",
            "invalid structural cue range or page limits",
        )?;
        let mut plans = Vec::new();
        let mut outcomes = Vec::new();
        let mut block_pruned_cells = 0;
        let mut checked = 0;
        let mut next = range.start;
        let tail_count = self.windows.tails.len() as u64;
        let [a, b] = &self.windows.sparse;
        while next < range.end && checked < scan_limit && plans.len() < plan_limit {
            cancel.check()?;
            let windows = if next < tail_count {
                self.windows.tails[next as usize]
            } else {
                let cell = next - tail_count;
                let index = self.possible_sparse.partition_point(|run| run.end <= cell);
                let boundary = self
                    .possible_sparse
                    .get(index)
                    .map_or(range.end, |run| (run.start + tail_count).min(range.end));
                if boundary > next {
                    record_span(
                        &mut outcomes,
                        next,
                        boundary,
                        BarCueOutcome::BlockTimingRejected,
                    );
                    block_pruned_cells += boundary - next;
                    next = boundary;
                    continue;
                }
                let width = b.len() as u64;
                [a[(cell / width) as usize], b[(cell % width) as usize]]
            };
            let outcome = match crate::tactic_cues::native_pair(
                self.tracks.each_ref(),
                windows,
                self.context,
                &self.policy,
                "structural",
            )? {
                Some(plan) => {
                    plans.push(plan);
                    BarCueOutcome::TimingWindow
                }
                None => BarCueOutcome::PinnedMapRejected,
            };
            record_span(&mut outcomes, next, next + 1, outcome);
            next += 1;
            checked += 1;
        }
        Ok(BarCuePage {
            scanned: BarCueRange {
                end: next,
                ..range.clone()
            },
            remainder: BarCueRange {
                start: next,
                ..range.clone()
            },
            plans,
            block_pruned_cells,
            outcomes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passage(seconds: f64, pulses: usize, meter: usize) -> Passage {
        Passage {
            start: 0,
            end: 0,
            context: 0,
            seconds,
            beat_phases: (0..=pulses).map(|i| i as f64 / pulses as f64).collect(),
            beat_frames: vec![],
            downbeats: (0..=pulses).step_by(meter).collect(),
            grid_error: 0.,
            bar_beats: meter,
            continued_meter: false,
        }
    }

    #[test]
    fn bar_row_bounds_preserve_every_feasible_ordinal_across_tempo_changes() {
        let policy = MixingConfig::default();
        let seconds = [17., 17., 17., 17., 8., 8., 8., 8., 8., 8., 8., 8., 8.];
        for (outgoing_pulses, incoming_pulses) in [(8, 8), (8, 16), (16, 8)] {
            let outgoing = passage(8., outgoing_pulses, 4);
            let incoming = IncomingPassages::new(
                seconds
                    .iter()
                    .enumerate()
                    .map(|(i, &s)| {
                        passage(s, incoming_pulses, if (8..12).contains(&i) { 2 } else { 4 })
                    })
                    .collect(),
            );
            let mapped = outgoing_pulses != incoming_pulses;
            let feasible = |i: usize| {
                mapped_pair_timing(&outgoing, &incoming.passages[i], &policy)
                    .is_some_and(|timing| timing.0 <= MAX_TACTIC_CONTINUATION_SECONDS)
            };
            assert_eq!(
                (0..seconds.len())
                    .filter(|&i| feasible(i))
                    .collect::<Vec<_>>(),
                [4, 5, 6, 7, 12]
            );
            assert!(!impossible_block(
                TimingBounds::passage(&outgoing),
                incoming.bounds[1],
                mapped,
                policy.max_speed_deviation,
            ));
            for start in 0..seconds.len() {
                let mut next = start;
                let mut surviving = Vec::new();
                let mut skipped = Vec::new();
                while next < seconds.len() {
                    if let Some(end) = incoming.rejected_end(
                        next,
                        TimingBounds::passage(&outgoing),
                        mapped,
                        policy.max_speed_deviation,
                    ) {
                        assert!(next < end && end <= seconds.len());
                        assert!((next..end).all(|i| !feasible(i)));
                        skipped.push(next..end);
                        next = end;
                    } else {
                        if feasible(next) {
                            surviving.push(next);
                        }
                        next += 1;
                    }
                }
                assert_eq!(
                    surviving,
                    (start..seconds.len())
                        .filter(|&i| feasible(i))
                        .collect::<Vec<_>>()
                );
                if start == 0 {
                    assert_eq!(skipped, [0..4, 8..12]);
                }
            }
        }
    }

    #[test]
    fn structural_bounds_match_each_outgoing_speed_check_at_float_boundaries() {
        for outgoing in [1. / 48_000., 0.2, 1.0, 8.273, 120., 1.0e9] {
            for limit in [0., 0.03, 0.06, 0.25] {
                let short = outgoing * (2. / (1. + limit) - 1.);
                let long = outgoing * (2. / (1. - limit) - 1.);
                let mut incoming = vec![outgoing / 4., outgoing, outgoing * 4.];
                for boundary in [short, long] {
                    let boundary: f64 = boundary;
                    incoming.extend([boundary.next_down(), boundary, boundary.next_up()]);
                }
                incoming.sort_by(f64::total_cmp);
                let surviving = compatible_incoming(outgoing, &incoming, limit);
                let exact = incoming
                    .iter()
                    .enumerate()
                    .filter_map(|(i, &b)| {
                        let duration = (outgoing + b) / 2.;
                        ((outgoing / duration - 1.).abs() <= limit).then_some(i)
                    })
                    .collect::<Vec<_>>();
                assert_eq!(surviving.collect::<Vec<_>>(), exact);
            }
        }
    }
}
