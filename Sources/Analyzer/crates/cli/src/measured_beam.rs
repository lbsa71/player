//! Search over admitted cue and tactic choices. A route's last source frame
//! is part of its state because it constrains the next exit.
use super::*;
use crate::tactics::MeasuredEdge;
use mixlib_planner::{CandidateEvidenceCache, StitchRequest, CHAIN_SOLO_SECONDS};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};

pub(super) fn prepared_pool(store: &Store) -> Result<Vec<Track>> {
    let mut pool = vec![];
    let mut hashes = BTreeSet::new();
    for track in store.tracks()? {
        if track.active_analysis.is_some()
            && matches!(track.lifecycle, Lifecycle::Available)
            && store
                .analysis(&track)?
                .artifacts
                .contains_key("tactic_analysis")
            && hashes.insert(track.audio_hash.digest.clone())
        {
            pool.push(track);
        }
    }
    pool.sort_by(|a, b| a.track_id.cmp(&b.track_id));
    Ok(pool)
}

type Graph = Vec<Vec<MeasuredEdge>>;

fn measure_graph(
    store: &Store,
    pool: &[Track],
    policy: &MixingConfig,
    context: usize,
    budget: usize,
    cancel: &Cancellation,
) -> Result<Graph> {
    let n = pool.len();
    let pairs: Vec<_> = (0..n)
        .flat_map(|a| (0..n).filter(move |&b| a != b).map(move |b| (a, b)))
        .collect();
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get().min(4));
    let completed = AtomicUsize::new(0);
    let mut graph: Graph = (0..n * n).map(|_| vec![]).collect();
    std::thread::scope(|scope| -> Result<()> {
        let jobs: Vec<_> = (0..workers)
            .map(|worker| {
                let pairs = &pairs;
                let completed = &completed;
                scope.spawn(move || -> Result<Vec<(usize, Vec<MeasuredEdge>)>> {
                    let mut cache = CandidateEvidenceCache::new();
                    let mut measured = vec![];
                    for &(a, b) in pairs.iter().skip(worker).step_by(workers) {
                        cancel.check()?;
                        let request = StitchRequest {
                            from: &pool[a].track_id,
                            to: &pool[b].track_id,
                            overlap_beats: 8,
                            context_beats: context,
                            outgoing_after_frame: 0,
                        };
                        measured.push((
                            a * n + b,
                            tactics::measure_edges(
                                store, &request, policy, &mut cache, budget, cancel,
                            )?,
                        ));
                        let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                        if done % 10 == 0 || done == pairs.len() {
                            eprintln!(
                                "stitch: measured {done}/{} directed track pairs",
                                pairs.len()
                            );
                        }
                    }
                    Ok(measured)
                })
            })
            .collect();
        for job in jobs {
            for (index, edges) in job
                .join()
                .map_err(|_| Error::new("TACTIC_WORKER", "measurement worker failed"))??
            {
                graph[index] = edges;
            }
        }
        Ok(())
    })?;
    Ok(graph)
}

#[derive(Clone, Copy)]
struct Quality {
    fallbacks: usize,
    weakest: f64,
    total: f64,
}

impl Quality {
    const EMPTY: Self = Self {
        fallbacks: 0,
        weakest: f64::INFINITY,
        total: 0.,
    };

    fn extend(self, score: f64, last_resort: bool) -> Self {
        Self {
            fallbacks: self.fallbacks + usize::from(last_resort),
            weakest: self.weakest.min(score),
            total: self.total + score,
        }
    }

    fn best_first(self, other: Self) -> std::cmp::Ordering {
        self.fallbacks
            .cmp(&other.fallbacks)
            .then_with(|| other.weakest.total_cmp(&self.weakest))
            .then_with(|| other.total.total_cmp(&self.total))
    }
}

struct Route {
    used: u128,
    last: usize,
    next_exit: u64,
    quality: Quality,
    plan: Option<MixPlan>,
}

struct Extension {
    prefix: usize,
    to: usize,
    edge: usize,
    quality: Quality,
    region: (usize, u64),
}

fn solo_frames(track: &Track) -> u64 {
    CHAIN_SOLO_SECONDS * u64::from(track.format.sample_rate_hz)
}

/// An optimistic reachability bound. Ignoring the no-repeat rule can only
/// keep extra states. Every actual extension still checks the used tracks.
fn runway_bounds(graph: &Graph, solo: &[u64], count: usize) -> Vec<Vec<Option<u64>>> {
    let n = solo.len();
    let mut bounds = vec![vec![Some(u64::MAX); n]];
    for remaining in 1..count {
        let previous = &bounds[remaining - 1];
        let layer = (0..n)
            .map(|from| {
                (0..n)
                    .flat_map(|to| {
                        graph[from * n + to].iter().filter(move |edge| {
                            previous[to].is_some_and(|latest| edge.entry_end + solo[to] <= latest)
                        })
                    })
                    .map(|edge| edge.exit)
                    .max()
            })
            .collect();
        bounds.push(layer);
    }
    bounds
}

