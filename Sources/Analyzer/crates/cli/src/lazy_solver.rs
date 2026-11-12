//! Anytime search over cue families. Cheap reachability precedes PCM work;
//! measurement and tactic admission remain owned by tactics::measure_window.
use super::*;
use crate::measured_beam::{route_local, route_quality, Quality};
use crate::tactics::{CandidateQueue, MeasuredEdge};
use mixlib_planner::{CandidateEvidenceCache, StitchRequest, TrackPacing};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BinaryHeap},
    sync::Arc,
    time::Instant,
};

struct Window {
    plan: MixPlan,
    measured: Option<Vec<MeasuredEdge>>,
}
struct Family {
    pair: usize,
    exit: u64,
    entry_end: u64,
    upper: f64,
    windows: Vec<Window>,
}
struct Pair {
    from: usize,
    to: usize,
    expansion: CandidateQueue,
    admitted: BTreeMap<String, usize>,
    rejected: BTreeMap<String, usize>,
    details: Vec<Value>,
}
#[derive(Clone, Copy)]
struct Probe {
    from: usize,
    to: usize,
    exit: u64,
    entry_end: u64,
}
struct Route {
    used: u128,
    last: usize,
    entry_end: u64,
    length: usize,
    quality: Quality,
    plan: Option<MixPlan>,
}
struct Work {
    parent: Arc<Route>,
    family: usize,
    window: usize,
    priority: f64,
    serial: u64,
    discovery: bool,
}
impl PartialEq for Work {
    fn eq(&self, other: &Self) -> bool {
        self.serial == other.serial
    }
}
impl Eq for Work {}
impl PartialOrd for Work {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Work {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .parent
            .quality
            .fallbacks
            .cmp(&self.parent.quality.fallbacks)
            .then_with(|| self.priority.total_cmp(&other.priority))
            .then_with(|| other.serial.cmp(&self.serial))
    }
}
#[derive(Default, serde::Serialize)]
struct Stats {
    proposed_families: usize,
    discovered_windows: usize,
    measured_windows: usize,
    cached_window_uses: usize,
    work_items: usize,
    cheap_rejections: usize,
    bound_rejections: usize,
    assembly_rejections: usize,
    complete_candidates: usize,
    frontier_dropped: usize,
    improvements: usize,
    measured_anchor_starts: usize,
    discovery_measurements: usize,
    graph_assemblies: usize,
}

/// Earlier entry dominates later entry for pacing feasibility. These probes
/// keep the shortest seed end even when its tactics fail: longer continuations
/// cannot enter earlier. That deliberately optimistic graph never authorizes audio.
struct Reachability<'a> {
    probes: &'a [Probe],
    pacing: &'a [TrackPacing],
    memo: BTreeMap<(u128, usize, u64, usize), bool>,
}
impl Reachability<'_> {
    fn can_finish(&mut self, used: u128, last: usize, entry: u64, left: usize) -> bool {
        if left == 0 {
            return true;
        }
        if self.pacing.len() - (used.count_ones() as usize) < left {
            return false;
        }
        let key = (used, last, entry, left);
        if let Some(answer) = self.memo.get(&key) {
            return *answer;
        }
        let mut earliest = BTreeMap::<usize, u64>::new();
        for p in self
            .probes
            .iter()
            .filter(|p| p.from == last && used & (1 << p.to) == 0)
        {
            if self.pacing[last].assess(entry, p.exit).is_some() {
                earliest
                    .entry(p.to)
                    .and_modify(|end| *end = (*end).min(p.entry_end))
                    .or_insert(p.entry_end);
            }
        }
        let answer = earliest
            .into_iter()
            .any(|(to, end)| self.can_finish(used | (1 << to), to, end, left - 1));
        if self.memo.len() >= 100_000 {
            self.memo.clear();
        }
        self.memo.insert(key, answer);
        answer
    }
}

fn optimistic(mut quality: Quality, remaining: usize, per_join_upper: f64) -> Quality {
    quality.dense_quality_seconds += remaining as f64 * per_join_upper;
    if remaining > 0 {
        quality.total = f64::INFINITY;
    }
    quality
}
fn can_beat(upper: Quality, incumbent: Option<Quality>) -> bool {
    incumbent.is_none_or(|q| upper.best_first(q).is_lt())
}
fn coordinates(plan: &MixPlan) -> (u64, u64) {
    let t = &plan.transitions[0];
    (
        plan.placements[0]
            .time_map
            .source_at(t.start_output_frame as f64)
            .round() as u64,
        plan.placements[1]
            .time_map
            .source_at(t.end_output_frame as f64)
            .round() as u64,
    )
}
fn duration(plan: &MixPlan) -> f64 {
    (plan.transitions[0].end_output_frame - plan.transitions[0].start_output_frame) as f64
        / f64::from(plan.output.sample_rate_hz)
}

fn propose(
    store: &Store,
    pool: &[Track],
    policy: &MixingConfig,
    context: usize,
    required: &crate::required_pairs::RequiredPairs,
    cancel: &Cancellation,
) -> Result<(Vec<Pair>, Vec<Family>)> {
    let jobs: Vec<_> = (0..pool.len())
        .flat_map(|a| {
            (0..pool.len())
                .filter(move |&b| a != b && required.permits_edge(a, b))
                .map(move |b| (a, b))
        })
        .collect();
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get().min(4));
    let proposed = std::thread::scope(|scope| -> Result<Vec<_>> {
        let handles: Vec<_> = (0..workers)
            .map(|worker| {
                let jobs = &jobs;
                scope.spawn(move || -> Result<Vec<_>> {
                    let mut results = vec![];
                    for &(from, to) in jobs.iter().skip(worker).step_by(workers) {
                        cancel.check()?;
                        let request = StitchRequest {
                            from: &pool[from].track_id,
                            to: &pool[to].track_id,
                            overlap_beats: 8,
                            context_beats: context,
                            outgoing_after_frame: 0,
                        };
                        let mut seeds = match mixlib_planner::tactic_stitch_candidates(
                            store, &request, policy, 24, cancel,
                        ) {
                            Ok(seeds) => seeds,
                            Err(e)
                                if matches!(
                                    e.code.as_str(),
                                    "STITCH_INFEASIBLE" | "STITCH_EXCLUDED"
                                ) =>
                            {
                                vec![]
                            }
                            Err(e) => return Err(e),
                        };
                        if crate::composition::Request::from_policy(policy)?.is_some() {
                            // Protect later exits before global cue ranking can discard them.
                            for fraction in [0.35, 0.60] {
                                let later = StitchRequest {
                                    outgoing_after_frame: (pool[from].format.frames as f64
                                        * fraction)
                                        as u64,
                                    ..request
                                };
                                match mixlib_planner::tactic_stitch_candidates(
                                    store, &later, policy, 12, cancel,
                                ) {
                                    Ok(extra) => seeds.extend(extra),
                                    Err(e)
                                        if matches!(
                                            e.code.as_str(),
                                            "STITCH_INFEASIBLE" | "STITCH_EXCLUDED"
                                        ) => {}
                                    Err(e) => return Err(e),
                                }
                            }
                            let mut seen = std::collections::BTreeSet::new();
                            seeds.retain(|p| seen.insert(coordinates(p)));
                        }
                        results.push((from, to, seeds));
                    }
                    Ok(results)
                })
            })
            .collect();
        let mut results = vec![];
        for handle in handles {
            results.extend(
                handle
                    .join()
                    .map_err(|_| Error::new("LAZY_WORKER", "cue proposal worker failed"))??,
            );
        }
        Ok(results)
    })?;
    let mut proposed = proposed;
    proposed.sort_by_key(|(a, b, _)| (*a, *b));
    let mut pairs = vec![];
    let mut families = vec![];
    for (from, to, seeds) in proposed {
        let pair = pairs.len();
        pairs.push(Pair {
            from,
            to,
            expansion: CandidateQueue::expansion_only(&seeds, 24),
            admitted: BTreeMap::new(),
            rejected: BTreeMap::new(),
            details: vec![],
        });
        for seed in seeds {
            let (exit, entry_end) = coordinates(&seed);
            let upper = duration(&seed).max(mixlib_planner::MAX_TACTIC_CONTINUATION_SECONDS);
            families.push(Family {
                pair,
                exit,
                entry_end,
                upper,
                windows: vec![Window {
                    plan: seed,
                    measured: None,
                }],
            });
        }
    }
    Ok((pairs, families))
}

