//! Construct a first candidate from admitted joins without cloning partial mix
//! plans at every node. All shortlisted routes go through normal assembly and
//! musical ending selection. The beam supplies candidates, not exclusions.

use super::*;
use crate::composition::{measured_development_ports, DevelopmentPort};
use mixlib_set_search::objective::transition_shortfall;

const WIDTH: usize = 4096;
const CLOCK_BUCKET_SECONDS: u64 = 75;
const CLOSING_ATTEMPTS_PER_DEPTH: usize = 16;

#[derive(Clone, Copy)]
struct Edge {
    from: usize,
    to: usize,
    overlap: u64,
    tail: u64,
    ports: [Option<DevelopmentPort>; 2],
    value: f64,
}

struct Prefix {
    used: u128,
    edges: Vec<usize>,
    elapsed: u64,
    value: f64,
}

impl Prefix {
    fn last(&self) -> usize {
        *self.edges.last().expect("startup prefix contains a join")
    }
}

#[derive(Default, Serialize)]
pub(super) struct StartupStats {
    pub(super) examined: usize,
    local_pairs_checked: usize,
    pub(super) deepest_tracks: usize,
    pub(super) furthest_join_seconds: f64,
    pub(super) closing_attempts: usize,
    assembly_rejections: BTreeMap<String, usize>,
    beam_drops: usize,
    time_budget_exhausted: bool,
    pub(super) approved: bool,
}

fn bridge_frames(r: &Refiner<'_>, edges: &[Edge], enter: usize, leave: usize) -> Option<u64> {
    let arrival = &r.edges[enter];
    let departure = &r.edges[leave];
    let track = edges[leave].from;
    let a = edges[enter].ports[1];
    let b = edges[leave].ports[0];
    r.search.pacing[track].assess(arrival.entry_end, departure.exit)?;
    if r.search.composer.rules_out_development(track, a, b) {
        return None;
    }
    let bridge = chain::measured_bridge(&arrival.plan, &departure.plan).ok()??;
    r.search
        .composer
        .developed_bridge(track, &bridge, a, b)
        .then_some(bridge.solo_end - bridge.solo_start)
}

fn retain_prefixes(routes: &mut Vec<Prefix>, width: usize, bucket: u64) -> usize {
    let examined = routes.len();
    // Retain different arrival cues and elapsed regions. Keep a second used
    // song set per region because the strongest prefix can strand a successor.
    routes.sort_by(|a, b| {
        b.elapsed
            .cmp(&a.elapsed)
            .then(b.value.total_cmp(&a.value))
            .then(a.edges.cmp(&b.edges))
    });
    let mut regions: BTreeMap<(usize, u64), [Option<u128>; 2]> = BTreeMap::new();
    routes.retain(|route| {
        let used = regions
            .entry((route.last(), route.elapsed / bucket))
            .or_default();
        if used.contains(&Some(route.used)) {
            return false;
        }
        if let Some(slot) = used.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(route.used);
            true
        } else {
            false
        }
    });
    routes.truncate(width);
    examined - routes.len()
}