#[derive(Clone, Copy, Default)]
struct ExitBounds {
    latest_feasible: Option<u64>,
    earliest_infeasible: Option<u64>,
}

/// For timing feasibility alone, the earliest reachable entry into each
/// unused track dominates later entries. Scored extensions still keep their
/// actual cue and validate the complete assembled schedule.
struct Runway<'a> {
    graph: &'a Graph,
    solo: &'a [u64],
    memo: BTreeMap<(u128, usize, usize), ExitBounds>,
}

impl Runway<'_> {
    fn can_finish(&mut self, used: u128, last: usize, exit: u64, remaining: usize) -> bool {
        if remaining == 0 {
            return true;
        }
        let key = (used, last, remaining);
        let mut bounds = self.memo.get(&key).copied().unwrap_or_default();
        if bounds.latest_feasible.is_some_and(|latest| exit <= latest) {
            return true;
        }
        if bounds
            .earliest_infeasible
            .is_some_and(|earliest| exit >= earliest)
        {
            return false;
        }
        let n = self.solo.len();
        let possible = (0..n).any(|to| {
            used & (1 << to) == 0
                && self.graph[last * n + to]
                    .iter()
                    .filter(|edge| edge.exit >= exit)
                    .map(|edge| edge.entry_end + self.solo[to])
                    .min()
                    .is_some_and(|next| self.can_finish(used | (1 << to), to, next, remaining - 1))
        });
        if possible {
            bounds.latest_feasible = Some(exit);
        } else {
            bounds.earliest_infeasible = Some(exit);
        }
        self.memo.insert(key, bounds);
        possible
    }
}

fn route_local(error: &Error) -> bool {
    matches!(
        error.code.as_str(),
        "CHAIN_CUES"
            | "TIME_MAP_SPEED"
            | "TIME_MAP_ACCELERATION"
            | "TIME_MAP"
            | "TACTIC_PLAN"
            | "TACTIC_VOCALS"
            | "TACTIC_TAIL"
            | "TACTIC_STEM_TIMEBASE"
    ) || error.code.starts_with("TACTIC_")
}

fn search(
    store: &Store,
    graph: &Graph,
    pool: &[Track],
    count: usize,
    width: usize,
    cancel: &Cancellation,
) -> Result<Option<MixPlan>> {
    let n = pool.len();
    let solo: Vec<_> = pool.iter().map(solo_frames).collect();
    let bounds = runway_bounds(graph, &solo, count);
    let mut runway = Runway {
        graph,
        solo: &solo,
        memo: BTreeMap::new(),
    };
    let mut beam: Vec<_> = (0..n)
        .map(|last| Route {
            used: 1 << last,
            last,
            next_exit: 0,
            quality: Quality::EMPTY,
            plan: None,
        })
        .collect();
    if n <= 16
        && !beam
            .iter()
            .any(|r| runway.can_finish(r.used, r.last, 0, count - 1))
    {
        eprintln!("stitch: measured cues have no distinct-track route with enough solo time");
        return Ok(None);
    }
    for size in 2..=count {
        cancel.check()?;
        let remaining = count - size;
        let mut extensions = vec![];
        for (prefix, route) in beam.iter().enumerate() {
            for (to, track) in pool.iter().enumerate() {
                if route.used & (1 << to) != 0 {
                    continue;
                }
                let Some(latest) = bounds[remaining][to] else {
                    continue;
                };
                for (edge, choice) in graph[route.last * n + to].iter().enumerate() {
                    if choice.exit < route.next_exit
                        || choice.entry_end + solo_frames(track) > latest
                    {
                        continue;
                    }
                    extensions.push(Extension {
                        prefix,
                        to,
                        edge,
                        quality: route.quality.extend(choice.score, choice.last_resort),
                        region: (
                            to,
                            choice.entry_end / (8 * u64::from(track.format.sample_rate_hz)),
                        ),
                    });
                }
            }
        }
        extensions.sort_by(|a, b| a.quality.best_first(b.quality));
        let mut next = vec![];
        let mut regions = BTreeSet::new();
        let mut deferred = vec![];
        let mut rejected = BTreeMap::new();
        let mut try_extend = |choice: Extension| -> Result<Option<Route>> {
            cancel.check()?;
            let parent = &beam[choice.prefix];
            let edge = &graph[parent.last * n + choice.to][choice.edge];
            let used = parent.used | (1 << choice.to);
            let next_exit = edge.entry_end + solo[choice.to];
            let lookahead = if n <= 16 { remaining } else { remaining.min(3) };
            if !runway.can_finish(used, choice.to, next_exit, lookahead) {
                return Ok(None);
            }
            let plan = match chain::extend_measured(
                parent.plan.as_ref(),
                &edge.plan,
                edge.score,
                edge.last_resort,
            ) {
                Ok(plan) => plan,
                Err(error) if route_local(&error) => {
                    *rejected.entry(error.code).or_insert(0usize) += 1;
                    return Ok(None);
                }
                Err(error) => return Err(error),
            };
            let plan = if remaining == 0 {
                match chain::finish_measured(store, plan, cancel) {
                    Ok(plan) => plan,
                    Err(error) if route_local(&error) => {
                        *rejected.entry(error.code).or_insert(0usize) += 1;
                        return Ok(None);
                    }
                    Err(error) => return Err(error),
                }
            } else {
                plan
            };
            Ok(Some(Route {
                used,
                last: choice.to,
                next_exit,
                quality: choice.quality,
                plan: Some(plan),
            }))
        };
        for choice in extensions {
            let region = choice.region;
            if remaining > 0 && regions.contains(&region) {
                deferred.push(choice);
                continue;
            }
            if let Some(route) = try_extend(choice)? {
                if remaining == 0 {
                    return Ok(route.plan);
                }
                next.push(route);
                regions.insert(region);
                if next.len() == width {
                    break;
                }
            }
        }
        if next.len() < width {
            for choice in deferred {
                if let Some(route) = try_extend(choice)? {
                    next.push(route);
                    if next.len() == width {
                        break;
                    }
                }
            }
        }
        eprintln!(
            "stitch: measured beam at {size}/{count} tracks retains {} states",
            next.len()
        );
        if next.is_empty() {
            if !rejected.is_empty() {
                eprintln!("stitch: assembled route rejections at {size} tracks: {rejected:?}");
            }
            return Ok(None);
        }
        beam = next;
    }
    Ok(None)
}