fn priority(parent: &Route, family: &Family, window: usize, refining: bool) -> f64 {
    let plan = &family.windows[window].plan;
    let cost = family.windows[0]
        .plan
        .header
        .extensions
        .get("tactic_cue_candidate")
        .and_then(|v| v.get("proposal_cost"))
        .and_then(Value::as_f64)
        .unwrap_or(1.);
    let cue = plan.header.extensions.get("tactic_cue_candidate");
    let bars = cue
        .and_then(|c| c.get("overlap_beats"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        / family.windows[0]
            .plan
            .header
            .extensions
            .get("tactic_cue_candidate")
            .and_then(|c| c.get("bar_beats"))
            .and_then(Value::as_u64)
            .unwrap_or(4)
            .max(1);
    let returning = cue
        .and_then(|c| c.get("entry_return_probe"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let known = family
        .windows
        .iter()
        .filter_map(|w| w.measured.as_ref())
        .flatten()
        .map(|e| e.dense_quality_seconds + e.handoff_quality_seconds)
        .fold(0., f64::max);
    parent.quality.sustained()
        + (if refining { 2. } else { 8. }) * parent.length as f64
        + opportunity_priority(cost, bars as usize, returning, window > 0, 0, known)
        - 12. * parent.quality.weak_links as f64
}

/// Discovery has its own budget lane: a good transition may belong at the
/// end or middle, even when it cannot start a complete set. These estimates
/// order measurements; only measured tactic evidence admits a join.
struct Discovery {
    required_bonus: Vec<f64>,
    duration_bonus: BTreeMap<usize, (f64, usize)>,
    temporal: bool,
    regions: BTreeMap<(usize, u64), usize>,
    counts: Vec<usize>,
    best: Vec<f64>,
    bridge_bonus: Vec<f64>,
    bridge_at: Vec<usize>,
}
fn opportunity_priority(
    cost: f64,
    bars: usize,
    returning: bool,
    continuation: bool,
    pair_count: usize,
    measured_value: f64,
) -> f64 {
    let hold = match bars {
        8..=16 => 4.,
        4..=7 => 1.,
        _ => 0.,
    };
    hold + if returning { 2. } else { 0. } + if continuation { 8. } else { 0. }
        - 8. * cost
        - 1.5 * pair_count as f64
        + 0.3 * measured_value
}
fn bridge_value(anchor_value: f64, weak: bool, held: f64) -> f64 {
    if anchor_value <= 0. {
        0.
    } else if weak {
        6. + 0.3 * anchor_value
    } else if held <= 0. {
        0.1 * anchor_value
    } else {
        0.
    }
}
impl Discovery {
    /// Ask which links stop the measured anchors from winning. This uses this
    /// run's own trials, never a supplied route or a previous winner's cues.
    fn learn_bridges(&mut self, plan: &MixPlan, pool: &[Track], pairs: &[Pair]) {
        self.bridge_bonus.fill(0.);
        self.bridge_at.clone_from(&self.counts);
        for trial in plan
            .header
            .extensions
            .get("dense_anchor_search")
            .and_then(|s| s.get("trials"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let anchor = &trial["anchor"];
            let value = anchor["dense_quality_seconds"].as_f64().unwrap_or(0.)
                + anchor["handoff_quality_seconds"].as_f64().unwrap_or(0.);
            for join in trial["route"]["joins"].as_array().into_iter().flatten() {
                let weak = join["last_resort"].as_bool().unwrap_or(true)
                    || join["score"].as_f64().unwrap_or(-1000.) < 25.;
                let held = join["dense_quality_seconds"].as_f64().unwrap_or(0.)
                    + join["handoff_quality_seconds"].as_f64().unwrap_or(0.);
                let bonus = bridge_value(value, weak, held);
                if let Some(pair) = pairs.iter().position(|p| {
                    join["from"].as_str() == Some(&pool[p.from].track_id)
                        && join["to"].as_str() == Some(&pool[p.to].track_id)
                }) {
                    self.bridge_bonus[pair] = self.bridge_bonus[pair].max(bonus);
                }
            }
        }
    }
    fn learn_duration_bridges(
        &mut self,
        demands: &[(usize, u64, f64)],
        families: &[Family],
        pairs: &[Pair],
        pacing: &[TrackPacing],
    ) {
        self.duration_bonus.clear();
        for &(from, entry, value) in demands {
            for (index, family) in families.iter().enumerate() {
                if pairs[family.pair].from == from
                    && pacing[from].assess(entry, family.exit).is_some()
                {
                    let bonus = value.min(12.);
                    self.duration_bonus
                        .entry(index)
                        .and_modify(|b| b.0 = b.0.max(bonus))
                        .or_insert((bonus, self.counts[family.pair]));
                }
            }
        }
    }
    fn next(&self, families: &[Family], pairs: &[Pair]) -> Option<Work> {
        let mut best: Option<(usize, usize, f64)> = None;
        for (family, f) in families.iter().enumerate() {
            for (window, w) in f
                .windows
                .iter()
                .enumerate()
                .filter(|(_, w)| w.measured.is_none())
            {
                let cue = f.windows[0]
                    .plan
                    .header
                    .extensions
                    .get("tactic_cue_candidate");
                let cost = cue
                    .and_then(|c| c.get("proposal_cost"))
                    .and_then(Value::as_f64)
                    .unwrap_or(1.);
                let c = w.plan.header.extensions.get("tactic_cue_candidate");
                let bars = c
                    .and_then(|c| c.get("overlap_beats"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    / cue
                        .and_then(|c| c.get("bar_beats"))
                        .and_then(Value::as_u64)
                        .unwrap_or(4)
                        .max(1);
                let returning = cue
                    .and_then(|c| c.get("entry_return_probe"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let rank = opportunity_priority(
                    cost,
                    bars as usize,
                    returning,
                    window > 0,
                    self.counts[f.pair],
                    self.best[f.pair],
                );
                let bucket =
                    f.exit / (45 * u64::from(w.plan.placements[0].time_map.source_sample_rate_hz));
                let rank = rank
                    - if self.temporal && window == 0 {
                        3. * self.regions.get(&(f.pair, bucket)).copied().unwrap_or(0) as f64
                    } else {
                        0.
                    };
                let rank = rank
                    + self.bridge_bonus[f.pair]
                        / (1 + self.counts[f.pair].saturating_sub(self.bridge_at[f.pair])) as f64;
                let rank = rank
                    + self.duration_bonus.get(&family).map_or(0., |(bonus, at)| {
                        bonus / (1 + self.counts[f.pair].saturating_sub(*at)) as f64
                    });
                let rank = rank
                    + self.required_bonus.get(f.pair).copied().unwrap_or(0.)
                        / (1. + self.counts[f.pair] as f64 / 4.);
                if best.is_none_or(|(_, _, value)| rank > value) {
                    best = Some((family, window, rank));
                }
            }
        }
        best.map(|(family, window, priority)| {
            let from = pairs[families[family].pair].from;
            Work {
                family,
                window,
                priority,
                serial: 0,
                discovery: true,
                parent: Arc::new(Route {
                    used: 1 << from,
                    last: from,
                    entry_end: 0,
                    length: 1,
                    quality: Quality::EMPTY,
                    plan: None,
                }),
            }
        })
    }
}

struct Frontier {
    heap: BinaryHeap<Work>,
    serial: u64,
    limit: usize,
    refining: bool,
}
impl Frontier {
    fn push(&mut self, parent: Arc<Route>, family: usize, window: usize, f: &Family) {
        self.serial += 1;
        // Ranking is a heuristic, never a pruning proof. Favor making a first
        // complete set and sustained candidates; branch bounds remain separate.
        let priority = priority(&parent, f, window, self.refining);
        self.heap.push(Work {
            parent,
            family,
            window,
            priority,
            serial: self.serial,
            discovery: false,
        });
    }
    fn refine(&mut self, families: &[Family]) {
        self.refining = true;
        let mut work = std::mem::take(&mut self.heap).into_vec();
        for w in &mut work {
            w.priority = priority(&w.parent, &families[w.family], w.window, true);
        }
        self.heap = work.into();
    }
    fn trim(&mut self, stats: &mut Stats) {
        if self.heap.len() <= self.limit {
            return;
        }
        let mut all = std::mem::take(&mut self.heap).into_sorted_vec();
        all.reverse();
        // Reserve space at each depth, so deep routes cannot evict every opening.
        let quota = (self.limit / 8).max(1);
        let mut counts = [0usize; 9];
        let mut keep = vec![];
        let mut rest = vec![];
        for work in all {
            if counts[work.parent.length] < quota {
                counts[work.parent.length] += 1;
                keep.push(work);
            } else {
                rest.push(work);
            }
        }
        let spare = self.limit.saturating_sub(keep.len());
        stats.frontier_dropped += rest.len().saturating_sub(spare);
        keep.extend(rest.into_iter().take(spare));
        self.heap = keep.into();
    }
    fn pop(&mut self, step: usize) -> Option<Work> {
        if step % 32 == 31 && !self.heap.is_empty() {
            // Periodically explore a shallower branch instead of exhausting
            // every variation of the current route before trying another opening.
            let mut all = std::mem::take(&mut self.heap).into_vec();
            let at = (0..all.len())
                .min_by(|&a, &b| {
                    all[a]
                        .parent
                        .length
                        .cmp(&all[b].parent.length)
                        .then_with(|| all[b].cmp(&all[a]))
                })
                .unwrap();
            let next = all.swap_remove(at);
            self.heap = all.into();
            Some(next)
        } else {
            self.heap.pop()
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn enqueue(
    parent: Arc<Route>,
    families: &[Family],
    pairs: &[Pair],
    pacing: &[TrackPacing],
    count: usize,
    reach: &mut Reachability<'_>,
    frontier: &mut Frontier,
    stats: &mut Stats,
) {
    for (index, f) in families.iter().enumerate() {
        let pair = &pairs[f.pair];
        if pair.from != parent.last || parent.used & (1 << pair.to) != 0 {
            continue;
        }
        if pacing[parent.last]
            .assess(parent.entry_end, f.exit)
            .is_none()
            || !reach.can_finish(
                parent.used | (1 << pair.to),
                pair.to,
                f.entry_end,
                count - parent.length - 1,
            )
        {
            stats.cheap_rejections += 1;
            continue;
        }
        frontier.push(parent.clone(), index, 0, f);
    }
    frontier.trim(stats);
}

fn musical_policy(policy: &MixingConfig) -> Result<ContentHash> {
    let mut policy = policy.clone();
    // Measured cue construction always uses this render scaffold, regardless
    // of the generic playlist template. Actual tactic fallback permission lives
    // in max_last_resort_transitions and remains part of the comparison.
    policy.allow_fallback = false;
    policy.template = Template::ShortCrossfade;
    for key in [
        "lazy_incumbent_plan",
        "lazy_measurement_budget",
        "lazy_search_seconds",
        "set_search_strategy",
        "set_config", // Provenance; effective musical choices remain separate policy keys.
    ] {
        policy.header.extensions.remove(key);
    }
    if policy
        .header
        .extensions
        .get(crate::required_pairs::KEY)
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
    {
        policy.header.extensions.remove(crate::required_pairs::KEY);
        policy.header.extensions.remove("required_pair_mode");
    }
    canonical_hash(&policy)
}
fn supplied_incumbent(
    store: &Store,
    policy: &MixingConfig,
    pool: &[Track],
    pacing: &[TrackPacing],
    count: usize,
    cancel: &Cancellation,
) -> Result<Option<MixPlan>> {
    let Some(id) = policy
        .header
        .extensions
        .get("lazy_incumbent_plan")
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    let mut plan = load_plan(store, id)?;
    require(
        plan.placements.len() == count
            && plan
                .placements
                .iter()
                .map(|p| &p.track_id)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == count,
        "LAZY_INCUMBENT",
        "incumbent must contain the requested number of distinct tracks",
    )?;
    require(
        musical_policy(&plan.policy)? == musical_policy(policy)?,
        "LAZY_INCUMBENT",
        "incumbent has different musical policy settings",
    )?;
    require(
        plan.header
            .extensions
            .get("chain_joins")
            .and_then(Value::as_array)
            .is_some_and(|v| v.len() + 1 == count),
        "LAZY_INCUMBENT",
        "incumbent lacks measured join scores",
    )?;
    for (index, p) in plan.placements.iter().enumerate() {
        let at = pool
            .iter()
            .position(|t| t.track_id == p.track_id)
            .ok_or_else(|| {
                Error::new(
                    "LAZY_INCUMBENT",
                    "incumbent track is outside the prepared pool",
                )
            })?;
        if index > 0 && index + 1 < count {
            let entry = p
                .time_map
                .source_at(plan.transitions[index - 1].end_output_frame as f64)
                .round() as u64;
            let exit = p
                .time_map
                .source_at(plan.transitions[index].start_output_frame as f64)
                .round() as u64;
            let decision = pacing[at].assess(entry, exit).ok_or_else(|| {
                Error::new("LAZY_INCUMBENT", "incumbent fails current pacing admission")
            })?;
            plan.header.extensions.get_mut("chain_joins").unwrap()[index]["pacing"] =
                serde_json::to_value(decision)?;
        }
    }
    verify_plan(store, &plan, cancel)?;
    plan.plan_id = crate::id();
    plan.planner_version = if plan.header.extensions.contains_key("set_composition") {
        "set-composition-lazy/2"
    } else {
        "lazy-constraint-search/3"
    }
    .into();
    Ok(Some(plan))
}

#[allow(clippy::too_many_arguments)]
fn save_improvement(
    store: &Store,
    snapshot: &Store,
    mut plan: MixPlan,
    incumbent: &mut Option<MixPlan>,
    stats: &mut Stats,
    history: &mut Vec<Value>,
    progress: &mut Report,
    started: Instant,
    origin: &str,
    cancel: &Cancellation,
) -> Result<bool> {
    if let Some(config) = plan.policy.header.extensions.get("set_config").cloned() {
        plan.header.extensions.insert("set_config".into(), config);
    }
    let pool = measured_beam::prepared_pool(snapshot)?;
    crate::required_pairs::RequiredPairs::resolve(snapshot, &plan.policy, &pool, pool.len())?
        .annotate(&mut plan)?;
    if incumbent
        .as_ref()
        .is_some_and(|old| !crate::composition::better(&plan, old))
    {
        return Ok(false);
    }
    snapshot.verify_read_snapshot(cancel)?;
    plan.plan_id = id();
    plan.planner_version = if plan.header.extensions.contains_key("set_composition") {
        "set-composition-lazy/2"
    } else {
        "lazy-constraint-search/3"
    }
    .into();
    stats.improvements += 1;
    let q = route_quality(&plan);
    history.push(json!({"origin":origin,"plan_id":plan.plan_id,"elapsed_seconds":started.elapsed().as_secs_f64(),
        "measured_windows":stats.measured_windows,"fallbacks":q.fallbacks,"weak_links":q.weak_links,
        "sustained_after_pacing":q.sustained(),"composition":plan.header.extensions.get("set_composition"),"weakest":q.weakest,"total":q.total}));
    plan.header.extensions.insert(
        "lazy_search".into(),
        json!({"status":"incumbent","stats":stats,"improvements":history}),
    );
    store.write(&format!("plans/{}.json", plan.plan_id), &plan)?;
    progress.header.extensions.insert("search".into(),json!({"status":"running","best_plan_id":plan.plan_id,"stats":stats,"improvements":history}));
    store.report(progress)?;
    eprintln!("lazy search: saved improved complete set {} after {} measurements (sustained {:.2}, {} fallback / {} weak joins; {origin})", plan.plan_id,stats.measured_windows,q.sustained(),q.fallbacks,q.weak_links);
    *incumbent = Some(plan);
    Ok(true)
}

fn checkpoint_due(
    measured: usize,
    assembled: usize,
    limit: usize,
    frontier_empty: bool,
    composing: bool,
) -> bool {
    measured > assembled
        && (measured - assembled >= if composing { 512 } else { 128 }
            || measured >= limit
            || (!composing && frontier_empty))
}

pub(super) fn choose(
    store: &Store,
    policy: &MixingConfig,
    context: usize,
    count: usize,
    pool: Vec<Track>,
    cancel: &Cancellation,
) -> Result<MixPlan> {
    if let Some(request) = crate::composition::Request::from_policy(policy)? {
        return crate::duration_search::choose(store, policy, context, pool, request, cancel);
    }
    let duration_mode = crate::composition::Request::from_policy(policy)?.is_some();
    require(
        (3..=8).contains(&count)
            && pool.len() >= (if duration_mode { 3 } else { count })
            && pool.len() <= 128,
        "CHAIN_POOL",
        "lazy set search needs enough distinct prepared tracks (at most 128)",
    )?;
    if let Some(request) = crate::composition::Request::from_policy(policy)? {
        let capacity: f64 = pool
            .iter()
            .map(|t| t.format.frames as f64 / f64::from(t.format.sample_rate_hz))
            .sum();
        require(request.capacity_possible(capacity, 1.-policy.max_speed_deviation), "SET_POOL_DURATION", "even the entire prepared pool is too short for this duration window; analyze more songs")?;
    }
    let snapshot = store.read_snapshot();
    let required = crate::required_pairs::RequiredPairs::resolve(
        &snapshot,
        policy,
        &pool,
        if duration_mode { pool.len() } else { count },
    )?;
    let composition = crate::composition::Request::from_policy(policy)?
        .map(|r| crate::composition::Context::load(&snapshot, &pool, r, cancel))
        .transpose()?;
    let option = |key: &str, default: u64| {
        policy
            .header
            .extensions
            .get(key)
            .and_then(Value::as_u64)
            .unwrap_or(default)
    };
    let measurement_limit = option("lazy_measurement_budget", 1536).clamp(1, 100_000) as usize;
    let seconds = option("lazy_search_seconds", 1200).clamp(1, 86400);
    let max_fallbacks = option("max_last_resort_transitions", 1).min((count - 1) as u64) as usize;
    let started = Instant::now();
    let pacing = pool
        .iter()
        .map(|t| mixlib_planner::read_track_pacing(&snapshot, &t.track_id, cancel))
        .collect::<Result<Vec<_>>>()?;
    eprintln!(
        "lazy search: proposing cue families from {} tracks before any pair PCM measurement",
        pool.len()
    );
    let (mut pairs, mut families) = propose(&snapshot, &pool, policy, context, &required, cancel)?;
    let probes: Vec<_> = families
        .iter()
        .map(|f| Probe {
            from: pairs[f.pair].from,
            to: pairs[f.pair].to,
            exit: f.exit,
            entry_end: f.entry_end,
        })
        .collect();
    let mut reach = Reachability {
        probes: &probes,
        pacing: &pacing,
        memo: BTreeMap::new(),
    };
    let upper = families.iter().map(|f| f.upper).fold(0., f64::max);
    let mut stats = Stats {
        proposed_families: families.len(),
        ..Stats::default()
    };
    let mut frontier = Frontier {
        heap: BinaryHeap::new(),
        serial: 0,
        limit: (policy.beam_width * 256).max(1024),
        refining: false,
    };
    for last in 0..if composition.is_some() { 0 } else { pool.len() } {
        if !required.start_allowed(last) {
            continue;
        }
        let root = Arc::new(Route {
            used: 1 << last,
            last,
            entry_end: 0,
            length: 1,
            quality: Quality::EMPTY,
            plan: None,
        });
        enqueue(
            root,
            &families,
            &pairs,
            &pacing,
            count,
            &mut reach,
            &mut frontier,
            &mut stats,
        );
    }
    let mut admission = tactics::policy(
        policy
            .header
            .extensions
            .get("allow_expressive_tactics")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    );
    admission.normal_speed_deviation = policy.max_speed_deviation;
    let mut cache = CandidateEvidenceCache::new();
    let mut incumbent = supplied_incumbent(&snapshot, policy, &pool, &pacing, count, cancel)?;
    if let Some(seed) = &mut incumbent {
        required.annotate(seed)?;
        if let Some(config) = policy.header.extensions.get("set_config") {
            seed.header
                .extensions
                .insert("set_config".into(), config.clone());
        }
    }
    let mut history = vec![];
    let mut progress = Report::new("lazy_set_search");
    if let Some(config) = policy.header.extensions.get("set_config") {
        progress
            .header
            .extensions
            .insert("set_config".into(), config.clone());
    }
    progress
        .header
        .extensions
        .insert("required_pairs".into(), required.evidence());
    if let Some(seed) = &mut incumbent {
        let q = route_quality(seed);
        history.push(json!({"origin":"provided_incumbent","plan_id":seed.plan_id,"elapsed_seconds":started.elapsed().as_secs_f64(),
            "measured_windows":0,"fallbacks":q.fallbacks,"weak_links":q.weak_links,"sustained_after_pacing":q.sustained(),"weakest":q.weakest,"total":q.total}));
        seed.header.extensions.insert(
            "lazy_search".into(),
            json!({"status":"incumbent","stats":stats,"improvements":history}),
        );
        store.write(&format!("plans/{}.json", seed.plan_id), seed)?;
        eprintln!(
            "lazy search: retaining verified supplied incumbent {} (sustained {:.2})",
            seed.plan_id,
            q.sustained()
        );
        frontier.refine(&families);
    }
    let mut stop = "frontier_exhausted";
    let mut discovery = Discovery {
        required_bonus: pairs
            .iter()
            .map(|p| required.priority(p.from, p.to))
            .collect(),
        duration_bonus: BTreeMap::new(),
        temporal: composition.is_some(),
        regions: BTreeMap::new(),
        counts: vec![0; pairs.len()],
        best: vec![0.; pairs.len()],
        bridge_bonus: vec![0.; pairs.len()],
        bridge_at: vec![0; pairs.len()],
    };
    let mut graph: Vec<Vec<MeasuredEdge>> = (0..pool.len() * pool.len()).map(|_| vec![]).collect();
    let mut assembled_at = 0;
    let mut assembly_history = vec![];
    let deadline = started + std::time::Duration::from_secs(seconds);
    let measurement_deadline = if composition.is_some() {
        deadline - std::time::Duration::from_secs((seconds / 10).clamp(1, 300))
    } else {
        deadline
    };
    loop {
        // Reserve time to assemble newly measured evidence even if the next
        // periodic checkpoint would lie beyond the wall-clock budget.
        let final_composition = composition.is_some()
            && (Instant::now() >= measurement_deadline
                || stats.work_items >= policy.max_expansions);
        // Recombine every measured edge, including anchors discovered on a
        // discarded prefix. A cached measurement must outlive route pruning.
        if checkpoint_due(
            stats.measured_windows,
            assembled_at,
            measurement_limit,
            frontier.heap.is_empty(),
            composition.is_some(),
        ) || (stats.measured_windows > assembled_at
            && (final_composition
                || families
                    .iter()
                    .all(|f| f.windows.iter().all(|w| w.measured.is_some()))))
        {
            assembled_at = stats.measured_windows;
            stats.graph_assemblies += 1;
            let assembled = if let Some(composer) = &composition {
                let result = composer.assemble(
                    &snapshot,
                    &graph,
                    &pool,
                    &pacing,
                    policy,
                    deadline.min(
                        Instant::now()
                            + std::time::Duration::from_secs(
                                if stats.measured_windows >= measurement_limit || final_composition
                                {
                                    300
                                } else {
                                    60
                                },
                            ),
                    ),
                    cancel,
                )?;
                discovery.learn_duration_bridges(&result.bridges, &families, &pairs, &pacing);
                assembly_history.push(json!({"kind":"composition","measured_windows":assembled_at,
                    "complete":result.plan.is_some(),"assembly":result.stats,
                    "bridge_demands":result.bridges.iter().map(|(from,entry,value)|json!({"from":pool[*from].track_id,"entry_end_frame":entry,"anchor_value":value})).collect::<Vec<_>>(),
                    "composition":result.plan.as_ref().and_then(|p|p.header.extensions.get("set_composition"))}));
                progress.header.extensions.insert(
                    "composition_checkpoint".into(),
                    assembly_history.last().unwrap().clone(),
                );
                store.report(&progress)?;
                eprintln!("composition: {} candidates; reached {} tracks / {:.1} min; {} duration and {} development rejections; {} bridge demands; complete {}",result.stats.examined,result.stats.deepest_tracks,result.stats.furthest_join_seconds/60.,result.stats.duration_rejections,result.stats.development_rejections,result.bridges.len(),result.plan.is_some());
                result.plan
            } else {
                measured_beam::assemble_measured(
                    &snapshot,
                    &graph,
                    &pool,
                    &pacing,
                    policy,
                    count,
                    max_fallbacks,
                    12,
                    Some(deadline),
                    cancel,
                )?
            };
            if assembly_history.len() > 16 {
                assembly_history.remove(0);
            }
            if let Some(plan) = assembled {
                stats.complete_candidates += 1;
                discovery.learn_bridges(&plan, &pool, &pairs);
                let q = route_quality(&plan);
                assembly_history.push(json!({"measured_windows":assembled_at,"sustained_after_pacing":q.sustained(),
                    "fallbacks":q.fallbacks,"weak_links":q.weak_links,"trials":plan.header.extensions.get("dense_anchor_search")}));
                if assembly_history.len() > 16 {
                    assembly_history.remove(0);
                }
                if save_improvement(
                    store,
                    &snapshot,
                    plan,
                    &mut incumbent,
                    &mut stats,
                    &mut history,
                    &mut progress,
                    started,
                    "measured_anchor_assembly",
                    cancel,
                )? {
                    frontier.refine(&families);
                }
            }
        }
        if final_composition {
            stop = if stats.work_items >= policy.max_expansions {
                "work_budget"
            } else {
                "time_budget"
            };
            break;
        }
        let discover = stats.measured_windows < measurement_limit && stats.work_items % 2 == 0;
        let work = if discover {
            discovery.next(&families, &pairs)
        } else {
            None
        }
        .or_else(|| frontier.pop(stats.work_items))
        .or_else(|| {
            (stats.measured_windows < measurement_limit)
                .then(|| discovery.next(&families, &pairs))
                .flatten()
        });
        let Some(work) = work else {
            if stats.measured_windows >= measurement_limit {
                stop = "measurement_budget";
            }
            break;
        };
        cancel.check()?;
        if stats.work_items >= policy.max_expansions {
            stop = "work_budget";
            break;
        }
        if started.elapsed().as_secs() >= seconds {
            stop = "time_budget";
            break;
        }
        stats.work_items += 1;
        let parent = &work.parent;
        let f = &families[work.family];
        let pair = &pairs[f.pair];
        let remaining = count - parent.length;
        if !work.discovery && !required.allows(parent.used, parent.last, pair.to, remaining - 1) {
            stats.cheap_rejections += 1;
            continue;
        }
        if !work.discovery
            && !can_beat(
                optimistic(parent.quality, remaining, upper),
                incumbent.as_ref().map(route_quality),
            )
        {
            stats.bound_rejections += 1;
            continue;
        }
        let (exit, entry_end) = coordinates(&f.windows[work.window].plan);
        let Some(paced) = pacing[parent.last].assess(parent.entry_end, exit) else {
            stats.cheap_rejections += 1;
            continue;
        };
        if !work.discovery
            && !reach.can_finish(
                parent.used | (1 << pair.to),
                pair.to,
                entry_end,
                remaining - 1,
            )
        {
            stats.cheap_rejections += 1;
            continue;
        }
        if f.windows[work.window].measured.is_none() {
            if stats.measured_windows >= measurement_limit {
                stop = "measurement_budget";
                continue;
            }
            let pair_id = f.pair;
            let seed = f.windows[work.window].plan.clone();
            let pair = &mut pairs[pair_id];
            let request = StitchRequest {
                from: &pool[pair.from].track_id,
                to: &pool[pair.to].track_id,
                overlap_beats: 8,
                context_beats: context,
                outgoing_after_frame: 0,
            };
            let edges = tactics::measure_window(
                &snapshot,
                &request,
                policy,
                &mut cache,
                &seed,
                &mut pair.expansion,
                &admission,
                &mut pair.admitted,
                &mut pair.rejected,
                &mut pair.details,
                cancel,
            )?;
            require(
                edges.iter().all(|e| {
                    e.dense_quality_seconds + e.handoff_quality_seconds
                        <= families[work.family].upper + 1e-6
                }),
                "LAZY_BOUND",
                "measured sustained value exceeded its optimistic duration bound",
            )?;
            discovery.counts[pair_id] += 1;
            let bucket = exit / (45 * u64::from(seed.placements[0].time_map.source_sample_rate_hz));
            *discovery.regions.entry((pair_id, bucket)).or_default() += 1;
            discovery.best[pair_id] = discovery.best[pair_id].max(
                edges
                    .iter()
                    .map(|e| e.dense_quality_seconds + e.handoff_quality_seconds)
                    .fold(0., f64::max),
            );
            graph[pair.from * pool.len() + pair.to].extend(edges.iter().cloned());
            if work.discovery {
                stats.discovery_measurements += 1;
            }
            let children = pair.expansion.take_discovered();
            let family = &mut families[work.family];
            for child in children {
                let (child_exit, child_end) = coordinates(&child);
                require(
                    child_exit == family.exit
                        && child_end >= family.entry_end
                        && duration(&child) <= family.upper + 1e-6,
                    "LAZY_BOUND",
                    "continuation escaped its cue-family envelope",
                )?;
                family.windows.push(Window {
                    plan: child,
                    measured: None,
                });
                stats.discovered_windows += 1;
            }
            let anchor_value = edges
                .iter()
                .map(|e| e.dense_quality_seconds + e.handoff_quality_seconds)
                .fold(0., f64::max);
            family.windows[work.window].measured = Some(edges);
            if anchor_value > 0. && parent.length > 1 {
                let root = Arc::new(Route {
                    used: 1 << pair.from,
                    last: pair.from,
                    entry_end: 0,
                    length: 1,
                    quality: Quality::EMPTY,
                    plan: None,
                });
                frontier.push(root, work.family, work.window, family);
                stats.measured_anchor_starts += 1;
            }
            stats.measured_windows += 1;
            if stats.measured_windows % 25 == 0 {
                eprintln!(
                    "lazy search: {} windows measured, {} cheap rejections, {} improvements",
                    stats.measured_windows, stats.cheap_rejections, stats.improvements
                );
            }
        } else {
            stats.cached_window_uses += 1;
        }
        if composition.is_some() {
            continue;
        }
        let f = &families[work.family];
        let pair = &pairs[f.pair];
        if work.discovery
            && !reach.can_finish(
                parent.used | (1 << pair.to),
                pair.to,
                entry_end,
                remaining - 1,
            )
        {
            stats.cheap_rejections += 1;
            continue;
        }
        if work.window + 1 < f.windows.len() {
            frontier.push(parent.clone(), work.family, work.window + 1, f);
        }
        if !required.start_allowed(parent.last) && parent.length == 1
            || !required.allows(parent.used, parent.last, pair.to, remaining - 1)
        {
            stats.cheap_rejections += 1;
            continue;
        }
        for edge in f.windows[work.window].measured.as_ref().unwrap() {
            let quality = parent
                .quality
                .extend(
                    edge.score,
                    edge.last_resort,
                    edge.dense_quality_seconds,
                    edge.handoff_quality_seconds,
                )
                .paced(paced.penalty_quality_seconds);
            if quality.fallbacks > max_fallbacks
                || !can_beat(
                    optimistic(quality, remaining - 1, upper),
                    incumbent.as_ref().map(route_quality),
                )
            {
                stats.bound_rejections += 1;
                continue;
            }
            let mut plan = match chain::extend_measured(
                parent.plan.as_ref(),
                &edge.plan,
                edge.score,
                edge.last_resort,
            ) {
                Ok(p) => p,
                Err(e) if route_local(&e) => {
                    stats.assembly_rejections += 1;
                    continue;
                }
                Err(e) => return Err(e),
            };
            plan.header
                .extensions
                .get_mut("chain_joins")
                .and_then(Value::as_array_mut)
                .unwrap()
                .last_mut()
                .unwrap()["pacing"] = serde_json::to_value(&paced)?;
            if remaining == 1 {
                stats.complete_candidates += 1;
                let complete = match chain::finish_measured(&snapshot, plan, cancel) {
                    Ok(p) => p,
                    Err(e) if route_local(&e) => {
                        stats.assembly_rejections += 1;
                        continue;
                    }
                    Err(e) => return Err(e),
                };
                if save_improvement(
                    store,
                    &snapshot,
                    complete,
                    &mut incumbent,
                    &mut stats,
                    &mut history,
                    &mut progress,
                    started,
                    "route_frontier",
                    cancel,
                )? {
                    frontier.refine(&families);
                }
            } else {
                let route = Arc::new(Route {
                    used: parent.used | (1 << pair.to),
                    last: pair.to,
                    entry_end: edge.entry_end,
                    length: parent.length + 1,
                    quality,
                    plan: Some(plan),
                });
                enqueue(
                    route,
                    &families,
                    &pairs,
                    &pacing,
                    count,
                    &mut reach,
                    &mut frontier,
                    &mut stats,
                );
            }
        }
        frontier.trim(&mut stats);
    }
    snapshot.verify_read_snapshot(cancel)?;
    let mut reports = vec![];
    for pair in pairs
        .iter()
        .filter(|p| !p.details.is_empty() || !p.rejected.is_empty())
    {
        let mut report = Report::new("lazy_measured_pair_windows");
        report.header.extensions.insert("evidence".into(),json!({"from":pool[pair.from].track_id,"to":pool[pair.to].track_id,"windows":pair.details,"admitted":pair.admitted,"rejected":pair.rejected}));
        store.report(&report)?;
        reports.push(format!("reports/{}.json", report.report_id));
    }
    let evidence = json!({"status":stop,"stats":stats,"improvements":history,"pair_reports":reports,"anchor_assemblies":assembly_history,
        "measurement_limit":measurement_limit,"time_limit_seconds":seconds,"elapsed_seconds":started.elapsed().as_secs_f64(),
        "unmeasured_seed_families":families.iter().filter(|f|f.windows[0].measured.is_none()).count(),
        "frontier_remaining":frontier.heap.len(),"frontier_limit":frontier.limit,"prepared_tracks":pool.len(),
        "scope":"bounded cue families and dynamically measured continuations; heuristic frontier ordering; no global-optimality claim",
        "objective":if composition.is_some() {"valid duration and minimum development; no fallback/weak joins; sustained quality per minute minus arc and dwell penalties"} else {"fewest fallbacks, fewest weak joins, most sustained quality minus pacing penalty, strongest weakest join, total fit"}});
    progress
        .header
        .extensions
        .insert("search".into(), evidence.clone());
    store.report(&progress)?;
    progress.header.extensions.insert("required_pair_admission".into(),json!(required.pairs.iter().map(|pair| {
        let [a,b]=pair.indices;
        let mut choices:Vec<_>=graph[a*pool.len()+b].iter().collect();
        if required.mode==crate::required_pairs::Mode::Either { choices.extend(&graph[b*pool.len()+a]); }
        json!({"spotify_ids":pair.spotify_ids,"track_ids":pair.track_ids,"mode":required.mode,
            "measured_admitted_choices":choices.len(),"clean_choices":choices.iter().filter(|e|!e.last_resort && e.score>=25.).count(),
            "meaning":if required.mode==crate::required_pairs::Mode::Anywhere {"membership only; adjacency is optional"} else {"choices measured in this search budget; zero is not proof of global impossibility"}})
    }).collect::<Vec<_>>()));
    store.report(&progress)?;
    let Some(mut best) = incumbent else {
        let code = if required.pairs.is_empty() {
            "LAZY_NO_INCUMBENT"
        } else {
            "REQUIRED_PAIRS_NO_SET"
        };
        return Err(Error::new(code,format!("lazy search stopped at {stop} without a complete admitted set; evidence reports/{}.json",progress.report_id)));
    };
    best.header
        .extensions
        .insert("lazy_search".into(), evidence);
    best.header.extensions.insert("library_pick".into(),json!({"analyzed_tracks":pool.len(),"measured_choices":stats.measured_windows,"track_ids":best.placements.iter().map(|p|&p.track_id).collect::<Vec<_>>(),"strategy":"lazy"}));
    store.write(&format!("plans/{}.json", best.plan_id), &best)?;
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duration_bridge_feedback_only_boosts_exits_after_the_retained_entry() {
        let mut discovery = Discovery {
            required_bonus: vec![],
            duration_bonus: BTreeMap::new(),
            temporal: true,
            regions: BTreeMap::new(),
            counts: vec![3],
            best: vec![0.],
            bridge_bonus: vec![0.],
            bridge_at: vec![0],
        };
        let pairs = vec![Pair {
            from: 0,
            to: 1,
            expansion: CandidateQueue::expansion_only(&[], 24),
            admitted: BTreeMap::new(),
            rejected: BTreeMap::new(),
            details: vec![],
        }];
        let families = vec![
            Family {
                pair: 0,
                exit: 60,
                entry_end: 0,
                upper: 120.,
                windows: vec![],
            },
            Family {
                pair: 0,
                exit: 100,
                entry_end: 0,
                upper: 120.,
                windows: vec![],
            },
        ];
        discovery.learn_duration_bridges(
            &[(0, 50, 10.)],
            &families,
            &pairs,
            &[TrackPacing::unknown(1)],
        );
        assert!(
            !discovery.duration_bonus.contains_key(&0),
            "an early exit cannot repair the retained route"
        );
        assert!(discovery.duration_bonus.contains_key(&1));
        discovery.learn_duration_bridges(&[], &families, &pairs, &[TrackPacing::unknown(1)]);
        assert!(
            discovery.duration_bonus.is_empty(),
            "stale bridge demand cannot permanently monopolize measurement"
        );
    }

    #[test]
    fn duration_discovery_does_not_reassemble_after_every_window() {
        assert!(!checkpoint_due(1, 0, 8192, true, true));
        assert!(!checkpoint_due(511, 0, 8192, true, true));
        assert!(checkpoint_due(512, 0, 8192, true, true));
        assert!(!checkpoint_due(512, 512, 8192, true, true));
        assert!(checkpoint_due(650, 512, 650, true, true));
        assert!(checkpoint_due(1, 0, 1536, true, false));
    }

    #[test]
    fn weak_bridges_to_strong_anchors_get_measurement_priority() {
        assert!(bridge_value(15., true, 0.) > bridge_value(15., false, 0.));
        assert!(bridge_value(15., true, 0.) > bridge_value(5., true, 0.));
        assert_eq!(bridge_value(15., false, 10.), 0.);
        assert_eq!(bridge_value(0., true, 0.), 0.);
    }
    #[test]
    fn discovery_protects_musical_probes_and_fair_coverage() {
        let long_unmeasured = opportunity_priority(0.1, 32, false, false, 0, 0.);
        let phrase_probe = opportunity_priority(0.25, 8, false, false, 0, 0.);
        assert!(
            phrase_probe > long_unmeasured,
            "duration alone must not hide a supported phrase probe"
        );
        assert!(
            opportunity_priority(0.25, 8, false, true, 1, 0.) > phrase_probe,
            "a measured promising seed must get to test its musical ending"
        );
        assert!(
            phrase_probe > opportunity_priority(0.25, 8, false, false, 4, 10.),
            "one promising pair cannot consume all untested pairs' opportunities"
        );
        assert!(
            opportunity_priority(0.25, 8, false, false, 1, 10.)
                > opportunity_priority(0.25, 8, false, false, 1, 0.),
            "measured mashup value should direct further exploration"
        );
    }

    #[test]
    fn incumbent_policy_ignores_only_search_controls() {
        let original = MixingConfig::default();
        let mut changed = original.clone();
        changed.allow_fallback = !original.allow_fallback;
        changed.template = Template::ShortCrossfade;
        for key in [
            "lazy_incumbent_plan",
            "lazy_measurement_budget",
            "lazy_search_seconds",
            "set_search_strategy",
            "set_config",
        ] {
            changed
                .header
                .extensions
                .insert(key.into(), json!("different"));
        }
        assert_eq!(
            musical_policy(&original).unwrap(),
            musical_policy(&changed).unwrap()
        );
        changed
            .header
            .extensions
            .insert(crate::required_pairs::KEY.into(), json!([]));
        changed
            .header
            .extensions
            .insert("required_pair_mode".into(), json!("either"));
        assert_eq!(
            musical_policy(&original).unwrap(),
            musical_policy(&changed).unwrap()
        );
        changed.header.extensions.insert(
            crate::required_pairs::KEY.into(),
            json!([["required-a", "required-b"]]),
        );
        assert_ne!(
            musical_policy(&original).unwrap(),
            musical_policy(&changed).unwrap()
        );
        changed.header.extensions.remove(crate::required_pairs::KEY);
        changed.header.extensions.remove("required_pair_mode");
        changed
            .header
            .extensions
            .insert("musical_constraint".into(), json!(true));
        assert_ne!(
            musical_policy(&original).unwrap(),
            musical_policy(&changed).unwrap()
        );
    }

    #[test]
    fn optimistic_reachability_matches_exhaustive_small_graphs() {
        fn exhaustive(
            probes: &[Probe],
            pacing: &[TrackPacing],
            used: u128,
            last: usize,
            entry: u64,
            left: usize,
        ) -> bool {
            left == 0
                || probes.iter().any(|p| {
                    p.from == last
                        && used & (1 << p.to) == 0
                        && pacing[last].assess(entry, p.exit).is_some()
                        && exhaustive(
                            probes,
                            pacing,
                            used | (1 << p.to),
                            p.to,
                            p.entry_end,
                            left - 1,
                        )
                })
        }
        let pacing = vec![TrackPacing::unknown(1); 4];
        let mut random = 7u64;
        for _ in 0..50 {
            let mut probes = vec![];
            for from in 0..4 {
                for to in 0..4 {
                    if from == to {
                        continue;
                    }
                    for _ in 0..2 {
                        random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                        probes.push(Probe {
                            from,
                            to,
                            exit: 40 + (random % 160),
                            entry_end: 10 + (random / 256 % 120),
                        });
                    }
                }
            }
            let mut reach = Reachability {
                probes: &probes,
                pacing: &pacing,
                memo: BTreeMap::new(),
            };
            for first in 0..4 {
                for length in 1..4 {
                    assert_eq!(
                        reach.can_finish(1 << first, first, 0, length),
                        exhaustive(&probes, &pacing, 1 << first, first, 0, length)
                    );
                }
            }
        }
    }

    #[test]
    fn cheap_reachability_rejects_repeats_and_insufficient_runway() {
        let probes = [
            Probe {
                from: 0,
                to: 1,
                exit: 20,
                entry_end: 100,
            },
            Probe {
                from: 1,
                to: 2,
                exit: 140,
                entry_end: 10,
            },
            Probe {
                from: 1,
                to: 0,
                exit: 200,
                entry_end: 10,
            },
        ];
        let pacing = vec![TrackPacing::unknown(1); 3];
        let mut r = Reachability {
            probes: &probes,
            pacing: &pacing,
            memo: BTreeMap::new(),
        };
        assert!(!r.can_finish(1, 0, 0, 2));
        assert!(r.can_finish(1, 0, 0, 1));
        assert!(!r.can_finish(0b111, 1, 100, 1));
    }
    #[test]
    fn upper_bound_never_discards_a_possible_quality_improvement() {
        let q = Quality::EMPTY.extend(35., false, 10., 0.).paced(1.);
        let incumbent = Quality::EMPTY.extend(40., false, 25., 0.);
        assert!(can_beat(optimistic(q, 1, 120.), Some(incumbent)));
        assert!(!can_beat(optimistic(q, 0, 120.), Some(incumbent)));
        assert!(!can_beat(
            optimistic(q.extend(100., true, 100., 0.), 3, 120.),
            Some(incumbent)
        ));
        let better = q.extend(50., false, 50., 0.).paced(2.);
        assert!(optimistic(q, 1, 120.).best_first(better).is_le());
    }
}
