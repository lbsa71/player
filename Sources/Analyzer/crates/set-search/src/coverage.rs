//! Upper bounds over disjoint regions of a declared route domain.

use crate::objective::SECONDS_PER_MINUTE;

/// A region's proof status. An unmeasured region can still have a universal
/// upper ceiling. A heuristic timeout or empty shortlist proves neither case.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RegionCeiling {
    Unknown,
    Empty,
    Upper(f64),
}

/// Take the largest ceiling over the union, including the current incumbent.
/// An empty union with no incumbent has no numerical score ceiling.
pub fn covered_upper(incumbent: Option<f64>, regions: &[RegionCeiling]) -> Option<f64> {
    let mut bound = incumbent;
    if bound.is_some_and(|value| !value.is_finite()) {
        return None;
    }
    for region in regions {
        match *region {
            RegionCeiling::Empty => {}
            RegionCeiling::Upper(upper) if upper.is_finite() => {
                bound = Some(bound.map_or(upper, |old| old.max(upper)));
            }
            _ => return None,
        }
    }
    bound
}

#[derive(Debug, Clone, Copy)]
pub struct JoinCredit {
    pub from: usize,
    pub to: usize,
    pub dense: f64,
    pub handoff: f64,
}

/// Verified input to the signed assignment relaxation. Track capacities
/// include their outgoing overlap and a conservative replay allowance.
#[derive(Debug, Clone, Copy)]
pub struct MeasuredJoin {
    pub from: usize,
    pub to: usize,
    pub dense: f64,
    pub handoff: f64,
    pub fit_shortfall: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct MeasuredAssignment {
    pub ceiling: RegionCeiling,
    pub surplus_upper: Option<f64>,
    pub minimum_joins: Option<usize>,
    pub clock_buckets: usize,
}

/// Relax each route of k joins to k distinct outgoing owners and,
/// independently, k distinct incoming owners. The best signed weight for
/// each owner includes its fit cost. Any such route uses k+1 distinct tracks,
/// so the sum of the largest k+1 capacities bounds its clock.
pub fn measured_assignment_upper(
    capacities: &[f64],
    minimum: f64,
    maximum: f64,
    incumbent: Option<f64>,
    joins: impl IntoIterator<Item = MeasuredJoin>,
) -> Option<MeasuredAssignment> {
    let n = capacities.len();
    if !(2..=128).contains(&n)
        || !minimum.is_finite()
        || !maximum.is_finite()
        || minimum < SECONDS_PER_MINUTE
        || maximum < minimum
        || incumbent.is_some_and(|score| !score.is_finite())
        || capacities.iter().any(|&cap| !cap.is_finite() || cap < 0.)
    {
        return None;
    }

    // Binary scaling is exact in this input range. Upward credit rounding
    // and downward cost rounding let signed assignment sums use integers.
    const SCALE: f64 = (1 << 20) as f64;
    const COMPONENT_LIMIT: f64 = 4096.;
    let mut outgoing: Vec<Option<i64>> = vec![None; n];
    let mut incoming = outgoing.clone();
    for join in joins {
        if join.from == join.to
            || join.from >= n
            || join.to >= n
            || [join.dense, join.handoff, join.fit_shortfall]
                .into_iter()
                .any(|x| !(0. ..=COMPONENT_LIMIT).contains(&x))
        {
            return None;
        }
        let up = |x: f64| (x * SCALE).ceil() as i64;
        let down = |x: f64| (x * SCALE).floor() as i64;
        let value = up(join.dense) + up(join.handoff) - down(join.fit_shortfall);
        for owner in [&mut outgoing[join.from], &mut incoming[join.to]] {
            *owner = Some(owner.map_or(value, |old| old.max(value)));
        }
    }
    let descending = |owners: Vec<Option<i64>>| {
        let mut weights: Vec<_> = owners.into_iter().flatten().collect();
        weights.sort_unstable_by(|a, b| b.cmp(a));
        weights
    };
    let outgoing = descending(outgoing);
    let incoming = descending(incoming);
    let mut capacities = capacities.to_vec();
    capacities.sort_unstable_by(|a, b| b.total_cmp(a));
    let mut clock = capacities[0];
    let mut sum_out = 0;
    let mut sum_in = 0;
    let mut result = MeasuredAssignment {
        ceiling: RegionCeiling::Empty,
        surplus_upper: None,
        minimum_joins: None,
        clock_buckets: 0,
    };
    for (index, (&out, &inc)) in outgoing.iter().zip(&incoming).take(n - 1).enumerate() {
        let count = index + 1;
        clock = (clock + capacities[count]).next_up();
        sum_out += out;
        sum_in += inc;
        let latest = clock.min(maximum);
        if latest < minimum {
            continue;
        }
        result.minimum_joins.get_or_insert(count);
        result.clock_buckets += 1;

        // Each scorer accumulator is bounded by 127 * 4096. Its ULP,
        // times the number of additions, is below one SCALE unit. Four
        // units cover the three accumulated lanes and their final sum
        // and subtraction. Dropping pacing and replay costs only raises
        // the numerator. Integer conversion and multiplication fit in 53 bits.
        let numerator = SECONDS_PER_MINUTE * (sum_out.min(sum_in) + 4) as f64 / SCALE;
        let denominator = if numerator < 0. { latest } else { minimum };
        let score = (numerator / denominator).next_up();
        result.ceiling = match result.ceiling {
            RegionCeiling::Empty => RegionCeiling::Upper(score),
            RegionCeiling::Upper(old) => RegionCeiling::Upper(old.max(score)),
            RegionCeiling::Unknown => unreachable!(),
        };
        if let Some(l) = incumbent {
            let time = if l < 0. { latest } else { minimum };
            let surplus = (numerator + (-l * time).next_up()).next_up();
            if !surplus.is_finite() {
                return None;
            }
            result.surplus_upper =
                Some(result.surplus_upper.map_or(surplus, |old| old.max(surplus)));
        }
    }
    Some(result)
}

/// Relax a unique-song path to one selected outgoing join per track, and
/// separately to one incoming join per track. Each gives a feasible dual
/// for the assignment relaxation. Both cover every permitted path.
pub fn measured_credit_upper(
    tracks: usize,
    joins: impl IntoIterator<Item = JoinCredit>,
) -> Option<f64> {
    let max_joins = tracks.saturating_sub(1);
    let mut outgoing = vec![[0_f64; 2]; tracks];
    let mut incoming = vec![[0_f64; 2]; tracks];
    for join in joins {
        if join.from == join.to
            || join.from >= tracks
            || join.to >= tracks
            || [join.dense, join.handoff]
                .iter()
                .any(|credit| !credit.is_finite() || *credit < 0.)
        {
            return None;
        }
        for (lane, credit) in [join.dense, join.handoff].into_iter().enumerate() {
            outgoing[join.from][lane] = outgoing[join.from][lane].max(credit);
            incoming[join.to][lane] = incoming[join.to][lane].max(credit);
        }
    }
    let sum_lane = |limits: &[[f64; 2]], lane: usize| {
        let mut credits = limits.iter().map(|entry| entry[lane]).collect::<Vec<_>>();
        credits.sort_unstable_by(|a, b| b.total_cmp(a));
        credits
            .into_iter()
            .take(max_joins)
            .filter(|&value| value > 0.)
            .fold(0., |sum, value| (sum + value).next_up())
    };
    let rounded_lane = |lane| {
        pad_positive_sum(
            sum_lane(&outgoing, lane).min(sum_lane(&incoming, lane)),
            max_joins,
        )
    };
    let upper = rounded_lane(0) + rounded_lane(1);
    upper.is_finite().then_some(upper)
}

/// A ceiling for any collection of at most `joins` nonnegative measured
/// credits with an admitted per-join cap. The scorer accumulates two lanes.
pub fn unrestricted_credit_upper(joins: usize, cap: f64) -> Option<f64> {
    if !cap.is_finite() || cap < 0. || joins > 127 {
        return None;
    }
    if cap == 0. || joins == 0 {
        return Some(0.);
    }
    let mathematical_upper = (cap * joins as f64).next_up();
    let upper = pad_positive_sum(mathematical_upper, joins * 2 + 1);
    upper.is_finite().then_some(upper)
}

/// Bound the complete-set score when each recorded join credit is checked
/// against its overlap duration plus `slack`. The overlaps must be disjoint
/// inside the final output clock. Final penalties may only reduce the score.
pub fn overlap_clock_score_upper(
    minimum_seconds: f64,
    maximum_seconds: f64,
    max_joins: usize,
    slack: f64,
) -> Option<f64> {
    overlap_clock_with_runway(minimum_seconds, maximum_seconds, max_joins, slack, 0.)
}

fn overlap_clock_with_runway(
    minimum_seconds: f64,
    maximum_seconds: f64,
    max_joins: usize,
    slack: f64,
    solo_seconds_lower: f64,
) -> Option<f64> {
    if !minimum_seconds.is_finite()
        || !maximum_seconds.is_finite()
        || minimum_seconds < SECONDS_PER_MINUTE
        || maximum_seconds < minimum_seconds
        || max_joins > 127
        || !slack.is_finite()
        || slack < 0.
        || !(0. ..=maximum_seconds).contains(&solo_seconds_lower)
    {
        return None;
    }
    if max_joins == 0 {
        return Some(0.);
    }

    let slack_sum = (slack * max_joins as f64).next_up();
    let headroom = ((maximum_seconds + slack_sum).next_up() + 1.).next_up() * 2.;
    let ulp = headroom.next_up() - headroom;
    // Per join: frame division, admission addition, credit addition, and
    // the two separate score accumulators. Also allow rounding of the
    // final output duration and of the sum of the two credit lanes.
    let allowance = (slack_sum + (ulp * (5 * max_joins + 2) as f64).next_up()).next_up();
    if (maximum_seconds + allowance).next_up() >= headroom {
        return None;
    }

    // Rounded score credit Q is at most T - solo + allowance. Use that
    // coupling and allow rounding of the evaluator's multiply and divide.
    let scaled_headroom = (SECONDS_PER_MINUTE * headroom).next_up();
    let multiply_ulp = scaled_headroom.next_up() - scaled_headroom;
    let numerator_allowance = ((SECONDS_PER_MINUTE * allowance).next_up() + multiply_ulp).next_up();
    let reserved = (SECONDS_PER_MINUTE * solo_seconds_lower)
        .next_down()
        .max(0.);
    let adjustment = (numerator_allowance - reserved).next_up();
    let duration = if adjustment < 0. {
        maximum_seconds
    } else {
        minimum_seconds
    };
    let excess = (adjustment / duration).next_up();
    let score = (SECONDS_PER_MINUTE + excess).next_up().next_up();
    score.is_finite().then_some(score)
}

/// Relax a unique-song route by length. Only the internal songs reserve
/// runway, so use the shortest `joins - 1` runway floors for each bucket.
/// The caller verifies the floors against the admitted playback geometry.
pub fn paced_overlap_score_upper(
    minimum_seconds: f64,
    maximum_seconds: f64,
    mathematical_credit_cap: f64,
    slack: f64,
    solo_seconds_lower: &[f64],
) -> Option<f64> {
    paced_bucket_upper(
        minimum_seconds,
        maximum_seconds,
        mathematical_credit_cap,
        slack,
        solo_seconds_lower.to_vec(),
        &[],
        1,
        None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn paced_bucket_upper(
    minimum_seconds: f64,
    maximum_seconds: f64,
    mathematical_credit_cap: f64,
    slack: f64,
    mut optional: Vec<f64>,
    forced: &[f64],
    minimum_joins: usize,
    clock_capacity: Option<ComponentCapacity>,
    preferred: Option<ComponentCapacity>,
) -> Option<f64> {
    let max_joins = (optional.len() + forced.len()).checked_sub(1)?;
    overlap_clock_score_upper(minimum_seconds, maximum_seconds, max_joins, slack)?;
    if !optional
        .iter()
        .chain(forced)
        .all(|s| s.is_finite() && *s >= 0.)
    {
        return None;
    }
    optional.sort_unstable_by(f64::total_cmp);
    let mut runway = forced
        .iter()
        .fold(0., |sum, value| (sum + value).next_down().max(0.));
    let mut upper: f64 = 0.;
    let minimum_joins = match clock_capacity {
        Some(capacity) => match capacity.minimum_tracks(minimum_seconds) {
            Some(tracks) => minimum_joins.max(tracks.saturating_sub(1)),
            None => return Some(0.),
        },
        None => minimum_joins,
    };
    let first = forced.len() + 1;
    let preferred_totals = preferred.map(ComponentCapacity::minimum_prefixes);
    for joins in first..=max_joins {
        if joins > first {
            runway = (runway + optional[joins - first - 1]).next_down().max(0.);
        }
        if runway > maximum_seconds {
            break;
        }
        if joins < minimum_joins {
            continue;
        }
        let clock =
            overlap_clock_with_runway(minimum_seconds, maximum_seconds, joins, slack, runway)?;
        let credit = unrestricted_credit_upper(joins, mathematical_credit_cap)?;
        let cap = crate::bounds::ScoreEnvelope {
            min_duration_seconds: minimum_seconds,
            max_duration_seconds: maximum_seconds,
            sustained_quality_upper: credit,
            transition_shortfall_lower: 0.,
            replay_seconds_lower: 0.,
            arc_error_lower: 0.,
            development_penalty_lower: 0.,
        }
        .upper_bound()?;
        // Original solo plus both songs' development credits can use each
        // overlap at most once. Replay adds no development. Jensen's
        // inequality bounds the mean squared shortage even if that time
        // could be reassigned freely among the chosen songs.
        let tracks = joins + 1;
        let dwell = preferred_totals.as_ref().map_or(0., |totals| {
            let total = totals[tracks - 1];
            let average_shortage = ((total - maximum_seconds).next_down()
                / (tracks as f64 * PREFERRED_FLOOR_MAX))
                .next_down()
                .max(0.);
            // Cover rounding in the passage and dwell accumulators, the
            // final division, and frame-to-seconds conversions. These
            // inputs are bounded by 128 tracks and a two-hour clock.
            (average_shortage * average_shortage - 1e-9).max(0.)
        });
        let penalty = (crate::objective::DEVELOPMENT_WEIGHT * dwell)
            .next_down()
            .max(0.);
        let score = clock.min(cap);
        upper = upper.max(if penalty == 0. {
            score
        } else {
            (score - penalty).next_up()
        });
    }
    Some(upper)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RequiredRunway {
    pub members: u128,
    pub internal: u128,
}

#[derive(Clone, Copy, Debug)]
pub struct TrackClockBounds<'a> {
    pub solo_seconds_lower: &'a [f64],
    /// Includes the maximum legal replay allowance. None skips the capacity check.
    pub duration_seconds_upper: Option<&'a [f64]>,
    /// Per-song floors on preferred development, bounded by 150 seconds.
    pub preferred_development_lower: Option<&'a [f64]>,
}

const PREFERRED_FLOOR_MAX: f64 = 150.;

#[derive(Default)]
struct ComponentCapacity {
    required_tracks: usize,
    required_seconds: f64,
    optional_seconds: Vec<f64>,
}

impl ComponentCapacity {
    fn add_floor(&mut self, seconds: f64, required: bool) {
        if required {
            self.required_tracks += 1;
            self.required_seconds = (self.required_seconds + seconds).next_down().max(0.);
        } else {
            self.optional_seconds.push(seconds);
        }
    }

    fn minimum_prefixes(mut self) -> Vec<f64> {
        self.optional_seconds.sort_unstable_by(f64::total_cmp);
        let mut totals = vec![0.; self.required_tracks + self.optional_seconds.len()];
        let mut sum = self.required_seconds;
        if self.required_tracks > 0 {
            totals[self.required_tracks - 1] = sum;
        }
        for (i, seconds) in self.optional_seconds.into_iter().enumerate() {
            sum = (sum + seconds).next_down().max(0.);
            totals[self.required_tracks + i] = sum;
        }
        totals
    }

    fn add(&mut self, seconds: f64, required: bool) {
        if required {
            self.required_tracks += 1;
            self.required_seconds = (self.required_seconds + seconds).next_up();
        } else {
            self.optional_seconds.push(seconds);
        }
    }

    fn minimum_tracks(mut self, minimum_seconds: f64) -> Option<usize> {
        self.optional_seconds
            .sort_unstable_by(|a, b| b.total_cmp(a));
        let mut total = self.required_seconds;
        if total >= minimum_seconds {
            return Some(self.required_tracks);
        }
        self.optional_seconds
            .into_iter()
            .position(|seconds| {
                total = (total + seconds).next_up();
                total >= minimum_seconds
            })
            .map(|index| self.required_tracks + index + 1)
    }
}

/// Optimistic track count over an unrestricted component. The outer option
/// represents invalid input, and the inner option an insufficient clock.
pub fn minimum_capacity_tracks(
    minimum_seconds: f64,
    capacities: &[f64],
    required: u128,
) -> Option<Option<usize>> {
    if !(1..=128).contains(&capacities.len())
        || !minimum_seconds.is_finite()
        || minimum_seconds <= 0.
        || (capacities.len() < 128 && required >> capacities.len() != 0)
        || !capacities.iter().all(|s| s.is_finite() && *s >= 0.)
    {
        return None;
    }
    let mut capacity = ComponentCapacity::default();
    for (track, &seconds) in capacities.iter().enumerate() {
        capacity.add(seconds, required & (1_u128 << track) != 0);
    }
    Some(capacity.minimum_tracks(minimum_seconds))
}

#[derive(Debug, Clone)]
pub struct DirectedCapacity {
    pub viable_tracks: u128,
    pub minimum_tracks: Option<usize>,
    pub iterations: usize,
    pub excluded_pairs: u32,
    retained_pairs: Vec<u128>,
    ancestors: Vec<u128>,
    descendants: Vec<u128>,
}

impl DirectedCapacity {
    pub fn permits_pair(&self, from: usize, to: usize) -> bool {
        self.retained_pairs[from] & (1_u128 << to) != 0
    }

    pub fn retained_pairs(&self) -> u32 {
        self.retained_pairs.iter().map(|row| row.count_ones()).sum()
    }

    /// Tracks that can precede or follow the join, excluding both endpoints.
    pub fn other_tracks_at_pair(&self, from: usize, to: usize) -> u128 {
        self.outside_tracks(from, to, (1_u128 << from) | (1_u128 << to))
    }

    /// Possible predecessors and successors outside a committed subpath.
    pub fn outside_tracks(&self, from: usize, to: usize, used: u128) -> u128 {
        (self.ancestors[from] | self.descendants[to]) & self.viable_tracks & !used
    }
}

/// Every pair of songs on a route is comparable by directed reachability.
/// A track can therefore only draw capacity from its ancestors and
/// descendants. Remove impossible tracks and repeat until no more fall out.
pub fn directed_capacity(
    minimum_seconds: f64,
    capacities: &[f64],
    required: u128,
    permits: impl Fn(usize, usize) -> bool,
) -> Option<DirectedCapacity> {
    minimum_capacity_tracks(minimum_seconds, capacities, required)?;
    let n = capacities.len();
    let mut edges = (0..n)
        .map(|from| {
            (0..n)
                .filter(|&to| from != to && permits(from, to))
                .fold(0, |mask, to| mask | (1_u128 << to))
        })
        .collect::<Vec<_>>();
    let mut viable = u128::MAX >> (128 - n);
    let original_pairs = edges.iter().map(|row| row.count_ones()).sum::<u32>();
    let mut iterations = 0;
    loop {
        iterations += 1;
        let mut reachable = edges
            .iter()
            .enumerate()
            .map(|(from, &edges)| {
                if viable & (1_u128 << from) != 0 {
                    (edges | (1_u128 << from)) & viable
                } else {
                    0
                }
            })
            .collect::<Vec<_>>();
        for via in 0..n {
            let descendants = reachable[via];
            for row in &mut reachable {
                if *row & (1_u128 << via) != 0 {
                    *row |= descendants;
                }
            }
        }
        let mut ancestors = vec![0; n];
        for (from, &row) in reachable.iter().enumerate() {
            for to in track_indices(row) {
                ancestors[to] |= 1_u128 << from;
            }
        }
        let mut retained = 0;
        let mut minimum_tracks = None;
        for (track, &descendants) in reachable.iter().enumerate() {
            let bit = 1_u128 << track;
            if viable & bit == 0 {
                continue;
            }
            let comparable = descendants | ancestors[track];
            if comparable & required != required {
                continue;
            }
            // This relaxation must include the candidate itself.
            let mut capacity = ComponentCapacity::default();
            for (index, &seconds) in capacities.iter().enumerate() {
                let candidate = 1_u128 << index;
                if comparable & candidate != 0 {
                    capacity.add(seconds, (required | bit) & candidate != 0);
                }
            }
            if let Some(count) = capacity.minimum_tracks(minimum_seconds) {
                retained |= bit;
                minimum_tracks = Some(minimum_tracks.map_or(count, |old: usize| old.min(count)));
            }
        }
        // For an immediate join there is no room for an intervening song.
        // Count the union so a song reachable on both sides contributes once.
        let mut capacity_by_mask = std::collections::BTreeMap::new();
        let mut removed_pair = false;
        for (from, row) in edges.iter_mut().enumerate() {
            let mut surviving = 0;
            if retained & (1_u128 << from) != 0 {
                for to in track_indices(*row & retained) {
                    let candidates = (ancestors[from] | reachable[to]) & retained;
                    if candidates & required != required {
                        continue;
                    }
                    let enough = capacity_by_mask.entry(candidates).or_insert_with(|| {
                        let upper = track_indices(candidates)
                            .fold(0., |sum, i| (sum + capacities[i]).next_up());
                        upper >= minimum_seconds
                    });
                    if *enough {
                        surviving |= 1_u128 << to;
                    }
                }
            }
            removed_pair |= surviving != *row;
            *row = surviving;
        }
        if (retained == viable && !removed_pair) || retained == 0 {
            return Some(DirectedCapacity {
                viable_tracks: retained,
                minimum_tracks,
                iterations,
                excluded_pairs: original_pairs
                    - edges.iter().map(|row| row.count_ones()).sum::<u32>(),
                retained_pairs: edges,
                ancestors,
                descendants: reachable,
            });
        }
        viable = retained;
    }
}

fn track_indices(mut mask: u128) -> impl Iterator<Item = usize> {
    std::iter::from_fn(move || {
        if mask == 0 {
            return None;
        }
        let index = mask.trailing_zeros() as usize;
        mask &= mask - 1;
        Some(index)
    })
}

/// A route can only use tracks in one weakly connected component of the
/// graph of permitted directed pairs. An edge with unknown musical evidence
/// must be present. Relax connectivity and direction within each component.
pub fn component_paced_overlap_score_upper(
    minimum_seconds: f64,
    maximum_seconds: f64,
    mathematical_credit_cap: f64,
    slack: f64,
    tracks: TrackClockBounds<'_>,
    required: RequiredRunway,
    permits: impl Fn(usize, usize) -> bool,
) -> Option<f64> {
    let TrackClockBounds {
        solo_seconds_lower,
        duration_seconds_upper,
        preferred_development_lower,
    } = tracks;
    let tracks = solo_seconds_lower.len();
    overlap_clock_score_upper(
        minimum_seconds,
        maximum_seconds,
        tracks.checked_sub(1)?,
        slack,
    )?;
    if !mathematical_credit_cap.is_finite()
        || mathematical_credit_cap < 0.
        || !solo_seconds_lower.iter().all(|s| s.is_finite() && *s >= 0.)
        || duration_seconds_upper.is_some_and(|capacities| {
            capacities.len() != tracks || !capacities.iter().all(|s| s.is_finite() && *s >= 0.)
        })
        || preferred_development_lower.is_some_and(|floors| {
            maximum_seconds > 7200.
                || floors.len() != tracks
                || !floors
                    .iter()
                    .all(|s| (0. ..=PREFERRED_FLOOR_MAX).contains(s))
        })
        || required.internal & !required.members != 0
        || (tracks < 128 && required.members >> tracks != 0)
    {
        return None;
    }
    let mut seen = vec![false; tracks];
    let mut upper: f64 = 0.;
    for first in 0..tracks {
        if seen[first] {
            continue;
        }
        seen[first] = true;
        let mut remaining = vec![first];
        let mut optional = vec![];
        let mut forced = vec![];
        let mut members = 0_u128;
        let mut capacity = duration_seconds_upper.map(|_| ComponentCapacity::default());
        let mut preferred = preferred_development_lower.map(|_| ComponentCapacity::default());
        while let Some(current) = remaining.pop() {
            let bit = 1_u128 << current;
            members |= bit;
            if let (Some(capacity), Some(durations)) = (&mut capacity, duration_seconds_upper) {
                capacity.add(durations[current], required.members & bit != 0);
            }
            if let (Some(preferred), Some(floors)) = (&mut preferred, preferred_development_lower) {
                preferred.add_floor(floors[current], required.members & bit != 0);
            }
            let runway = if required.internal & bit == 0 {
                &mut optional
            } else {
                &mut forced
            };
            runway.push(solo_seconds_lower[current]);
            for (next, visited) in seen.iter_mut().enumerate() {
                if !*visited && (permits(current, next) || permits(next, current)) {
                    *visited = true;
                    remaining.push(next);
                }
            }
        }
        if members & required.members != required.members {
            continue;
        }
        upper = upper.max(paced_bucket_upper(
            minimum_seconds,
            maximum_seconds,
            mathematical_credit_cap,
            slack,
            optional,
            &forced,
            (required.members.count_ones() as usize).saturating_sub(1),
            capacity,
            preferred,
        )?);
    }
    Some(upper)
}

/// Nonnegative partial sums never exceed the mathematical total plus the
/// errors already accumulated. At each addition one upward ULP at that
/// total bounds the possible rounding error, including binade boundaries.
fn pad_positive_sum(mut upper: f64, additions: usize) -> f64 {
    if upper > 0. {
        for _ in 0..additions {
            upper = upper.next_up();
        }
    }
    upper
}