pub(super) fn choose(
    store: &Store,
    policy: &MixingConfig,
    context: usize,
    count: usize,
    pool: Vec<Track>,
    cancel: &Cancellation,
) -> Result<MixPlan> {
    require(
        pool.len() >= count && pool.len() <= 128,
        "CHAIN_POOL",
        format!(
            "need {count} to 128 distinct prepared recordings; found {}",
            pool.len()
        ),
    )?;
    let snapshot = store.read_snapshot();
    for budget in [24, 64] {
        eprintln!(
            "stitch: building measured choices from {} tracks, up to {budget} cue windows per pair",
            pool.len()
        );
        let graph = measure_graph(&snapshot, &pool, policy, context, budget, cancel)?;
        let edges: usize = graph.iter().map(Vec::len).sum();
        eprintln!("stitch: {edges} admitted cue/tactic choices");
        for width in [policy.beam_width, policy.beam_width.saturating_mul(4)] {
            let Some(mut plan) = search(&snapshot, &graph, &pool, count, width, cancel)? else {
                continue;
            };
            snapshot.verify_read_snapshot(cancel)?;
            plan.planner_version = "measured-cue-beam/1".into();
            plan.header.extensions.insert("library_pick".into(), serde_json::json!({
                "analyzed_tracks": pool.len(), "measured_choices": edges, "cue_budget_per_pair": budget,
                "beam_width": width, "track_ids": plan.placements.iter().map(|p| &p.track_id).collect::<Vec<_>>(),
            }));
            return Ok(plan);
        }
    }
    snapshot.verify_read_snapshot(cancel)?;
    Err(Error::new(
        "TACTIC_NO_COMPLETE_ROUTE",
        format!(
            "measured beam found no complete {count}-track route from {} prepared recordings",
            pool.len()
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_better_local_join_can_leave_too_little_runway() {
        let plan: MixPlan =
            serde_json::from_str(include_str!("../../../examples/mix-plan.json")).unwrap();
        let edge = |exit, entry_end, score| MeasuredEdge {
            plan: plan.clone(),
            exit,
            entry_end,
            score,
            last_resort: false,
        };
        let mut graph: Graph = (0..9).map(|_| vec![]).collect();
        graph[1] = vec![edge(30, 150, 80.), edge(30, 100, 60.)];
        graph[5] = vec![edge(170, 20, 75.)];
        let bounds = runway_bounds(&graph, &[45; 3], 3);
        let latest = bounds[1][1].unwrap();
        let usable = graph[1].iter().filter(|e| e.entry_end + 45 <= latest);
        assert_eq!(usable.map(|e| e.score).collect::<Vec<_>>(), [60.]);
        assert_eq!(bounds[2][0], Some(30));
        let mut runway = Runway {
            graph: &graph,
            solo: &[45; 3],
            memo: BTreeMap::new(),
        };
        assert!(!runway.can_finish(0b011, 1, 195, 1));
        assert!(runway.can_finish(0b011, 1, 145, 1));
        assert!(!runway.can_finish(0b111, 1, 145, 1));
    }
}