pub(super) fn search(
    r: &mut Refiner<'_>,
    deadline: Instant,
    cancel: &Cancellation,
) -> Result<StartupStats> {
    let mut stats = StartupStats::default();
    let rate = r.rate();
    let minimum = (r.search.request.min() * rate).ceil() as u64;
    let maximum = (r.search.request.max() * rate).floor() as u64;
    let mut edges = Vec::with_capacity(r.edges.len());
    let mut outgoing = vec![vec![]; r.search.pool.len()];
    for (id, measured) in r.edges.iter().enumerate() {
        if id % 64 == 0 {
            cancel.check()?;
            if Instant::now() >= deadline {
                stats.time_budget_exhausted = true;
                return Ok(stats);
            }
        }
        let (from, to) = r.ends(id);
        let join = &measured.plan.transitions[0];
        let map = &measured.plan.placements[1].time_map;
        let arrival = &map.anchors[map.anchors.len() - 2];
        let tail = if arrival.output_frame == join.end_output_frame {
            (seconds(
                &r.search.pool[to],
                r.search.pool[to]
                    .format
                    .frames
                    .saturating_sub(arrival.source_frame),
            ) * rate
                / map.slopes().last().unwrap())
            .round() as u64
        } else {
            0
        };
        edges.push(Edge {
            from,
            to,
            overlap: join.end_output_frame - join.start_output_frame,
            tail,
            ports: measured_development_ports(measured),
            value: measured.dense_quality_seconds + measured.handoff_quality_seconds
                - transition_shortfall(measured.score),
        });
        if !measured.last_resort && r.search.permits_transition(from, to) {
            outgoing[from].push(id);
        }
    }

    let mut beam = Vec::new();
    for id in outgoing.iter().flatten().copied() {
        cancel.check()?;
        if Instant::now() >= deadline {
            stats.time_budget_exhausted = true;
            return Ok(stats);
        }
        let edge = edges[id];
        if !r.search.required.start_allowed(edge.from) {
            continue;
        }
        if let Some(plan) = r.search.composer.extend(
            None,
            &r.edges[id],
            edge.from,
            0,
            &r.search.pacing[edge.from],
        )? {
            beam.push(Prefix {
                used: (1 << edge.from) | (1 << edge.to),
                edges: vec![id],
                elapsed: plan.transitions[0].end_output_frame,
                value: edge.value,
            });
        }
    }

    let mut bridges = BTreeMap::<(usize, usize), Option<u64>>::new();
    while !beam.is_empty() && Instant::now() < deadline {
        cancel.check()?;
        stats.deepest_tracks = stats.deepest_tracks.max(beam[0].edges.len() + 1);
        stats.furthest_join_seconds = stats
            .furthest_join_seconds
            .max(beam.iter().map(|p| p.elapsed).max().unwrap_or(0) as f64 / rate);

        let mut closing = beam
            .iter()
            .filter(|route| {
                let last = edges[route.last()];
                route.edges.len() >= 2
                    && r.search.required.finish_allowed(last.to)
                    && r.search.required.complete_used(route.used)
                    && route.elapsed + last.tail >= minimum
            })
            .collect::<Vec<_>>();
        closing.sort_by(|a, b| {
            b.value
                .total_cmp(&a.value)
                .then(a.elapsed.cmp(&b.elapsed))
                .then(a.edges.cmp(&b.edges))
        });
        for route in closing.into_iter().take(CLOSING_ATTEMPTS_PER_DEPTH) {
            if Instant::now() >= deadline {
                stats.time_budget_exhausted = true;
                return Ok(stats);
            }
            cancel.check()?;
            let plan = match r.assemble_measured_chain_diagnosed(&route.edges)? {
                Ok((_, plan)) => plan,
                Err(
                    AssemblyRejection::Prefix { reason, .. } | AssemblyRejection::Unknown(reason),
                ) => {
                    *stats.assembly_rejections.entry(reason).or_default() += 1;
                    continue;
                }
            };
            if !r.search.required.satisfied(&plan) {
                continue;
            }
            stats.closing_attempts += 1;
            r.scheduler.closing_attempts += 1;
            let (finished, diagnostics) =
                r.search
                    .composer
                    .close_original(&r.search.snapshot, &plan, cancel)?;
            r.scheduler.closing_diagnostics.merge(&diagnostics);
            if let Some(finished) = finished {
                if r.search.save(finished, "cp_measured_bootstrap", cancel)? {
                    stats.approved = true;
                    return Ok(stats);
                }
            }
        }

        let mut next = Vec::new();
        for route in beam {
            let last = route.last();
            let from = edges[last].to;
            for &leave in &outgoing[from] {
                if stats.examined % 256 == 0 {
                    cancel.check()?;
                    if Instant::now() >= deadline {
                        stats.time_budget_exhausted = true;
                        return Ok(stats);
                    }
                }
                stats.examined += 1;
                let to = edges[leave].to;
                if !r.search.required.allows(
                    route.used,
                    from,
                    to,
                    r.search.pool.len().saturating_sub(route.edges.len() + 2),
                ) {
                    continue;
                }
                let Some(solo) = *bridges.entry((last, leave)).or_insert_with(|| {
                    stats.local_pairs_checked += 1;
                    bridge_frames(r, &edges, last, leave)
                }) else {
                    continue;
                };
                let join_start = route.elapsed + solo;
                let elapsed = join_start + edges[leave].overlap;
                if join_start < r.edges[leave].plan.transitions[0].start_output_frame
                    || elapsed >= maximum
                {
                    continue;
                }
                let mut indices = route.edges.clone();
                indices.push(leave);
                next.push(Prefix {
                    used: route.used | (1 << to),
                    edges: indices,
                    elapsed,
                    value: route.value + edges[leave].value,
                });
                if next.len() >= WIDTH * 8 {
                    stats.beam_drops +=
                        retain_prefixes(&mut next, WIDTH, CLOCK_BUCKET_SECONDS * rate as u64);
                }
            }
        }
        stats.beam_drops += retain_prefixes(&mut next, WIDTH, CLOCK_BUCKET_SECONDS * rate as u64);
        beam = next;
    }
    stats.time_budget_exhausted = !beam.is_empty();
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_retains_distinct_terminal_cues_clock_regions_and_used_songs() {
        let make = |used, last, elapsed, value| Prefix {
            used,
            edges: vec![last],
            elapsed,
            value,
        };
        let mut prefixes = vec![
            make(3, 1, 120, 8.),
            make(3, 1, 110, 12.),
            make(5, 1, 100, 0.),
            make(9, 1, 90, 25.),
            make(3, 2, 100, 0.),
            make(3, 1, 50, 0.),
        ];
        assert_eq!(retain_prefixes(&mut prefixes, 16, 75), 2);
        let kept = prefixes
            .into_iter()
            .map(|p| (p.used, p.last(), p.elapsed))
            .collect::<Vec<_>>();
        assert_eq!(
            kept,
            vec![(3, 1, 120), (5, 1, 100), (3, 2, 100), (3, 1, 50)]
        );
    }
}
