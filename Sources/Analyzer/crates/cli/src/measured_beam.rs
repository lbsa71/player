//! Search over admitted cue and tactic choices. A route's last source frame
//! is part of its state because it constrains the next exit.
use super::*;
use crate::tactics::MeasuredEdge;
use mixlib_planner::{CandidateEvidenceCache, PacingDecision, StitchRequest, TrackPacing};
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
    mixlib_planner::unique_song_pool(store, pool)
}

type Graph = Vec<Vec<MeasuredEdge>>;

fn measure_graph(
    store: &Store,
    pool: &[Track],
    policy: &MixingConfig,
    context: usize,
    budget: usize,
    cancel: &Cancellation,
) -> Result<(Graph, Report, Vec<Report>)> {
    let n = pool.len();
    let pairs: Vec<_> = (0..n)
        .flat_map(|a| (0..n).filter(move |&b| a != b).map(move |b| (a, b)))
        .collect();
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get().min(4));
    let completed = AtomicUsize::new(0);
    let mut graph: Graph = (0..n * n).map(|_| vec![]).collect();
    let mut diagnostics = vec![];
    std::thread::scope(|scope| -> Result<()> {
        let jobs: Vec<_> = (0..workers)
            .map(|worker| {
                let pairs = &pairs;
                let completed = &completed;
                scope.spawn(move || -> Result<Vec<_>> {
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
            for (index, (edges, diagnostic)) in job
                .join()
                .map_err(|_| Error::new("TACTIC_WORKER", "measurement worker failed"))??
            {
                graph[index] = edges;
                diagnostics.push(diagnostic);
            }
        }
        Ok(())
    })?;
    let mut by_tactic = BTreeMap::<String, usize>::new();
    for edge in graph.iter().flatten() {
        for tactic in read_tactics(&edge.plan)? {
            *by_tactic
                .entry(format!("{:?}", tactic.tactic_id))
                .or_default() += 1;
        }
    }
    eprintln!("stitch: admitted choices by tactic: {by_tactic:?}");
    // Each pair owns detailed window evidence. Keeping every profile in one
    // graph record can exceed the catalog's 8 MiB record limit before search.
    let mut pair_reports = vec![];
    for diagnostic in &mut diagnostics {
        if let Some(details) = diagnostic
            .as_object_mut()
            .and_then(|d| d.remove("window_details"))
        {
            let mut pair = Report::new("measured_pair_windows");
            pair.header
                .extensions
                .insert("from".into(), diagnostic["from"].clone());
            pair.header
                .extensions
                .insert("to".into(), diagnostic["to"].clone());
            pair.header.extensions.insert("windows".into(), details);
            diagnostic["window_report"] =
                serde_json::json!(format!("reports/{}.json", pair.report_id));
            pair_reports.push(pair);
        }
    }
    let mut report = Report::new("measured_graph");
    report
        .header
        .extensions
        .insert("pairs".into(), serde_json::json!(diagnostics));
    report
        .header
        .extensions
        .insert("admitted_by_tactic".into(), serde_json::json!(by_tactic));
    report.counts.insert("cue_budget".into(), budget as u64);
    Ok((graph, report, pair_reports))
}

#[derive(Clone, Copy)]
pub(super) struct Quality {
    pub(super) fallbacks: usize,
    pub(super) weak_links: usize,
    pub(super) dense_quality_seconds: f64,
    pub(super) handoff_quality_seconds: f64,
    pub(super) pacing_penalty: f64,
    pub(super) weakest: f64,
    pub(super) total: f64,
}

impl Quality {
    pub(super) const EMPTY: Self = Self {
        fallbacks: 0,
        weak_links: 0,
        dense_quality_seconds: 0.,
        handoff_quality_seconds: 0.,
        pacing_penalty: 0.,
        weakest: f64::INFINITY,
        total: 0.,
    };

    pub(super) fn extend(
        self,
        score: f64,
        last_resort: bool,
        dense_quality_seconds: f64,
        handoff_quality_seconds: f64,
    ) -> Self {
        Self {
            fallbacks: self.fallbacks + usize::from(last_resort),
            weak_links: self.weak_links + usize::from(score < 25.),
            dense_quality_seconds: self.dense_quality_seconds + dense_quality_seconds,
            handoff_quality_seconds: self.handoff_quality_seconds + handoff_quality_seconds,
            pacing_penalty: self.pacing_penalty,
            weakest: self.weakest.min(score),
            total: self.total + score,
        }
    }

    pub(super) fn paced(mut self, penalty: f64) -> Self {
        self.pacing_penalty += penalty;
        self
    }

    pub(super) fn sustained(self) -> f64 {
        self.dense_quality_seconds + self.handoff_quality_seconds - self.pacing_penalty
    }

    pub(super) fn best_first(self, other: Self) -> std::cmp::Ordering {
        self.fallbacks
            .cmp(&other.fallbacks)
            .then_with(|| self.weak_links.cmp(&other.weak_links))
            .then_with(|| other.sustained().total_cmp(&self.sustained()))
            .then_with(|| other.weakest.total_cmp(&self.weakest))
            .then_with(|| other.total.total_cmp(&self.total))
    }
}

#[derive(Clone, Copy)]
struct Anchor {
    from: usize,
    to: usize,
    edge: usize,
}

impl Anchor {
    fn permits(self, used: u128, last: usize, to: usize, edge: usize, remaining: usize) -> bool {
        if used & (1 << self.to) != 0 {
            return true;
        } // Already traversed the anchor.
        if last == self.from {
            return to == self.to && edge == self.edge;
        }
        if to == self.to || used & (1 << self.from) != 0 {
            return false;
        }
        remaining >= if to == self.from { 1 } else { 2 }
    }
}

pub(super) fn route_quality(plan: &MixPlan) -> Quality {
    plan.header
        .extensions
        .get("chain_joins")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .fold(Quality::EMPTY, |q, j| {
            q.extend(
                j["score"].as_f64().unwrap_or(-1000.),
                j["last_resort"].as_bool().unwrap_or(true),
                j["dense_quality_seconds"].as_f64().unwrap_or(0.),
                j["handoff_quality_seconds"].as_f64().unwrap_or(0.),
            )
            .paced(
                j["pacing"]["penalty_quality_seconds"]
                    .as_f64()
                    .unwrap_or(0.),
            )
        })
}

/// Explain the first lexicographic difference, using the same quality fields
/// as search. A great anchor can lose because another join needs a fallback.
pub(super) fn route_comparisons(plan: &MixPlan) -> serde_json::Value {
    let winner = route_quality(plan);
    let trials = plan
        .header
        .extensions
        .get("dense_anchor_search")
        .and_then(|v| v.get("trials"))
        .and_then(serde_json::Value::as_array);
    serde_json::json!(trials.into_iter().flatten().map(|trial| {
        let route = &trial["route"];
        let (result, reason) = if route.is_null() {
            ("incomplete", trial["failure"].as_str().unwrap_or("No complete route in the measured search budget").to_owned())
        } else {
            let candidate = Quality {
                fallbacks: route["fallbacks"].as_u64().unwrap_or(0) as usize,
                weak_links: route["weak_links"].as_u64().unwrap_or(0) as usize,
                dense_quality_seconds: route["dense_quality_seconds"].as_f64().unwrap_or(0.),
                handoff_quality_seconds: route["handoff_quality_seconds"].as_f64().unwrap_or(0.),
                pacing_penalty: route["pacing_penalty"].as_f64().unwrap_or(0.),
                weakest: route["weakest"].as_f64().unwrap_or(0.),
                total: route["total"].as_f64().unwrap_or(0.),
            };
            let reason = quality_difference(candidate, winner);
            (if candidate.best_first(winner).is_eq() {"equal objective"} else {"lost"}, reason)
        };
        serde_json::json!({"anchor":trial["anchor"],"result":result,"reason":reason,"route":route,
            "problem_joins":route["joins"].as_array().into_iter().flatten().filter(|j| j["last_resort"].as_bool().unwrap_or(false) || j["score"].as_f64().unwrap_or(0.) < 25.).collect::<Vec<_>>()})
    }).collect::<Vec<_>>())
}

fn quality_difference(candidate: Quality, winner: Quality) -> String {
    if candidate.fallbacks != winner.fallbacks {
        return format!("Fallback joins: {} versus selected {}. Sustained quality {:.2} versus {:.2} is considered only after fallback and weak-join counts.", candidate.fallbacks,winner.fallbacks,candidate.dense_quality_seconds+candidate.handoff_quality_seconds,winner.dense_quality_seconds+winner.handoff_quality_seconds);
    }
    if candidate.weak_links != winner.weak_links {
        return format!(
            "Joins below score 25: {} versus selected {}",
            candidate.weak_links, winner.weak_links
        );
    }
    let held = candidate.sustained();
    let selected = winner.sustained();
    if held.total_cmp(&selected).is_ne() {
        return format!(
            "Quality-weighted sustained seconds after pacing penalty: {held:.2} versus selected {selected:.2}"
        );
    }
    if candidate.weakest.total_cmp(&winner.weakest).is_ne() {
        return format!(
            "Weakest join score: {:.3} versus selected {:.3}",
            candidate.weakest, winner.weakest
        );
    }
    if candidate.total.total_cmp(&winner.total).is_ne() {
        return format!(
            "Total fit: {:.3} versus selected {:.3}",
            candidate.total, winner.total
        );
    }
    "Matches the selected objective; equal routes retain deterministic search order".into()
}

struct Route {
    used: u128,
    last: usize,
    entry_end: u64,
    quality: Quality,
    plan: Option<MixPlan>,
}

struct Extension {
    prefix: usize,
    to: usize,
    edge: usize,
    quality: Quality,
    region: (usize, u64),
    pacing: PacingDecision,
}

/// An optimistic reachability bound. Ignoring the no-repeat rule can only
/// keep extra states. Every actual extension still checks the used tracks.
fn runway_bounds(graph: &Graph, pacing: &[TrackPacing], count: usize) -> Vec<Vec<Option<u64>>> {
    let n = pacing.len();
    let mut bounds = vec![vec![Some(u64::MAX); n]];
    for remaining in 1..count {
        let previous = &bounds[remaining - 1];
        let layer = (0..n)
            .map(|from| {
                (0..n)
                    .flat_map(|to| {
                        graph[from * n + to].iter().filter(move |edge| {
                            previous[to].is_some_and(|latest| {
                                pacing[to].earliest_exit(edge.entry_end) <= latest
                            })
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

/// At equal fallback cost, the earliest reachable entry into each unused track
/// dominates later entries for timing feasibility. Preserve both costs so an
/// early fallback cannot erase a later clean entry needed to finish the set.
struct Runway<'a> {
    graph: &'a Graph,
    pacing: &'a [TrackPacing],
    anchor: Option<Anchor>,
    memo: BTreeMap<(u128, usize, usize, usize), ExitBounds>,
}

impl Runway<'_> {
    fn can_finish(
        &mut self,
        used: u128,
        last: usize,
        exit: u64,
        remaining: usize,
        fallbacks: usize,
    ) -> bool {
        if remaining == 0 {
            return self.anchor.is_none_or(|a| used & (1 << a.to) != 0);
        }
        let key = (used, last, remaining, fallbacks);
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
        let n = self.pacing.len();
        let possible = (0..n).any(|to| {
            used & (1 << to) == 0
                && [false, true].into_iter().any(|fallback| {
                    let cost = usize::from(fallback);
                    cost <= fallbacks
                        && self.graph[last * n + to]
                            .iter()
                            .enumerate()
                            .filter(|(index, edge)| {
                                self.pacing[last].assess(exit, edge.exit).is_some()
                                    && edge.last_resort == fallback
                                    && self.anchor.is_none_or(|a| {
                                        a.permits(used, last, to, *index, remaining - 1)
                                    })
                            })
                            .map(|(_, edge)| edge.entry_end)
                            .min()
                            .is_some_and(|next| {
                                self.can_finish(
                                    used | (1 << to),
                                    to,
                                    next,
                                    remaining - 1,
                                    fallbacks - cost,
                                )
                            })
                })
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

pub(super) fn route_local(error: &Error) -> bool {
    matches!(
        error.code.as_str(),
        "CHAIN_CUES"
            | "CHAIN_PACING"
            | "TIME_MAP_SPEED"
            | "TIME_MAP_ACCELERATION"
            | "TIME_MAP"
            | "TACTIC_PLAN"
            | "TACTIC_VOCALS"
            | "TACTIC_TAIL"
            | "TACTIC_STEM_TIMEBASE"
    ) || error.code.starts_with("TACTIC_")
}

#[allow(clippy::too_many_arguments)]
fn search(
    store: &Store,
    graph: &Graph,
    pool: &[Track],
    pacing: &[TrackPacing],
    count: usize,
    width: usize,
    max_fallbacks: usize,
    anchor: Option<Anchor>,
    cancel: &Cancellation,
) -> Result<Option<MixPlan>> {
    // Future fallback cost is part of feasibility, not merely the score of
    // joins already played. Exhaust clean routes before opening escape hatches.
    for fallbacks in 0..=max_fallbacks {
        for beam_width in [width, width.saturating_mul(4)] {
            if let Some(mut plan) = search_with_fallback_budget(
                store, graph, pool, pacing, count, beam_width, fallbacks, anchor, cancel,
            )? {
                plan.header.extensions.insert(
                    "search_bounds".into(),
                    serde_json::json!({"beam_width":beam_width,"fallback_budget":fallbacks}),
                );
                return Ok(Some(plan));
            }
        }
    }
    Ok(None)
}

#[allow(clippy::too_many_arguments)]
fn search_with_fallback_budget(
    store: &Store,
    graph: &Graph,
    pool: &[Track],
    pacing: &[TrackPacing],
    count: usize,
    width: usize,
    max_fallbacks: usize,
    anchor: Option<Anchor>,
    cancel: &Cancellation,
) -> Result<Option<MixPlan>> {
    let n = pool.len();
    let bounds = runway_bounds(graph, pacing, count);
    let mut runway = Runway {
        graph,
        pacing,
        anchor: if n <= 16 { anchor } else { None },
        memo: BTreeMap::new(),
    };
    let mut beam: Vec<_> = (0..n)
        .filter(|last| anchor.is_none_or(|a| *last != a.to))
        .map(|last| Route {
            used: 1 << last,
            last,
            entry_end: 0,
            quality: Quality::EMPTY,
            plan: None,
        })
        .collect();
    if n <= 16
        && !beam
            .iter()
            .any(|r| runway.can_finish(r.used, r.last, 0, count - 1, max_fallbacks))
    {
        eprintln!("stitch: measured cues have no distinct-track route with enough solo time within the last-resort budget");
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
                    if let Some(a) = anchor {
                        if !a.permits(route.used, route.last, to, edge, remaining)
                            || route.used & (1 << a.to) == 0
                                && to == a.from
                                && pacing[to].earliest_exit(choice.entry_end)
                                    > graph[a.from * n + a.to][a.edge].exit
                        {
                            continue;
                        }
                    }
                    let Some(pacing_decision) =
                        pacing[route.last].assess(route.entry_end, choice.exit)
                    else {
                        continue;
                    };
                    let quality = route
                        .quality
                        .extend(
                            choice.score,
                            choice.last_resort,
                            choice.dense_quality_seconds,
                            choice.handoff_quality_seconds,
                        )
                        .paced(pacing_decision.penalty_quality_seconds);
                    if pacing[to].earliest_exit(choice.entry_end) > latest
                        || quality.fallbacks > max_fallbacks
                    {
                        continue;
                    }
                    extensions.push(Extension {
                        prefix,
                        to,
                        edge,
                        quality,
                        pacing: pacing_decision,
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
            let entry_end = edge.entry_end;
            let lookahead = if n <= 16 { remaining } else { remaining.min(3) };
            if !runway.can_finish(
                used,
                choice.to,
                entry_end,
                lookahead,
                max_fallbacks - choice.quality.fallbacks,
            ) {
                return Ok(None);
            }
            let mut plan = match chain::extend_measured(
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
            if let Some(join) = plan
                .header
                .extensions
                .get_mut("chain_joins")
                .and_then(serde_json::Value::as_array_mut)
                .and_then(|j| j.last_mut())
            {
                join["pacing"] = serde_json::to_value(&choice.pacing)?;
            }
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
                entry_end,
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

/// Assemble only already measured choices. Both eager selection and lazy
/// discovery share anchor ranking, phrase pacing, route constraints and score.
#[allow(clippy::too_many_arguments)]
pub(super) fn assemble_measured(
    store: &Store,
    graph: &Graph,
    pool: &[Track],
    pacing: &[TrackPacing],
    policy: &MixingConfig,
    count: usize,
    max_fallbacks: usize,
    anchor_slots: usize,
    deadline: Option<std::time::Instant>,
    cancel: &Cancellation,
) -> Result<Option<MixPlan>> {
    let n = pool.len();
    // Keep anchor opportunities from both successful musical patterns.
    // Neither plentiful dense mashups nor long sparse handoffs can evict
    // the other family before full-route comparison.
    let anchors_for = |handoff: bool| {
        let mut anchors: Vec<_> = graph
            .iter()
            .enumerate()
            .flat_map(|(pair, edges)| {
                edges
                    .iter()
                    .enumerate()
                    .filter(move |(_, e)| {
                        if handoff {
                            e.handoff_quality_seconds > 0.
                        } else {
                            e.dense_quality_seconds > 0.
                        }
                    })
                    .map(move |(edge, _)| Anchor {
                        from: pair / n,
                        to: pair % n,
                        edge,
                    })
            })
            .collect();
        anchors.sort_by(|a, b| {
            let value = |a: &Anchor| {
                let e = &graph[a.from * n + a.to][a.edge];
                if handoff {
                    e.handoff_quality_seconds
                } else {
                    e.dense_quality_seconds
                }
            };
            value(b).total_cmp(&value(a))
        });
        anchors.truncate(anchor_slots);
        anchors
    };
    let mut anchors = anchors_for(false);
    anchors.extend(anchors_for(true));
    let mut trials = vec![];
    {
        let width = policy.beam_width;
        let mut best: Option<MixPlan> = None;

        for anchor in anchors
            .iter()
            .copied()
            .map(Some)
            .chain(std::iter::once(None))
        {
            if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
                break;
            }
            let candidate = search(
                store,
                graph,
                pool,
                pacing,
                count,
                width,
                max_fallbacks,
                anchor,
                cancel,
            )?;
            let effective_width = candidate
                .as_ref()
                .and_then(|p| p.header.extensions.get("search_bounds"))
                .and_then(|v| v.get("beam_width"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(width.saturating_mul(4) as u64);
            let edge = anchor.map(|a| &graph[a.from * pool.len() + a.to][a.edge]);
            trials.push(serde_json::json!({
                    "anchor":edge.map(|e|serde_json::json!({"from":e.plan.placements[0].track_id,"to":e.plan.placements[1].track_id,
                        "cue":e.plan.header.extensions.get("tactic_cue_candidate"),"score":e.score,"dense_quality_seconds":e.dense_quality_seconds,"handoff_quality_seconds":e.handoff_quality_seconds})),
                    "beam_width":effective_width,"complete":candidate.is_some(),
                    "route":candidate.as_ref().map(|p|serde_json::json!({"tracks":p.placements.iter().map(|p|&p.track_id).collect::<Vec<_>>(),
                        "joins":p.header.extensions.get("chain_joins"),
                        "fallbacks":route_quality(p).fallbacks,"weak_links":route_quality(p).weak_links,
                        "weakest":route_quality(p).weakest,"total":route_quality(p).total,"dense_quality_seconds":route_quality(p).dense_quality_seconds,"handoff_quality_seconds":route_quality(p).handoff_quality_seconds,"pacing_penalty":route_quality(p).pacing_penalty})),
                    "failure":if candidate.is_none(){Some("no complete route within this anchor, cue, solo-runway and beam budget")}else{None::<&str>}
                }));
            if let Some(plan) = candidate {
                if best
                    .as_ref()
                    .is_none_or(|old| route_quality(&plan).best_first(route_quality(old)).is_lt())
                {
                    best = Some(plan);
                }
            }
            eprintln!("stitch: tested {} sustained-anchor routes", trials.len());
        }
        let Some(mut plan) = best else {
            return Ok(None);
        };

        plan.planner_version = "phrase-paced-anchor-beam/6".into();
        plan.header.extensions.insert("dense_anchor_search".into(),serde_json::json!({"trials":trials,
                "objective":"fewest fallbacks, fewest joins below score 25, most quality-weighted dense mashup plus stable handoff seconds minus pacing penalty, strongest weakest join, total fit",
                "anchor_limit":anchor_slots*2,"selected_dense_quality_seconds":route_quality(&plan).dense_quality_seconds,"selected_handoff_quality_seconds":route_quality(&plan).handoff_quality_seconds,"selected_pacing_penalty":route_quality(&plan).pacing_penalty,"anchor_slots_per_family":anchor_slots}));
        Ok(Some(plan))
    }
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
    let pacing = pool
        .iter()
        .map(|track| mixlib_planner::read_track_pacing(&snapshot, &track.track_id, cancel))
        .collect::<Result<Vec<_>>>()?;
    // A set assembled almost entirely from escape hatches is not a successful
    // continuous mix. Widen cue search before accepting repeated fallbacks.
    let max_fallbacks = policy
        .header
        .extensions
        .get("max_last_resort_transitions")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1)
        .min((count - 1) as u64) as usize;
    for budget in [24, 64] {
        eprintln!(
            "stitch: building measured choices from {} tracks, up to {budget} base seeds, {} return probes and {budget} event-ending windows per pair",
            pool.len(), (budget/3).max(1)
        );
        let (graph, report, pair_reports) =
            measure_graph(&snapshot, &pool, policy, context, budget, cancel)?;
        snapshot.verify_read_snapshot(cancel)?;
        for pair in pair_reports {
            store.report(&pair)?;
        }
        store.report(&report)?;
        eprintln!(
            "stitch: cue and tactic diagnostics: reports/{}.json",
            report.report_id
        );
        let edges: usize = graph.iter().map(Vec::len).sum();
        eprintln!("stitch: {edges} admitted cue/tactic choices");
        let Some(mut plan) = assemble_measured(
            &snapshot,
            &graph,
            &pool,
            &pacing,
            policy,
            count,
            max_fallbacks,
            12,
            None,
            cancel,
        )?
        else {
            continue;
        };
        snapshot.verify_read_snapshot(cancel)?;
        plan.header.extensions.insert("library_pick".into(), serde_json::json!({
                "analyzed_tracks": pool.len(), "measured_choices": edges, "cue_budget_per_pair": budget*2+(budget/3).max(1),
                "return_probe_budget_per_pair":(budget/3).max(1),
                "seed_cue_budget_per_pair":budget,"continuation_budget_per_pair":budget,
                "beam_width": plan.header.extensions.get("search_bounds").and_then(|v|v.get("beam_width")), "track_ids": plan.placements.iter().map(|p| &p.track_id).collect::<Vec<_>>(),
                "max_last_resort_transitions": max_fallbacks,
            }));
        return Ok(plan);
    }
    snapshot.verify_read_snapshot(cancel)?;
    Err(Error::new(
        "TACTIC_NO_COMPLETE_ROUTE",
        format!(
            "measured beam found no complete {count}-track route within the limit of {max_fallbacks} last-resort transitions from {} prepared recordings",
            pool.len()
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn an_anchor_that_cannot_open_a_set_can_finish_one() {
        let plan: MixPlan =
            serde_json::from_str(include_str!("../../../examples/mix-plan.json")).unwrap();
        let edge = |exit, entry_end, value| MeasuredEdge {
            plan: plan.clone(),
            exit,
            entry_end,
            score: 50.,
            last_resort: false,
            dense_quality_seconds: value,
            handoff_quality_seconds: 0.,
        };
        let mut graph: Graph = (0..9).map(|_| vec![]).collect();
        graph[1].push(edge(10, 20, 0.));
        graph[5].push(edge(70, 20, 20.));
        let pacing = vec![TrackPacing::unknown(1); 3];
        let mut runway = Runway {
            graph: &graph,
            pacing: &pacing,
            anchor: Some(Anchor {
                from: 1,
                to: 2,
                edge: 0,
            }),
            memo: BTreeMap::new(),
        };
        assert!(!runway.can_finish(2, 1, 0, 2, 0));
        assert!(runway.can_finish(1, 0, 0, 2, 0));
    }

    #[test]
    fn reachability_uses_measured_phrases_instead_of_reapplying_a_time_minimum() {
        let events: Vec<_> = (0..=128)
            .map(|i| Beat {
                event_id: format!("b{i}"),
                source_frame: 100 + i * 100,
                score: 1.,
                local_bpm: Some(120.),
            })
            .collect();
        let beats = Beats {
            header: Header::default(),
            beats_id: id(),
            source_sample_rate_hz: 200,
            source_frames: 13000,
            downbeat_ids: events
                .iter()
                .step_by(4)
                .map(|e| e.event_id.clone())
                .collect(),
            events,
            meter_segments: vec![],
            unreliable_intervals: vec![],
            score_semantics: "fixture".into(),
        };
        let frames: Vec<_> = (0..32)
            .map(|i| {
                let level = if (i / 4) % 2 == 0 { 0.1 } else { 0.8 };
                FeatureFrame {
                    source_frame: 100 + i * 400,
                    chroma: [1.; 12],
                    band_energy: [level * level; 3],
                    rms: level,
                    peak: level,
                    onset: level,
                }
            })
            .collect();
        let timing = mixlib_planner::TrackPacing::from_features(&beats, &frames);
        assert_eq!(timing.assess(1700, 6500).unwrap().complete_bars, Some(12));
        let plan: MixPlan =
            serde_json::from_str(include_str!("../../../examples/mix-plan.json")).unwrap();
        let edge = |exit, entry_end| MeasuredEdge {
            plan: plan.clone(),
            exit,
            entry_end,
            score: 50.,
            last_resort: false,
            dense_quality_seconds: 5.,
            handoff_quality_seconds: 0.,
        };
        let mut graph: Graph = (0..9).map(|_| vec![]).collect();
        graph[1] = vec![edge(100, 1700)];
        graph[5] = vec![edge(6500, 1700)];
        let policies = vec![timing; 3];
        let mut runway = Runway {
            graph: &graph,
            pacing: &policies,
            anchor: None,
            memo: BTreeMap::new(),
        };
        assert!(runway.can_finish(1, 0, 0, 2, 0));
        let unknown = vec![TrackPacing::unknown(200); 3];
        let mut conservative = Runway {
            graph: &graph,
            pacing: &unknown,
            anchor: None,
            memo: BTreeMap::new(),
        };
        assert!(!conservative.can_finish(1, 0, 0, 2, 0));
    }

    #[test]
    fn pacing_is_a_bounded_preference_not_a_replacement_for_tactic_quality() {
        let exceptional = Quality::EMPTY.extend(50., false, 20., 0.).paced(2.25);
        let ordinary = Quality::EMPTY.extend(35., false, 8., 0.);
        assert!(exceptional.best_first(ordinary).is_lt());
        assert!(ordinary.best_first(ordinary.paced(0.75)).is_lt());
        let fallback = exceptional.extend(100., true, 100., 0.);
        assert!(ordinary.best_first(fallback).is_lt());
    }

    #[test]
    fn evidence_explains_fallback_before_better_sustained_quality() {
        let winner = Quality::EMPTY.extend(35., false, 10., 0.);
        let candidate = Quality::EMPTY
            .extend(50., false, 40., 0.)
            .extend(-120., true, 0., 0.);
        assert!(winner.best_first(candidate).is_lt());
        let reason = quality_difference(candidate, winner);
        assert!(reason.starts_with("Fallback joins: 1 versus selected 0"));
        assert!(reason.contains("40.00 versus 10.00"));
        let weaker = Quality::EMPTY.extend(24., false, 50., 0.);
        assert!(quality_difference(weaker, winner).starts_with("Joins below score 25"));
    }

    #[test]
    fn reachability_keeps_clean_entries_and_rejects_chains_of_fallbacks() {
        let plan: MixPlan =
            serde_json::from_str(include_str!("../../../examples/mix-plan.json")).unwrap();
        let edge = |exit, entry_end, last_resort| MeasuredEdge {
            plan: plan.clone(),
            exit,
            entry_end,
            score: 50.,
            last_resort,
            dense_quality_seconds: 0.,
            handoff_quality_seconds: 0.,
        };
        let mut graph: Graph = (0..9).map(|_| vec![]).collect();
        graph[1] = vec![edge(30, 100, true), edge(30, 150, false)];
        graph[5] = vec![edge(210, 20, true)];
        let mut runway = Runway {
            graph: &graph,
            pacing: &vec![TrackPacing::unknown(1); 3],
            anchor: None,
            memo: BTreeMap::new(),
        };
        assert!(runway.can_finish(1, 0, 0, 2, 1));
        let mut anchored = Runway {
            graph: &graph,
            pacing: &vec![TrackPacing::unknown(1); 3],
            anchor: Some(Anchor {
                from: 0,
                to: 1,
                edge: 1,
            }),
            memo: BTreeMap::new(),
        };
        assert!(anchored.can_finish(1, 0, 0, 2, 1));
        assert!(
            !anchored.can_finish(1, 0, 31, 2, 1),
            "the required anchor cannot be replaced after its cue has passed"
        );
        assert!(!runway.can_finish(1, 0, 0, 2, 0));
        // A prefix can be musically attractive yet need a future fallback;
        // the clean pass must reject it before score-based beam truncation.
        graph[1].pop();
        let mut runway = Runway {
            graph: &graph,
            pacing: &vec![TrackPacing::unknown(1); 3],
            anchor: None,
            memo: BTreeMap::new(),
        };
        assert!(!runway.can_finish(1, 0, 0, 2, 1));
        assert!(runway.can_finish(1, 0, 0, 2, 2));
    }

    #[test]
    fn fallback_free_routes_outrank_even_high_scoring_cut_chains() {
        let mut cuts = Quality::EMPTY;
        let mut blends = Quality::EMPTY;
        for _ in 0..7 {
            cuts = cuts.extend(100., true, 50., 0.);
            blends = blends.extend(15., false, 0., 0.);
        }
        assert!(blends.best_first(cuts).is_lt());
        assert_eq!(cuts.fallbacks, 7);
    }

    #[test]
    fn anchor_cannot_be_bypassed_or_consumed_in_reverse_order() {
        let a = Anchor {
            from: 1,
            to: 2,
            edge: 3,
        };
        assert!(!a.permits(1, 0, 2, 0, 4));
        assert!(!a.permits(2, 1, 2, 0, 4));
        assert!(a.permits(2, 1, 2, 3, 0));
        assert!(!a.permits(1, 0, 1, 0, 0));
        assert!(a.permits(1, 0, 1, 0, 1));
        assert!(a.permits(6, 2, 0, 0, 0));
    }

    #[test]
    fn stable_handoffs_can_anchor_a_set_without_erasing_dense_value() {
        let baseline = Quality::EMPTY.extend(50., false, 10., 0.);
        let with_handoff = baseline.extend(45., false, 0., 8.);
        let ordinary = baseline.extend(60., false, 0., 0.);
        assert!(with_handoff.best_first(ordinary).is_lt());
        let weak_handoff = baseline.extend(15., false, 0., 80.);
        assert!(ordinary.best_first(weak_handoff).is_lt());
        assert_eq!(with_handoff.dense_quality_seconds, 10.);
        assert_eq!(with_handoff.handoff_quality_seconds, 8.);
    }
    #[test]
    fn sustained_quality_defines_the_set_but_cannot_buy_weak_bridges() {
        let brief = Quality::EMPTY
            .extend(45., false, 10., 0.)
            .extend(32., false, 0., 0.);
        let long = Quality::EMPTY
            .extend(54., false, 20., 0.)
            .extend(30., false, 0., 0.);
        assert!(long.best_first(brief).is_lt());
        let weak = Quality::EMPTY
            .extend(54., false, 40., 0.)
            .extend(15., false, 0., 0.);
        assert!(brief.best_first(weak).is_lt());
    }

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
            dense_quality_seconds: 0.,
            handoff_quality_seconds: 0.,
        };
        let mut graph: Graph = (0..9).map(|_| vec![]).collect();
        graph[1] = vec![edge(30, 150, 80.), edge(30, 100, 60.)];
        graph[5] = vec![edge(170, 20, 75.)];
        let bounds = runway_bounds(&graph, &vec![TrackPacing::unknown(1); 3], 3);
        let latest = bounds[1][1].unwrap();
        let usable = graph[1].iter().filter(|e| e.entry_end + 45 <= latest);
        assert_eq!(usable.map(|e| e.score).collect::<Vec<_>>(), [60.]);
        assert_eq!(bounds[2][0], Some(30));
        let mut runway = Runway {
            graph: &graph,
            pacing: &vec![TrackPacing::unknown(1); 3],
            anchor: None,
            memo: BTreeMap::new(),
        };
        assert!(!runway.can_finish(0b011, 1, 150, 1, 0));
        assert!(runway.can_finish(0b011, 1, 100, 1, 0));
        assert!(!runway.can_finish(0b111, 1, 100, 1, 0));
    }
}
