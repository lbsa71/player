//! Whole-set local refinement. Nil is an unresolved planning edge, never an
//! executable or admitted musical tactic. Only a fully validated plan exports.
use super::*;
use crate::{
    discovery_schedule::{Probe, Schedule},
    refinement_branches::Branch,
    refinement_layout,
};
use std::collections::VecDeque;

#[cfg(feature = "cp-search")]
#[path = "cp_archive.rs"]
mod cp_archive;

#[cfg(feature = "cp-search")]
#[path = "cp_search.rs"]
mod cp_search;

#[cfg(feature = "cp-search")]
#[path = "cp_manifest.rs"]
mod cp_manifest;

#[cfg(feature = "cp-search")]
#[path = "cp_startup.rs"]
mod cp_startup;

#[derive(Clone)]
struct Block {
    tracks: Vec<usize>,
    edges: Vec<usize>,
    plan: Option<MixPlan>,
    minimum: u64,
    maximum: u64,
}
#[derive(Clone)]
struct Draft {
    blocks: Vec<Arc<Block>>,
    lengths: Vec<u64>,
    score: f64,
    key: String,
}
impl Draft {
    fn nil(&self) -> usize {
        self.blocks.len().saturating_sub(1)
    }
    fn edges(&self) -> Vec<usize> {
        self.blocks
            .iter()
            .flat_map(|b| b.edges.iter().copied())
            .collect()
    }
    fn tracks(&self) -> Vec<usize> {
        self.blocks
            .iter()
            .flat_map(|b| b.tracks.iter().copied())
            .collect()
    }
    fn better(&self, other: &Self) -> bool {
        self.nil() < other.nil() || (self.nil() == other.nil() && self.score > other.score + 1e-9)
    }
}
#[derive(Default)]
struct Scheduler {
    parked: VecDeque<Draft>,
    generation: usize,
    pool_cursor: usize,
    repair_cursor: usize,
    proposed_pairs: BTreeSet<(usize, usize)>,
    measured_pairs: BTreeSet<(usize, usize)>,
    closing_attempts: usize,
    #[cfg(feature = "cp-search")]
    archive_revalidation: Option<Value>,
    closing_diagnostics: crate::composition::CloseDiagnostics,
    bound_pruned: usize,
    cache_loaded: bool,
    frontier_saved: Option<Instant>,
    branches: VecDeque<Branch>,
    branch_roots: BTreeSet<String>,
    branches_expanded: usize,
    impossible_branches: usize,
    identities: std::cell::RefCell<BTreeMap<usize, String>>,
}

// Reordering and replacing songs can change every join. Until those freedoms
// are restricted, pretending the currently measured graph is exhaustive would
// give an invalid bound. All composition penalties are nonnegative.
fn unrestricted_upper_bound(pool_len: usize, request: Request) -> f64 {
    unrestricted_score_upper(pool_len, request).unwrap_or(f64::INFINITY)
}
fn closing_capacity(maximum: u64, rate: f64, minimum_seconds: f64) -> bool {
    maximum as f64 / rate * (1. + MAX_REPLAY_FRACTION) + 1e-6 >= minimum_seconds
}
fn frontier_parts(
    cache: &Cache,
    store: &Store,
    rows: Vec<Value>,
    deadline: Option<Instant>,
) -> Result<Option<Vec<String>>> {
    // Removing the first parked item or changing the best candidate must not
    // shift every subsequent chunk. Stable hash buckets localize membership
    // changes, and content-addressed chunks remain reusable across snapshots.
    let mut buckets: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
    for row in rows {
        if deadline.is_some_and(|at| Instant::now() >= at) {
            return Ok(None);
        }
        let key = canonical_hash(&row)?.digest;
        buckets
            .entry(key[..2].to_owned())
            .or_default()
            .insert(key, row);
    }
    let mut parts = vec![];
    for bucket in buckets.into_values() {
        let rows = bucket.into_values().collect::<Vec<_>>();
        for chunk in rows.chunks(128) {
            if deadline.is_some_and(|at| Instant::now() >= at) {
                return Ok(None);
            }
            let payload = json!(chunk);
            let part = canonical_hash(&payload)?.digest;
            cache.write_immutable(store, "refinement-frontier-parts", &part, &payload)?;
            parts.push(part);
        }
    }
    Ok(Some(parts))
}
fn shuffled_key(index: usize, generation: usize) -> u64 {
    let mut x = (index as u64).wrapping_add((generation as u64).wrapping_mul(0x9e3779b97f4a7c15));
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

#[cfg_attr(not(feature = "cp-search"), allow(dead_code))]
enum AssemblyRejection {
    Prefix { len: usize, reason: String },
    Unknown(String),
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum StopReason {
    Cancelled,
    WallClock,
    MeasurementWindows,
    GeneratedProbeScope,
    #[cfg(feature = "cp-search")]
    DirectedDurationCapacity,
    #[cfg(feature = "cp-search")]
    RequiredAdjacencyChain,
}

struct Refiner<'a> {
    search: Search<'a>,
    edges: Vec<Arc<MeasuredEdge>>,
    edge_keys: BTreeSet<String>,
    blocks: BTreeMap<Vec<usize>, Option<Arc<Block>>>,
    population: Vec<Draft>,
    best: Option<Draft>,
    seen: BTreeSet<String>,
    attempted_repairs: BTreeSet<(String, usize)>,
    attempted_final: BTreeSet<Vec<usize>>,
    draft_report: Report,
    history: Vec<Value>,
    rounds: usize,
    rejected_layouts: usize,
    scheduler: Scheduler,
    #[cfg(feature = "cp-search")]
    cp: Option<cp_search::CpSearch>,
}
impl<'a> Refiner<'a> {
    fn new(search: Search<'a>) -> Self {
        Self {
            edges: vec![],
            edge_keys: BTreeSet::new(),
            blocks: BTreeMap::new(),
            population: vec![],
            best: None,
            seen: BTreeSet::new(),
            attempted_repairs: BTreeSet::new(),
            attempted_final: BTreeSet::new(),
            draft_report: Report::new("complete_set_refinement"),
            history: vec![],
            rounds: 0,
            rejected_layouts: 0,
            scheduler: Scheduler::default(),
            #[cfg(feature = "cp-search")]
            cp: (search
                .policy
                .header
                .extensions
                .get(crate::set_config::ENGINE_KEY)
                .and_then(Value::as_str)
                == Some("cp"))
            .then(cp_search::CpSearch::default),
            search,
        }
    }
    #[cfg(feature = "cp-search")]
    fn propose_cp_route(&mut self, deadline: Instant, cancel: &Cancellation) -> Result<()> {
        if let Some(mut cp) = self.cp.take() {
            let result = cp.step(self, deadline, cancel).and_then(|draft| {
                cp.closing.checkpoint_routes(&self.search)?;
                Ok(draft)
            });
            self.cp = Some(cp);
            if let Some(draft) = result? {
                self.admit(draft, cancel)?;
            }
        }
        Ok(())
    }

    #[cfg(feature = "cp-search")]
    fn close_cp_routes(&mut self, deadline: Instant, cancel: &Cancellation) -> Result<bool> {
        if let Some(mut cp) = self.cp.take() {
            let result = cp
                .closing
                .slice(self, deadline, cancel)
                .and_then(|pending| {
                    cp.closing.checkpoint_routes(&self.search)?;
                    Ok(pending)
                });
            self.cp = Some(cp);
            return result;
        }
        Ok(false)
    }

    #[cfg(feature = "cp-search")]
    fn schedule_closing_window(
        &mut self,
        discovery: &mut Schedule,
        frames: &[u64],
        minimum: &[u64],
        measured: &mut BTreeSet<String>,
        exploration: &mut VecDeque<MixPlan>,
        cancel: &Cancellation,
    ) -> Result<Option<MixPlan>> {
        let Some(cp) = &self.cp else {
            return Ok(None);
        };
        let targets = cp.closing_probes(self);
        let Some(probe) = discovery.next_targeted(&targets, frames, minimum) else {
            return Ok(None);
        };
        self.scheduler.proposed_pairs.insert((probe.from, probe.to));
        let plans = self
            .search
            .proposals(probe.from, probe.to, probe.after, probe.tier, cancel)?;
        let mut selected = None;
        for seed in plans {
            let key = window_key(&self.search.cache, &seed)?;
            if self.search.measured_keys.contains(&key) || measured.contains(&key) {
                continue;
            }
            if selected.is_none() && cp.matches_closing_window(self, probe, &seed) {
                measured.insert(key);
                selected = Some(seed);
            } else {
                exploration.push_back(seed);
            }
        }
        self.cp
            .as_mut()
            .unwrap()
            .note_closing_probe(selected.is_some());
        Ok(selected)
    }

    #[cfg(feature = "cp-search")]
    fn bootstrap_measured_incumbent(
        &mut self,
        deadline: Instant,
        cancel: &Cancellation,
    ) -> Result<()> {
        let Some(cp) = &self.cp else {
            return Ok(());
        };
        let required_growth = (cp.bootstrap_edges / 4).max(16);
        if self.search.incumbent.is_some()
            || self.edges.len() < 2
            || Instant::now() >= deadline
            || (cp.bootstrap_attempts > 0
                && self.edges.len() < cp.bootstrap_edges + required_growth)
        {
            return Ok(());
        }
        let mut touched = BTreeSet::new();
        for edge in &self.edges {
            touched.extend(edge.plan.placements.iter().map(|p| p.track_id.as_str()));
        }
        let source_seconds = self
            .search
            .pool
            .iter()
            .filter(|t| touched.contains(t.track_id.as_str()))
            .map(|t| seconds(t, t.format.frames))
            .sum();
        let min_speed = self
            .edges
            .iter()
            .flat_map(|e| &e.plan.placements)
            .map(|p| p.time_map.speed_ratio_limits[0])
            .fold(1. - self.search.policy.max_speed_deviation, f64::min);
        if !self
            .search
            .request
            .capacity_possible(source_seconds, min_speed)
        {
            return Ok(());
        }
        let stats = cp_startup::search(self, deadline, cancel)?;
        eprintln!("set search: measured bootstrap examined {} edges, reached {} tracks / {:.1} min, closing attempts {}, approved {}",
            stats.examined, stats.deepest_tracks, stats.furthest_join_seconds / 60.,
            stats.closing_attempts, stats.approved);
        let cp = self.cp.as_mut().unwrap();
        cp.bootstrap_edges = self.edges.len();
        cp.bootstrap_attempts += 1;
        cp.measured_bootstrap = Some(stats);
        Ok(())
    }

    fn rate(&self) -> f64 {
        f64::from(self.search.policy.output_sample_rate_hz)
    }
    fn track_seconds(&self, i: usize) -> f64 {
        seconds(&self.search.pool[i], self.search.pool[i].format.frames)
    }
    fn singleton(&self, i: usize) -> Arc<Block> {
        let length = self.track_seconds(i);
        Arc::new(Block {
            tracks: vec![i],
            edges: vec![],
            plan: None,
            minimum: (length.mul_add(0.45, 0.).min(90.) * self.rate()).ceil() as u64,
            maximum: (length * self.rate()).floor() as u64,
        })
    }
    fn sync_edges(&mut self) -> Result<bool> {
        let before = self.edges.len();
        let n = self.search.pool.len();
        for (pair, edges) in self.search.graph.iter().enumerate() {
            if edges.is_empty() {
                continue;
            }
            let (from, to) = (pair / n, pair % n);
            if !self.search.schedules_transition(from, to) {
                continue;
            }
            for edge in edges {
                #[cfg(feature = "cp-search")]
                if let Some(proof) = &self.search.capacity_proof {
                    if self.search.window_cannot_fit(&edge.plan, proof, from, to) {
                        continue;
                    }
                }
                let key = measured_edge_key(edge)?;
                if self.edge_keys.insert(key.clone()) {
                    self.scheduler
                        .identities
                        .borrow_mut()
                        .insert(self.edges.len(), key);
                    self.edges.push(edge.clone());
                }
            }
        }
        if before != self.edges.len() {
            for draft in self.population.clone() {
                self.seed_branch(&draft)?;
            }
        }
        Ok(before != self.edges.len())
    }
    fn ends(&self, i: usize) -> (usize, usize) {
        let p = &self.edges[i].plan.placements;
        let index = |id: &str| {
            self.search
                .pool
                .iter()
                .position(|t| t.track_id == id)
                .unwrap()
        };
        (index(&p[0].track_id), index(&p[1].track_id))
    }
    fn assemble_measured_chain(&self, edges: &[usize]) -> Result<Option<(Vec<usize>, MixPlan)>> {
        Ok(self.assemble_measured_chain_diagnosed(edges)?.ok())
    }

    fn assemble_measured_chain_diagnosed(
        &self,
        edges: &[usize],
    ) -> Result<std::result::Result<(Vec<usize>, MixPlan), AssemblyRejection>> {
        self.assemble_measured(edges, true)
    }

    #[cfg(feature = "cp-search")]
    fn assemble_measured_for_closing(
        &self,
        edges: &[usize],
    ) -> Result<std::result::Result<(Vec<usize>, MixPlan), AssemblyRejection>> {
        self.assemble_measured(edges, false)
    }

    fn assemble_measured(
        &self,
        edges: &[usize],
        use_preferred_opening: bool,
    ) -> Result<std::result::Result<(Vec<usize>, MixPlan), AssemblyRejection>> {
        let Some(&first) = edges.first() else {
            return Ok(Err(AssemblyRejection::Unknown("empty_route".into())));
        };
        let mut plan = None;
        let mut tracks = vec![self.ends(first).0];
        let mut entry = 0;
        for (i, &id) in edges.iter().enumerate() {
            let (from, to) = self.ends(id);
            if tracks.last() != Some(&from) || tracks.contains(&to) {
                return Ok(Err(AssemblyRejection::Unknown(
                    "disconnected_or_repeated_track".into(),
                )));
            }
            let mut rejection = None;
            let edge = &self.edges[id];
            let pacing = &self.search.pacing[from];
            let next = if use_preferred_opening {
                self.search.composer.extend_diagnosed(
                    plan.as_ref(),
                    edge,
                    from,
                    entry,
                    pacing,
                    &mut rejection,
                )?
            } else {
                // An opening-specific development or duration failure cannot
                // close this prefix for the other supported openings.
                Context::append_measured(plan.as_ref(), edge, entry, pacing, &mut rejection)?
            };
            if next.is_none() {
                return Ok(Err(match rejection {
                    Some(reason) if reason != "invalid_evidence" => {
                        AssemblyRejection::Prefix { len: i + 1, reason }
                    }
                    _ => AssemblyRejection::Unknown("invalid_evidence".into()),
                }));
            }
            plan = next;
            entry = self.edges[id].entry_end;
            tracks.push(to);
        }
        Ok(Ok((tracks, plan.expect("nonempty measured chain"))))
    }

    fn block(&mut self, edges: &[usize]) -> Result<Option<Arc<Block>>> {
        if let Some(cached) = self.blocks.get(edges) {
            return Ok(cached.clone());
        }
        let Some((tracks, p)) = self.assemble_measured_chain(edges)? else {
            self.blocks.insert(edges.to_vec(), None);
            return Ok(None);
        };
        let last = *tracks.last().unwrap();
        let map = &p.placements.last().unwrap().time_map;
        let anchor = &map.anchors[map.anchors.len() - 2];
        let output_per_source =
            self.rate() / f64::from(map.source_sample_rate_hz) / map.slopes().last().unwrap();
        let maximum = anchor.output_frame
            + ((self.search.pool[last].format.frames - anchor.source_frame) as f64
                * output_per_source)
                .floor() as u64;
        // No draft fitting may remove an admitted overlap or the following song's
        // minimum development. Final musical endpoint selection happens later.
        let minimum = p.transitions.last().unwrap().end_output_frame
            + (self.track_seconds(last).mul_add(0.45, 0.).min(90.) * self.rate()).ceil() as u64;
        let block = (minimum <= maximum).then(|| {
            Arc::new(Block {
                tracks,
                edges: edges.to_vec(),
                plan: Some(p),
                minimum,
                maximum,
            })
        });
        self.blocks.insert(edges.to_vec(), block.clone());
        Ok(block)
    }
    fn paths(&self, choices: &[usize]) -> Option<Vec<Vec<usize>>> {
        let mut outgoing = BTreeMap::new();
        let mut incoming = BTreeSet::new();
        for &id in choices {
            let (a, b) = self.ends(id);
            if outgoing.insert(a, id).is_some() || !incoming.insert(b) {
                return None;
            }
        }
        let mut paths = vec![];
        let mut visited = BTreeSet::new();
        for &a in outgoing.keys().filter(|a| !incoming.contains(a)) {
            let mut path = vec![];
            let mut at = a;
            while let Some(&edge) = outgoing.get(&at) {
                if !visited.insert(edge) {
                    return None;
                }
                path.push(edge);
                at = self.ends(edge).1;
            }
            paths.push(path);
        }
        (visited.len() == choices.len()).then_some(paths)
    }
    fn candidate(&mut self, choices: &[usize], salt: usize) -> Result<Option<Draft>> {
        let Some(paths) = self.paths(choices) else {
            return Ok(None);
        };
        let mut blocks = vec![];
        let mut used = BTreeSet::new();
        for path in paths {
            let Some(block) = self.block(&path)? else {
                return Ok(None);
            };
            used.extend(block.tracks.iter().copied());
            blocks.push(block);
        }
        // Required songs and direct adjacency remain hard constraints even while
        // their musical handoff is unresolved.
        for pair in &self.search.required.pairs {
            for i in pair.indices {
                if used.insert(i) {
                    blocks.push(self.singleton(i));
                }
            }
        }
        let target = (((self.search.request.min() + self.search.request.max()) / 2.) * self.rate())
            .round() as u64;
        let minimum_target = (self.search.request.min() * self.rate()).ceil() as u64;
        let maximum_target = (self.search.request.max() * self.rate()).floor() as u64;
        let mut available: Vec<_> = (0..self.search.pool.len())
            .filter(|i| !used.contains(i))
            .collect();
        // Distinct deterministic initial arrangements; all songs remain eligible.
        if salt == 0 {
            available.sort_by(|&a, &b| self.track_seconds(b).total_cmp(&self.track_seconds(a)));
        } else {
            available.sort_by_key(|&i| shuffled_key(i, salt));
        }
        let mut capacity: u64 = blocks.iter().map(|b| b.maximum).sum();
        for i in available {
            if capacity >= target && !blocks.is_empty() {
                break;
            }
            let b = self.singleton(i);
            capacity += b.maximum;
            blocks.push(b);
        }
        if capacity < minimum_target || blocks.is_empty() {
            return Ok(None);
        }
        let minimum: u64 = blocks.iter().map(|b| b.minimum).sum();
        if minimum > maximum_target {
            return Ok(None);
        }
        let target = target
            .clamp(minimum, capacity)
            .clamp(minimum_target, maximum_target);
        if salt > 0 {
            blocks.sort_by_key(|b| shuffled_key(b.tracks[0], salt));
        }
        let order_input: Vec<_> = blocks.iter().map(|b| b.tracks.clone()).collect();
        let Some(order) =
            refinement_layout::order(&order_input, &self.search.required, self.search.pool.len())
        else {
            return Ok(None);
        };
        let blocks: Vec<_> = order.into_iter().map(|i| blocks[i].clone()).collect();
        let ranges: Vec<_> = blocks.iter().map(|b| (b.minimum, b.maximum)).collect();
        let Some(lengths) = refinement_layout::fit(&ranges, target) else {
            return Ok(None);
        };
        let score = choices
            .iter()
            .map(|&i| {
                let e = &self.edges[i];
                e.score.max(0.) / 25. + e.dense_quality_seconds + e.handoff_quality_seconds
            })
            .sum::<f64>()
            .max(0.)
            / (target as f64 / self.rate() / 60.);
        let key = canonical_hash(&json!({"blocks":blocks.iter().map(|b|(&b.tracks,&b.edges)).collect::<Vec<_>>(),"lengths":lengths}))?.digest;
        Ok(Some(Draft {
            blocks,
            lengths,
            score,
            key,
        }))
    }
    fn fixed_candidate(
        &mut self,
        order: &[usize],
        choices: &[Option<usize>],
    ) -> Result<Option<Draft>> {
        if order.is_empty()
            || choices.len() + 1 != order.len()
            || order.iter().any(|&i| i >= self.search.pool.len())
            || order.iter().copied().collect::<BTreeSet<_>>().len() != order.len()
        {
            return Ok(None);
        }
        let mut blocks = vec![];
        let mut start = 0;
        while start < order.len() {
            let mut end = start;
            let mut edges = vec![];
            while end < choices.len() {
                let Some(edge) = choices[end] else {
                    break;
                };
                if edge >= self.edges.len() || self.ends(edge) != (order[end], order[end + 1]) {
                    return Ok(None);
                }
                edges.push(edge);
                end += 1;
            }
            let block = if edges.is_empty() {
                self.singleton(order[start])
            } else {
                let Some(block) = self.block(&edges)? else {
                    return Ok(None);
                };
                block
            };
            blocks.push(block);
            start = end + 1;
        }
        let mut used = 0u128;
        for (i, &track) in order.iter().enumerate() {
            let allowed = if i == 0 {
                self.search.required.start_allowed(track)
            } else {
                self.search
                    .required
                    .allows(used, order[i - 1], track, order.len() - i - 1)
            };
            if !allowed {
                return Ok(None);
            }
            used |= 1u128 << track;
        }
        if !self.search.required.complete_used(used) {
            return Ok(None);
        }
        let ranges = blocks
            .iter()
            .map(|b| (b.minimum, b.maximum))
            .collect::<Vec<_>>();
        let lo = ranges
            .iter()
            .map(|r| r.0)
            .sum::<u64>()
            .max((self.search.request.min() * self.rate()).ceil() as u64);
        let hi = ranges
            .iter()
            .map(|r| r.1)
            .sum::<u64>()
            .min((self.search.request.max() * self.rate()).floor() as u64);
        if lo > hi {
            return Ok(None);
        }
        let target = (((self.search.request.min() + self.search.request.max()) / 2. * self.rate())
            .round() as u64)
            .clamp(lo, hi);
        let Some(lengths) = refinement_layout::fit(&ranges, target) else {
            return Ok(None);
        };
        let score = choices
            .iter()
            .flatten()
            .map(|&i| {
                let e = &self.edges[i];
                e.score.max(0.) / 25. + e.dense_quality_seconds + e.handoff_quality_seconds
            })
            .sum::<f64>()
            .max(0.)
            / (target as f64 / self.rate() / 60.);
        let key=canonical_hash(&json!({"blocks":blocks.iter().map(|b|(&b.tracks,&b.edges)).collect::<Vec<_>>(),"lengths":lengths}))?.digest;
        Ok(Some(Draft {
            blocks,
            lengths,
            score,
            key,
        }))
    }
    fn assignments(&self, draft: &Draft) -> Vec<Option<usize>> {
        let order = draft.tracks();
        let edges = draft.edges();
        order
            .windows(2)
            .map(|w| {
                edges
                    .iter()
                    .copied()
                    .find(|&i| self.ends(i) == (w[0], w[1]))
            })
            .collect()
    }
    fn seed_branch(&mut self, draft: &Draft) -> Result<()> {
        if self.edges.is_empty() {
            return Ok(());
        }
        let key =
            canonical_hash(&json!({"order":draft.tracks(),"graph_revision":self.edges.len()}))?
                .digest;
        if self.scheduler.branch_roots.insert(key) {
            if let Some(branch) = Branch::new(draft.tracks(), self.assignments(draft)) {
                self.scheduler.branches.push_back(branch);
            }
        }
        Ok(())
    }
    fn branch_step(&mut self, cancel: &Cancellation, deadline: Instant) -> Result<()> {
        if Instant::now() >= deadline {
            return Ok(());
        }
        let Some(branch) = self.scheduler.branches.pop_front() else {
            return Ok(());
        };
        let bound = branch.upper_bound_with_fit(
            |i| self.edges[i].dense_quality_seconds + self.edges[i].handoff_quality_seconds,
            |i| self.edges[i].score,
            self.search.request.min(),
            self.search.request.max(),
        );
        if bound
            .zip(self.search.incumbent.as_ref())
            .is_some_and(|(bound, best)| bound <= crate::composition::score(best))
        {
            self.scheduler.bound_pruned += 1;
            return Ok(());
        }
        if branch.leaf() {
            return Ok(());
        }
        self.scheduler.branches_expanded += 1;
        let pair = (branch.order[branch.locked], branch.order[branch.locked + 1]);
        let mut alternatives = (0..self.edges.len())
            .filter(|&i| self.ends(i) == pair)
            .collect::<Vec<_>>();
        alternatives.sort_by(|&a, &b| {
            let priority = |i: usize| {
                self.edges[i].dense_quality_seconds
                    + self.edges[i].handoff_quality_seconds
                    + self.edges[i].score.max(0.) / 25.
            };
            priority(b).total_cmp(&priority(a))
        });
        for mut child in branch.children(&alternatives).into_iter().rev() {
            cancel.check()?;
            if Instant::now() >= deadline {
                // Revisit this root on resume. Already admitted completions
                // remain deduplicated; unvisited alternatives must not vanish.
                self.scheduler.branches.push_front(branch);
                return Ok(());
            }
            // Nil is allowed in the uncommitted completion. Permanently fixing
            // a nil join can never satisfy the export contract, so this child is
            // provably infeasible. A later graph revision seeds a new root.
            if child.choices[..child.locked].iter().any(Option::is_none) {
                self.scheduler.impossible_branches += 1;
                continue;
            }
            let mut candidate = self.fixed_candidate(&child.order, &child.choices)?;
            if candidate.is_none() {
                for choice in &mut child.choices[child.locked..] {
                    *choice = None;
                }
                candidate = self.fixed_candidate(&child.order, &child.choices)?;
            }
            // Failure to fit this provisional completion does not prove that
            // all remaining cue assignments fail. Keep its branch alive.
            if let Some(candidate) = candidate {
                self.admit(candidate, cancel)?;
            }
            if !child.leaf() {
                self.scheduler.branches.push_front(child);
            }
        }
        Ok(())
    }
    /// The PCM budget limits discovery, not assembly of already measured joins.
    /// Give the final bridges a bounded chance to become a complete set, even
    /// when the last batch exhausted discovery or its measurement allowance.
    fn drain_pending(
        &mut self,
        cancel: &Cancellation,
        deadline: Instant,
        remaining_generations: usize,
    ) -> Result<()> {
        if self.edges.is_empty() || Instant::now() >= deadline {
            return Ok(());
        }
        let deadline = deadline.min(Instant::now() + std::time::Duration::from_secs(60));
        let mut rotations = 0;
        let rotation_limit = remaining_generations.min(8);
        let mut progress = Instant::now();
        for turn in 0..256 {
            cancel.check()?;
            if Instant::now() >= deadline {
                break;
            }
            let before = (
                self.seen.len(),
                self.attempted_repairs.len(),
                self.scheduler.branches.len(),
                self.scheduler.branches_expanded,
                self.scheduler.closing_attempts,
            );
            #[cfg(feature = "cp-search")]
            let closing_pending = self.close_cp_routes(
                deadline.min(Instant::now() + std::time::Duration::from_millis(250)),
                cancel,
            )?;
            #[cfg(not(feature = "cp-search"))]
            let closing_pending = false;
            for _ in 0..4 {
                self.branch_step(cancel, deadline)?;
            }
            self.repair(
                cancel,
                deadline.min(Instant::now() + std::time::Duration::from_millis(400)),
            )?;
            let after = (
                self.seen.len(),
                self.attempted_repairs.len(),
                self.scheduler.branches.len(),
                self.scheduler.branches_expanded,
                self.scheduler.closing_attempts,
            );
            let worked = before != after;
            if rotations < rotation_limit && (!worked || turn % 4 == 3) {
                if Instant::now() >= deadline {
                    break;
                }
                // Rotation also restores a parked candidate, so a retained
                // alternative can benefit from the most recent measured edge.
                self.regenerate(cancel)?;
                rotations += 1;
            } else if !worked && !closing_pending && self.scheduler.branches.is_empty() {
                break;
            }
            if progress.elapsed().as_secs() >= 10 {
                self.checkpoint("assembling_measured_graph")?;
                progress = Instant::now();
            }
        }
        Ok(())
    }
    fn draft_value(&self, draft: &Draft) -> Result<Value> {
        let mut at = 0u64;
        let mut blocks = vec![];
        let mut joins = vec![];
        for (index, (block, &length)) in draft.blocks.iter().zip(&draft.lengths).enumerate() {
            let mut passages = vec![];
            for (j, &i) in block.tracks.iter().enumerate() {
                let track = &self.search.pool[i];
                let meta = self.search.snapshot.recording_metadata(track)?;
                let mut row = json!({"track_id":track.track_id,"artist":meta["artist"],"title":meta["track"],"source_signature":self.search.cache.sources[&track.track_id]});
                if let Some(p) = &block.plan {
                    let placement = &p.placements[j];
                    let mut map = placement.time_map.clone();
                    let mut source_end = placement.source_end_frame;
                    if j + 1 == block.tracks.len() {
                        let anchor = &map.anchors[map.anchors.len() - 2];
                        source_end = anchor.source_frame
                            + ((length - anchor.output_frame) as f64 / self.rate()
                                * f64::from(map.source_sample_rate_hz)
                                * map.slopes().last().unwrap())
                            .round() as u64;
                        *map.anchors.last_mut().unwrap() = Anchor {
                            source_frame: source_end,
                            output_frame: length,
                        };
                    }
                    for a in &mut map.anchors {
                        a.output_frame += at;
                    }
                    row["source_start_frame"] = json!(placement.source_start_frame);
                    row["source_end_frame"] = json!(source_end);
                    row["time_map"] = json!(map);
                } else {
                    row["source_start_frame"] = json!(0);
                    row["source_end_frame"] = json!((length as f64 / self.rate()
                        * f64::from(track.format.sample_rate_hz))
                    .round() as u64);
                    row["output_start_frame"] = json!(at);
                    row["output_end_frame"] = json!(at + length);
                    row["playback_speed"] = json!(1.);
                }
                passages.push(row);
            }
            if let Some(p) = &block.plan {
                for (i, &edge) in block.edges.iter().enumerate() {
                    let tactic = read_tactics(&self.edges[edge].plan)?;
                    joins.push(json!({"choice":tactic[0].tactic_id,"score":self.edges[edge].score,
                        "status":"measured","from":p.transitions[i].from_track_id,"to":p.transitions[i].to_track_id,
                        "start_output_frame":at+p.transitions[i].start_output_frame,"end_output_frame":at+p.transitions[i].end_output_frame,
                        "evidence_coordinate_system":"pair-local output frames","evidence":tactic[0]}));
                }
            }
            blocks.push(
                json!({"start_output_frame":at,"end_output_frame":at+length,"passages":passages,
                "tail_endpoint":"provisional; requires musical endpoint validation before export"}),
            );
            at += length;
            if index + 1 < draft.blocks.len() {
                joins.push(json!({"choice":"nil","score":0.,"status":"unresolved",
                    "from":self.search.pool[*block.tracks.last().unwrap()].track_id,
                    "to":self.search.pool[draft.blocks[index+1].tracks[0]].track_id,
                    "start_output_frame":at,"end_output_frame":at,
                    "reason":"No approved musical handoff assigned; provisional concatenation at native tail speed."}));
            }
        }
        Ok(
            json!({"kind":"set_draft","version":1,"full_duration":true,"exportable":false,
            "sample_rate_hz":self.search.policy.output_sample_rate_hz,"total_output_frames":at,
            "duration_seconds":at as f64/self.rate(),"nil_joins":draft.nil(),"resolved_joins":draft.edges().len(),
            "score":draft.score,"score_semantics":"Nil earns exactly zero. Resolve fewer nil joins first, then maximize measured fit and sustained overlap per minute. Final plans use the full composition objective.",
            "blocks":blocks,"joins":joins,"required_pairs":self.search.required.evidence(),
            "sources":self.search.cache.sources,"musical_policy":self.search.policy,
            "set_key":Cache::set_key(self.search.policy)?}),
        )
    }
    fn checkpoint(&mut self, status: &str) -> Result<()> {
        self.checkpoint_with_stop(status, None)
    }

    fn checkpoint_with_stop(&mut self, status: &str, stop: Option<StopReason>) -> Result<()> {
        let checkpoint_started = Instant::now();
        let draft = self
            .best
            .as_ref()
            .map(|best| self.draft_value(best))
            .transpose()?
            .unwrap_or(Value::Null);
        self.draft_report
            .header
            .extensions
            .insert("draft".into(), draft);
        self.draft_report.header.extensions.insert("search".into(),json!({"method":"complete-set-refinement/1","status":status,
            "prepared_tracks":self.search.pool.len(),"elapsed_seconds":self.search.started.elapsed().as_secs_f64(),
            "cache_mode":self.search.cache.mode,
            "measured_windows":self.search.stats.measured_windows,"cached_windows":self.search.stats.cached_windows,
            "selector_refresh_attempts":self.search.stats.selector_refresh_attempts,
            "capacity_skipped_windows":self.search.stats.capacity_skipped_windows,
            "in_flight_pcm_overrun_seconds":self.search.stats.in_flight_pcm_overrun_seconds,
            "known_good_choices":self.edges.len(),"complete_drafts_examined":self.seen.len(),"rounds":self.rounds,
            "duplicate_measured_choices":self.search.stats.duplicate_measured_choices,
            "rejected_layouts":self.rejected_layouts,"history":self.history,
            "generated_layouts":self.scheduler.generation,"global_pair_cursor":self.scheduler.pool_cursor,"proposed_ordered_pairs":self.scheduler.proposed_pairs.len(),"measured_ordered_pairs":self.scheduler.measured_pairs.len(),"closing_attempts":self.scheduler.closing_attempts,"closing_diagnostics":self.scheduler.closing_diagnostics,"parked_candidates":self.scheduler.parked.len(),
            "bound_pruned":self.scheduler.bound_pruned,"fixed_order_branches":self.scheduler.branches.len(),"branches_expanded":self.scheduler.branches_expanded,"infeasible_committed_nil_branches":self.scheduler.impossible_branches,"optimistic_objective_bound":unrestricted_upper_bound(self.search.pool.len(),self.search.request),
            "bound_scope":"Unrestricted replacements under the per-join credit cap and disjoint overlap clock. Future penalties are relaxed.",
            "best_plan_id":self.search.incumbent.as_ref().map(|p|&p.plan_id)}));
        if let Some(stop) = stop {
            let search = self
                .draft_report
                .header
                .extensions
                .get_mut("search")
                .unwrap();
            search["stop_reason"] = json!(stop);
            search["termination"] = json!(match (stop, self.search.incumbent.is_some()) {
                #[cfg(feature = "cp-search")]
                (StopReason::DirectedDurationCapacity | StopReason::RequiredAdjacencyChain, _) =>
                    "infeasible_within_domain",
                (StopReason::GeneratedProbeScope, _) => "proposal_scope_exhausted",
                (StopReason::Cancelled, true) => "cancelled_with_incumbent",
                (StopReason::Cancelled, false) => "cancelled_without_incumbent",
                (_, true) => "budget_with_incumbent",
                (_, false) => "budget_without_incumbent",
            });
            search["best_verified_objective"] = json!(self
                .search
                .incumbent
                .as_ref()
                .map(crate::composition::score));
            search["certified_global_upper_bound"] = Value::Null;
            search["certified_absolute_gap"] = Value::Null;
        }
        #[cfg(feature = "cp-search")]
        if let Some(cp) = &self.cp {
            let coverage = self.search.score_coverage();
            let search = self
                .draft_report
                .header
                .extensions
                .get_mut("search")
                .unwrap();
            search["certified_global_upper_bound"] = coverage["global_upper"].clone();
            search["certified_absolute_gap"] = coverage["absolute_gap_upper"].clone();
            match stop {
                Some(StopReason::DirectedDurationCapacity) => {
                    search["infeasibility_proof"] =
                        json!(self.search.duration_infeasibility_evidence());
                }
                Some(StopReason::RequiredAdjacencyChain) => {
                    let chain = self
                        .search
                        .blocked_required_chain()
                        .expect("required chain proof");
                    search["infeasibility_proof"] =
                        self.search.required_chain_infeasibility_evidence(&chain);
                }
                _ => {}
            }
            let mut cp_evidence = cp.evidence();
            cp_evidence["objective_bound"] = coverage["global_upper"].clone();
            cp_evidence["score_coverage"] = coverage;
            if let Some(frontier) = &self.search.continuations {
                cp_evidence["continuation_frontier"] =
                    frontier.evidence(&self.search.measured_keys);
            }
            cp_evidence["window_oracle"] = self.search.oracle.evidence();
            cp_evidence["archive_revalidation"] = json!(self.scheduler.archive_revalidation);
            search["cp_search"] = cp_evidence;
            if self.search.budget_open() {
                cp.save_cue_frontiers(&self.search)?;
            }
            if self.search.budget_open() {
                cp.save_conflicts(self)?;
            }
        }
        if self.scheduler.cache_loaded && self.search.budget_open() {
            self.save_best()?;
            if self.search.budget_open()
                && self
                    .scheduler
                    .frontier_saved
                    .is_none_or(|at| at.elapsed().as_secs() >= 60)
            {
                self.save_frontier()?;
                self.scheduler.frontier_saved = Some(Instant::now());
            }
        }
        let search = self
            .draft_report
            .header
            .extensions
            .get_mut("search")
            .unwrap();
        search["elapsed_seconds"] = json!(self.search.started.elapsed().as_secs_f64());
        search["checkpoint_seconds"] = json!(checkpoint_started.elapsed().as_secs_f64());
        search["checkpoint_deadline_reached"] = json!(!self.search.budget_open());
        self.search.store.report(&self.draft_report)?;
        self.log_progress(status);
        Ok(())
    }

    fn log_progress(&self, status: &str) {
        let Some(best) = &self.best else {
            eprintln!(
                "set refinement: {status} | no full draft | {:.1}s",
                self.search.started.elapsed().as_secs_f64()
            );
            return;
        };
        let approved = self.search.incumbent.as_ref().map_or_else(
            || "not yet approved".to_owned(),
            |plan| {
                let seconds = plan.total_output_frames / u64::from(plan.output.sample_rate_hz);
                format!(
                    "approved {:02}:{:02} / {} tracks",
                    seconds / 60,
                    seconds % 60,
                    plan.placements.len()
                )
            },
        );
        eprintln!("set refinement: {status} | {approved} | full {:.2} min draft | {} nil / {} measured draft joins | draft score {:.4} | {} fresh / {} cached windows | {:.1}s",
            best.lengths.iter().sum::<u64>() as f64/self.rate()/60.,best.nil(),best.edges().len(),best.score,
            self.search.stats.measured_windows,self.search.stats.cached_windows,self.search.started.elapsed().as_secs_f64());
    }
    fn try_close(
        &mut self,
        edges: &[usize],
        plan: &MixPlan,
        origin: &str,
        cancel: &Cancellation,
    ) -> Result<()> {
        if !self.search.required.satisfied(plan) {
            return Ok(());
        }
        #[cfg(feature = "cp-search")]
        if self.cp.is_some() {
            let keys = self.closing_route_keys(edges)?;
            let closing = &mut self.cp.as_mut().unwrap().closing;
            closing.queue(edges, keys)?;
            closing.checkpoint_routes(&self.search)?;
            return Ok(());
        }
        if !self.search.budget_open() {
            return Ok(());
        }
        if self.attempted_final.insert(edges.to_vec()) {
            self.scheduler.closing_attempts += 1;
            let (finished, diagnostics) =
                self.search
                    .composer
                    .close_with_diagnostics(&self.search.snapshot, plan, cancel)?;
            self.scheduler.closing_diagnostics.merge(&diagnostics);
            if let Some(finished) = finished {
                self.search.save(finished, origin, cancel)?;
            }
        }
        Ok(())
    }

    fn admit(&mut self, draft: Draft, cancel: &Cancellation) -> Result<()> {
        if self.search.incumbent.as_ref().is_some_and(|p| {
            unrestricted_upper_bound(self.search.pool.len(), self.search.request)
                <= crate::composition::score(p)
        }) {
            self.scheduler.bound_pruned += 1;
            return Ok(());
        }
        if !self.seen.insert(draft.key.clone()) {
            return Ok(());
        }
        // A resolved component can already fill the hour using verified phrase
        // replays. Do not force a raw57-minute chain before asking the composer.
        for block in &draft.blocks {
            if !closing_capacity(block.maximum, self.rate(), self.search.request.min()) {
                continue;
            }
            if let Some(plan) = &block.plan {
                self.try_close(&block.edges, plan, "whole_set_refinement", cancel)?;
            }
        }
        if self.best.as_ref().is_none_or(|old| draft.better(old)) {
            self.best = Some(draft.clone());
            self.history.push(json!({"nil_joins":draft.nil(),"resolved_joins":draft.edges().len(),"score":draft.score,
                "elapsed_seconds":self.search.started.elapsed().as_secs_f64(),"track_ids":draft.tracks().iter().map(|&i| &self.search.pool[i].track_id).collect::<Vec<_>>()}));
            if self.history.len() == 1 {
                self.checkpoint("FULL_DRAFT_FOUND")?;
            } else {
                if self.scheduler.cache_loaded && self.search.budget_open() {
                    self.save_best()?;
                }
                self.log_progress("BETTER_DRAFT_FOUND");
            }
        }
        self.seed_branch(&draft)?;
        self.population.push(draft);
        Ok(())
    }
    fn continuation_state(&self, draft: &Draft) -> Option<(usize, u64, Vec<usize>)> {
        let block = draft
            .blocks
            .iter()
            .filter(|b| !b.edges.is_empty())
            .max_by_key(|b| b.maximum)?;
        let last = *block.tracks.last()?;
        let entry = self.edges[*block.edges.last()?].entry_end;
        let mut used = block.tracks.clone();
        used.sort_unstable();
        Some((last, entry, used))
    }
    fn prune(&mut self) {
        self.population.sort_by(|a, b| {
            a.nil()
                .cmp(&b.nil())
                .then_with(|| b.score.total_cmp(&a.score))
        });
        let mut kept: Vec<_> = self.population.iter().take(12).cloned().collect();
        // Keep strong mashup alternatives even when they temporarily leave more
        // nil joins. This avoids a single greedy chain freezing the search.
        self.population.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.nil().cmp(&b.nil()))
        });
        for d in &self.population {
            if kept.len() >= 24 {
                break;
            }
            if !kept.iter().any(|k| k.key == d.key) {
                kept.push(d.clone());
            }
        }
        // Long resolved components are useful stepping stones toward duration,
        // even when ordinary blends give them little sustained-quality credit.
        self.population.sort_by(|a, b| {
            let length = |d: &Draft| {
                d.blocks
                    .iter()
                    .filter(|b| !b.edges.is_empty())
                    .map(|b| b.maximum)
                    .max()
                    .unwrap_or(0)
            };
            length(b)
                .cmp(&length(a))
                .then_with(|| b.score.total_cmp(&a.score))
        });
        let limit = kept.len() + 8;
        let mut states = kept
            .iter()
            .filter_map(|d| self.continuation_state(d))
            .collect::<BTreeSet<_>>();
        let mut terminals = states.iter().map(|s| s.0).collect::<BTreeSet<_>>();
        // Extending a component depends on its last song, entry cue and which
        // songs remain unused. Eight variants of one blocked endpoint are not
        // eight useful duration alternatives. Cover new terminal songs first.
        for draft in &self.population {
            if kept.len() >= limit {
                break;
            }
            if let Some(state) = self.continuation_state(draft) {
                if terminals.insert(state.0) {
                    states.insert(state);
                    kept.push(draft.clone());
                }
            }
        }
        // Then preserve alternate entry/used-song states at known endpoints.
        // Exact measured entry coordinates avoid inventing an admission gate.
        for draft in &self.population {
            if kept.len() >= limit {
                break;
            }
            if let Some(state) = self.continuation_state(draft) {
                if states.insert(state) {
                    kept.push(draft.clone());
                }
            }
        }
        // If the graph has fewer distinct continuation states, fill the rest
        // with its longest resolved alternatives rather than dropping capacity.
        for draft in &self.population {
            if kept.len() >= limit {
                break;
            }
            if self.continuation_state(draft).is_some() && !kept.iter().any(|k| k.key == draft.key)
            {
                kept.push(draft.clone());
            }
        }
        for draft in self.population.drain(..) {
            if !kept.iter().any(|k| k.key == draft.key) {
                self.scheduler.parked.push_back(draft);
            }
        }
        self.population = kept;
    }
    fn regenerate(&mut self, cancel: &Cancellation) -> Result<()> {
        self.scheduler.generation += 1;
        let salt = self.scheduler.generation;
        // New seeds are drawn throughout the run, including at a plateau.
        // Alternatives retained on disk are scheduling choices, not pruned proofs.
        if let Some(draft) = self.candidate(&[], salt)? {
            self.admit(draft, cancel)?;
        }
        if !self.edges.is_empty() {
            if let Some(draft) = self.candidate(&[salt % self.edges.len()], salt)? {
                self.admit(draft, cancel)?;
            }
        }
        if let Some(parked) = self.scheduler.parked.pop_front() {
            if !self.population.iter().any(|d| d.key == parked.key) {
                self.population.push(parked);
            }
        }
        Ok(())
    }
    #[cfg(feature = "cp-search")]
    fn closing_route_keys(&self, edges: &[usize]) -> Result<Vec<String>> {
        edges.iter().map(|&i| self.persistent_key(i)).collect()
    }

    fn persistent_key(&self, index: usize) -> Result<String> {
        if let Some(key) = self.scheduler.identities.borrow().get(&index) {
            return Ok(key.clone());
        }
        let key = measured_edge_key(&self.edges[index])?;
        self.scheduler
            .identities
            .borrow_mut()
            .insert(index, key.clone());
        Ok(key)
    }
    fn save_frontier(&self) -> Result<()> {
        let key = Cache::set_key(self.search.policy)?;
        let seeds=self.best.iter().chain(&self.population).chain(&self.scheduler.parked).map(|d| {
            let choices=self.assignments(d).into_iter().map(|i|i.map(|i|self.persistent_key(i)).transpose()).collect::<Result<Vec<_>>>()?;
            Ok(json!({"choices":choices,"tracks":d.tracks().iter().map(|&i|&self.search.pool[i].track_id).collect::<Vec<_>>() }))
        }).collect::<Result<Vec<_>>>()?;
        let Some(parts) = frontier_parts(
            &self.search.cache,
            self.search.store,
            seeds,
            self.search.deadline,
        )?
        else {
            return Ok(());
        };
        // Persist fixed-order work separately. Rebuild/rescore every restored
        // completion; no stale draft admission is trusted.
        let branches=self.scheduler.branches.iter().map(|b| {
            let choices=b.choices.iter().map(|i|i.map(|i|self.persistent_key(i)).transpose()).collect::<Result<Vec<_>>>()?;
            Ok(json!({"choices":choices,"tracks":b.order.iter().map(|&i|&self.search.pool[i].track_id).collect::<Vec<_>>(),"locked":b.locked}))
        }).collect::<Result<Vec<_>>>()?;
        let Some(branch_parts) = frontier_parts(
            &self.search.cache,
            self.search.store,
            branches,
            self.search.deadline,
        )?
        else {
            return Ok(());
        };
        if !self.search.budget_open() {
            return Ok(());
        }
        self.search.cache.write(self.search.store,"refinement-frontiers",&key,&json!({
            "version":2,"sources":self.search.cache.sources,"generation":self.scheduler.generation,"pool_cursor":self.scheduler.pool_cursor,"parts":parts,"branch_parts":branch_parts}))
    }
    fn save_best(&self) -> Result<()> {
        let Some(best) = &self.best else {
            return Ok(());
        };
        let choices = self
            .assignments(best)
            .into_iter()
            .map(|i| i.map(|i| self.persistent_key(i)).transpose())
            .collect::<Result<Vec<_>>>()?;
        let key = Cache::set_key(self.search.policy)?;
        self.search.cache.write(self.search.store, "refinement-best", &key, &json!({
            "version":1,"sources":self.search.cache.sources,
            "generation":self.scheduler.generation,"pool_cursor":self.scheduler.pool_cursor,
            "tracks":best.tracks().iter().map(|&i| &self.search.pool[i].track_id).collect::<Vec<_>>(),
            "choices":choices
        }))
    }
    fn restore_best(&mut self, cancel: &Cancellation) -> Result<()> {
        let key = Cache::set_key(self.search.policy)?;
        let Some(seed) =
            self.search
                .cache
                .read::<Value>(self.search.store, "refinement-best", &key)?
        else {
            return Ok(());
        };
        let valid = seed["sources"].as_object().is_some_and(|sources| {
            sources.iter().all(|(id, key)| {
                self.search
                    .cache
                    .sources
                    .get(id)
                    .is_some_and(|v| key.as_str() == Some(v))
            })
        });
        if !valid {
            self.search.cache.invalid += 1;
            return Ok(());
        }
        let lookup = self
            .edges
            .iter()
            .enumerate()
            .map(|(i, _)| Ok((self.persistent_key(i)?, i)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let Some(order) = seed["tracks"].as_array().and_then(|v| {
            v.iter()
                .map(|id| {
                    self.search
                        .pool
                        .iter()
                        .position(|t| Some(t.track_id.as_str()) == id.as_str())
                })
                .collect::<Option<Vec<_>>>()
        }) else {
            return Ok(());
        };
        let Some(choices) = seed["choices"].as_array().and_then(|v| {
            v.iter()
                .map(|key| {
                    if key.is_null() {
                        Some(None)
                    } else {
                        Some(Some(*lookup.get(key.as_str()?)?))
                    }
                })
                .collect::<Option<Vec<_>>>()
        }) else {
            return Ok(());
        };
        if let Some(draft) = self.fixed_candidate(&order, &choices)? {
            self.admit(draft, cancel)?;
            self.scheduler.generation = self
                .scheduler
                .generation
                .max(seed["generation"].as_u64().unwrap_or(0) as usize);
            self.scheduler.pool_cursor = self
                .scheduler
                .pool_cursor
                .max(seed["pool_cursor"].as_u64().unwrap_or(0) as usize);
        }
        Ok(())
    }
    fn restore_incumbent(&mut self, cancel: &Cancellation) -> Result<()> {
        // Rebuilding frontier candidates may immediately close and save a set.
        // Install the durable incumbent first so those completions compete with
        // it instead of overwriting the saved winner while incumbent is empty.
        if let Some(plan) = self
            .search
            .cache
            .saved_set(self.search.store, self.search.policy)?
        {
            if self.search.required.satisfied(&plan) {
                self.search.save(plan, "cached_complete_set", cancel)?;
            }
        }
        Ok(())
    }
    fn restore_frontier(&mut self, cancel: &Cancellation) -> Result<()> {
        let key = Cache::set_key(self.search.policy)?;
        if let Some(saved) =
            self.search
                .cache
                .read::<Value>(self.search.store, "refinement-frontiers", &key)?
        {
            let valid = saved["sources"].as_object().is_some_and(|sources| {
                sources.iter().all(|(id, key)| {
                    self.search
                        .cache
                        .sources
                        .get(id)
                        .is_some_and(|v| key.as_str() == Some(v))
                })
            });
            if valid {
                self.scheduler.generation = saved["generation"].as_u64().unwrap_or(0) as usize;
                self.scheduler.pool_cursor = saved["pool_cursor"].as_u64().unwrap_or(0) as usize;
                let lookup = self
                    .edges
                    .iter()
                    .enumerate()
                    .map(|(i, _)| Ok((self.persistent_key(i)?, i)))
                    .collect::<Result<BTreeMap<_, _>>>()?;
                let mut restored = 0usize;
                for kind in ["parts", "branch_parts"] {
                    let mut parts = saved[kind]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>();
                    if kind == "parts" && saved["version"].as_u64() != Some(2) {
                        // One-time migration: an old budget-dependent identifier
                        // could orphan the better predecessor frontier. Immutable
                        // chunks survive; reconstruct only current measured edges.
                        parts.extend(
                            self.search
                                .cache
                                .keys(self.search.store, "refinement-frontier-parts")?,
                        );
                        parts.sort();
                        parts.dedup();
                    }
                    for part in parts {
                        cancel.check()?;
                        if !self.search.budget_open() {
                            return Ok(());
                        }
                        let Some(seeds) = self.search.cache.read::<Vec<Value>>(
                            self.search.store,
                            "refinement-frontier-parts",
                            &part,
                        )?
                        else {
                            continue;
                        };
                        for seed in seeds {
                            if !self.search.budget_open() {
                                return Ok(());
                            }
                            let Some(order) = seed["tracks"].as_array().and_then(|v| {
                                v.iter()
                                    .map(|id| {
                                        self.search
                                            .pool
                                            .iter()
                                            .position(|t| Some(t.track_id.as_str()) == id.as_str())
                                    })
                                    .collect::<Option<Vec<_>>>()
                            }) else {
                                continue;
                            };
                            let Some(choices) = seed["choices"].as_array().and_then(|v| {
                                v.iter()
                                    .map(|key| {
                                        if key.is_null() {
                                            Some(None)
                                        } else {
                                            Some(Some(*lookup.get(key.as_str()?)?))
                                        }
                                    })
                                    .collect::<Option<Vec<_>>>()
                            }) else {
                                continue;
                            };
                            if kind == "branch_parts" {
                                if let Some(mut branch) = Branch::new(order, choices) {
                                    branch.locked = seed["locked"].as_u64().unwrap_or(0) as usize;
                                    if branch.locked <= branch.choices.len() {
                                        self.scheduler.branches.push_back(branch);
                                    }
                                }
                            } else if let Some(draft) = self.fixed_candidate(&order, &choices)? {
                                restored += 1;
                                self.admit(draft, cancel)?;
                            }
                        }
                    }
                }
                self.prune();
                eprintln!("set refinement: restored {restored} complete candidates and {} pending fixed-order branches",self.scheduler.branches.len());
            } else {
                self.search.cache.invalid += 1;
            }
        }
        if !self.search.budget_open() {
            return Ok(());
        }
        self.restore_best(cancel)?;
        self.scheduler.cache_loaded = true;
        self.scheduler.frontier_saved = Some(Instant::now());
        Ok(())
    }
    fn repair(&mut self, cancel: &Cancellation, deadline: Instant) -> Result<()> {
        loop {
            self.rounds += 1;
            let before = self.seen.len();
            let parents = self.population.clone();
            let mut ids: Vec<_> = (0..self.edges.len()).collect();
            ids.sort_by(|&a, &b| {
                let benefit = |i: usize| {
                    self.edges[i].dense_quality_seconds
                        + self.edges[i].handoff_quality_seconds
                        + self.edges[i].score.max(0.) / 25.
                };
                benefit(b).total_cmp(&benefit(a))
            });
            let cursor = self.scheduler.repair_cursor % ids.len().max(1);
            ids.rotate_left(cursor);
            for (offset, &edge) in ids.iter().enumerate() {
                self.scheduler.repair_cursor = (cursor + offset + 1) % ids.len().max(1);
                for parent in &parents {
                    cancel.check()?;
                    if Instant::now() >= deadline {
                        self.prune();
                        return Ok(());
                    }
                    if !self.attempted_repairs.insert((parent.key.clone(), edge)) {
                        continue;
                    }
                    let (a, b) = self.ends(edge);
                    let mut choices: Vec<_> = parent
                        .edges()
                        .into_iter()
                        .filter(|&i| {
                            let (x, y) = self.ends(i);
                            x != a && y != b
                        })
                        .collect();
                    choices.push(edge);
                    // If inserting an edge closes a cycle, open its weakest old
                    // edge. Every result is rebuilt and duration-fitted in full.
                    if self.paths(&choices).is_none() {
                        if let Some(remove) = choices
                            .iter()
                            .copied()
                            .filter(|&i| i != edge)
                            .min_by(|&x, &y| self.edges[x].score.total_cmp(&self.edges[y].score))
                        {
                            choices.retain(|&i| i != remove);
                        }
                    }
                    if let Some(candidate) = self.candidate(&choices, 0)? {
                        self.admit(candidate, cancel)?;
                    } else {
                        self.rejected_layouts += 1;
                        // A changed entry may make the old exit impossible. Try
                        // replacing those neighboring decisions as one move.
                        choices.retain(|&i| {
                            i == edge || {
                                let (x, y) = self.ends(i);
                                y != a && x != b
                            }
                        });
                        if let Some(candidate) = self.candidate(&choices, 0)? {
                            self.admit(candidate, cancel)?;
                        }
                    }
                }
            }
            self.prune();
            if self.seen.len() == before {
                break;
            }
        }
        Ok(())
    }
    fn minimum_after(&self, track: usize, entry: u64) -> u64 {
        let rate = f64::from(self.search.pool[track].format.sample_rate_hz);
        let development = (90f64).min(self.track_seconds(track) * 0.45)
            * (1. + self.search.policy.max_speed_deviation);
        self.search.pacing[track]
            .earliest_exit(entry)
            .max(entry.saturating_add((development * rate).ceil() as u64))
    }
    fn demands(&self) -> Vec<Probe> {
        let mut output = vec![];
        let mut seen = BTreeSet::new();
        #[cfg(feature = "cp-search")]
        if let Some(cp) = &self.cp {
            for &probe in cp.demands() {
                if seen.insert(probe) {
                    output.push(probe);
                }
            }
        }
        let drafts = self.best.iter().chain(self.population.iter().take(12));
        for draft in drafts {
            // Direct unresolved boundaries first, then alternatives that reorder
            // retained components. A separate global lane cannot be starved.
            let mut pairs = (0..draft.blocks.len().saturating_sub(1))
                .map(|i| (i, i + 1))
                .collect::<Vec<_>>();
            for a in 0..draft.blocks.len() {
                for b in 0..draft.blocks.len() {
                    if a != b {
                        pairs.push((a, b));
                    }
                }
            }
            for (a, b) in pairs {
                let from = *draft.blocks[a].tracks.last().unwrap();
                let to = draft.blocks[b].tracks[0];
                if !self.search.schedules_transition(from, to) {
                    continue;
                }
                let entry = draft.blocks[a]
                    .edges
                    .last()
                    .map_or(0, |&i| self.edges[i].entry_end);
                let minimum = self.minimum_after(from, entry);
                let frames = self.search.pool[from].format.frames;
                for fraction in [0.60, 0.80, 0.35, 0.] {
                    let probe = Probe {
                        from,
                        to,
                        after: minimum.max((frames as f64 * fraction) as u64),
                        tier: 1,
                    };
                    if probe.after < frames && seen.insert(probe) {
                        output.push(probe);
                    }
                }
                // Event/long-hold proposals may earn development credit and keep
                // the original phrase-aware lower bound available as well.
                for tier in [0, 24] {
                    let probe = Probe {
                        from,
                        to,
                        after: self.search.pacing[from].earliest_exit(entry),
                        tier,
                    };
                    if seen.insert(probe) {
                        output.push(probe);
                    }
                }
            }
        }
        output
    }
}

pub(super) fn run(
    store: &Store,
    policy: &MixingConfig,
    context: usize,
    pool: Option<Vec<Track>>,
    request: Request,
    started: Instant,
    cancel: &Cancellation,
) -> Result<Value> {
    let search = new_search(store, policy, context, pool, request, started, cancel)?;
    run_prepared(Refiner::new(search), cancel)
}

fn run_prepared(mut r: Refiner<'_>, cancel: &Cancellation) -> Result<Value> {
    let policy = r.search.policy;
    let option = |key: &str, default: u64| {
        policy
            .header
            .extensions
            .get(key)
            .and_then(Value::as_u64)
            .unwrap_or(default)
    };
    let budget_seconds = option("lazy_search_seconds", 1200);
    let deadline = r
        .search
        .started
        .checked_add(std::time::Duration::from_secs(budget_seconds))
        .ok_or_else(|| {
            Error::new(
                "SET_SEARCH_BUDGET",
                "search deadline is outside the supported clock range",
            )
        })?;
    r.search.deadline = Some(deadline);
    let measurement_limit = option("lazy_measurement_budget", 1536) as usize;
    r.search.snapshot.verify_read_snapshot(cancel)?;
    #[cfg(feature = "cp-search")]
    if let Some(cp) = &mut r.cp {
        r.search.capacity_proof = r.search.directed_capacity();
        cp.bind_manifest(&r.search)?;
    }
    #[cfg(feature = "cp-search")]
    if let Some(proof) = r
        .search
        .structural_infeasibility()
        .filter(|_| r.cp.is_some())
    {
        let reason = match proof {
            StructuralInfeasibility::DirectedCapacity => StopReason::DirectedDurationCapacity,
            StructuralInfeasibility::RequiredChain => StopReason::RequiredAdjacencyChain,
        };
        let preparation_seconds = r.search.started.elapsed().as_secs_f64();
        return publish_result(
            r,
            "infeasible",
            reason,
            budget_seconds,
            measurement_limit,
            preparation_seconds,
            cancel,
        );
    }
    let cp_enabled = cfg!(feature = "cp-search")
        && policy
            .header
            .extensions
            .get(crate::set_config::ENGINE_KEY)
            .and_then(Value::as_str)
            == Some("cp");
    for salt in 0..8 {
        if (cp_enabled || r.best.is_some())
            && (cancel.check().is_err() || (salt > 0 && !r.search.budget_open()))
        {
            break;
        }
        if let Some(d) = r.candidate(&[], salt)? {
            r.admit(d, cancel)?;
        }
    }
    require(
        r.best.is_some() || cp_enabled,
        "SET_DRAFT_CAPACITY",
        "cannot fit a full draft from available source capacity and required adjacencies",
    )?;
    let mut preparation_seconds = r.search.started.elapsed().as_secs_f64();
    let outcome = r.restore_incumbent(cancel).and_then(|()| {
        refine(
            &mut r,
            deadline,
            measurement_limit,
            &mut preparation_seconds,
            cancel,
        )
    });
    let (status, stop) = match outcome {
        Ok(outcome) => outcome,
        Err(error) if error.code == "CANCELLED" => ("cancelled", StopReason::Cancelled),
        Err(error) => return Err(error),
    };
    publish_result(
        r,
        status,
        stop,
        budget_seconds,
        measurement_limit,
        preparation_seconds,
        cancel,
    )
}

fn refine(
    r: &mut Refiner<'_>,
    deadline: Instant,
    measurement_limit: usize,
    preparation_seconds: &mut f64,
    cancel: &Cancellation,
) -> Result<(&'static str, StopReason)> {
    *preparation_seconds = r.search.started.elapsed().as_secs_f64();
    r.search.load_known(cancel)?;
    r.sync_edges()?;
    #[cfg(feature = "cp-search")]
    if let Some(mut cp) = r.cp.take() {
        let restored: Result<()> = (|| {
            if r.search.budget_open() {
                cp.restore_cue_frontiers(&mut r.search, cancel)?;
            }
            if r.search.budget_open() {
                cp.closing.restore(&mut r.search, &r.edges, cancel)?;
            }
            if r.search.budget_open() {
                cp.restore_conflicts(r, cancel)?;
            }
            Ok(())
        })();
        r.cp = Some(cp);
        restored?;
    }
    #[cfg(feature = "cp-search")]
    if r.search.budget_open() {
        r.scheduler.archive_revalidation =
            cp_archive::recover(r, measurement_limit, deadline, cancel)?;
    }
    if r.search.budget_open() {
        r.restore_frontier(cancel)?;
    }
    #[cfg(feature = "cp-search")]
    r.bootstrap_measured_incumbent(
        deadline.min(Instant::now() + std::time::Duration::from_secs(12)),
        cancel,
    )?;
    r.repair(
        cancel,
        deadline.min(Instant::now() + std::time::Duration::from_secs(1)),
    )?;
    let policy = r.search.policy;
    let mut discovery = Schedule::new(r.scheduler.pool_cursor);
    let frames = r
        .search
        .pool
        .iter()
        .map(|t| t.format.frames)
        .collect::<Vec<_>>();
    let minimum = (0..r.search.pool.len())
        .map(|i| r.minimum_after(i, 0))
        .collect::<Vec<_>>();
    let mut measured = BTreeSet::new();
    let mut pending = VecDeque::new();
    let mut exploration = VecDeque::from(crate::historical_seeds::load(
        r.search.store,
        policy,
        &r.search.pool,
        &r.search.cache,
        deadline,
        cancel,
    )?);
    let mut progress = Instant::now();
    let mut status = "refinement_budget";
    let generation_limit = measurement_limit.saturating_mul(2).clamp(64, 10000);
    let generation_start = r.scheduler.generation;
    let mut turns = 0usize;
    while Instant::now() < deadline && r.search.stats.measured_windows < measurement_limit {
        cancel.check()?;
        if progress.elapsed().as_secs() >= 10 {
            r.checkpoint("refining")?;
            progress = Instant::now();
        }
        if !r.search.budget_open() {
            break;
        }
        turns += 1;
        #[cfg(feature = "cp-search")]
        if turns % 4 == 1 {
            r.propose_cp_route(deadline, cancel)?;
        }
        #[cfg(feature = "cp-search")]
        r.close_cp_routes(
            deadline.min(Instant::now() + std::time::Duration::from_millis(250)),
            cancel,
        )?;
        r.branch_step(cancel, deadline)?;
        if !r.search.budget_open() {
            break;
        }
        if turns % 4 == 0 && r.scheduler.generation - generation_start < generation_limit {
            r.regenerate(cancel)?;
        }
        let mut batch = vec![];
        let batch_limit = 4.min(measurement_limit - r.search.stats.measured_windows);
        // Give a rejected closing arrival one fresh measurement slot on
        // alternating turns, even when SCIP never returns a mixed route.
        // The other turns and remaining slots retain their usual schedules.
        #[cfg(feature = "cp-search")]
        if turns % 2 == 1 && Instant::now() < deadline {
            batch.extend(r.schedule_closing_window(
                &mut discovery,
                &frames,
                &minimum,
                &mut measured,
                &mut exploration,
                cancel,
            )?);
        }
        // Keep a bounded lane for unranked cue coverage alongside the existing
        // musical proposals. The cursor stays put until the window is durable.
        #[cfg(feature = "cp-search")]
        let cue_page = if let Some(cp) =
            r.cp.as_mut()
                .filter(|_| r.search.budget_open() && batch.len() < batch_limit)
        {
            cp.schedule_cue_page(&mut r.search, turns, &mut measured, &mut batch, cancel)?
        } else {
            None
        };
        let improving_cp = policy
            .header
            .extensions
            .get(crate::set_config::ENGINE_KEY)
            .and_then(Value::as_str)
            == Some("cp")
            && r.search.incumbent.is_some();
        let mut probes = 0;
        while batch.len() < batch_limit && Instant::now() < deadline {
            // Feasibility prioritizes baseline coverage. With an approved
            // incumbent the CP engine alternates baseline and quality probes,
            // including when the remaining measurement allowance is one.
            let quality_turn = if improving_cp {
                (turns + batch.len()) % 2 == 0
            } else {
                batch.len() == 3
            };
            let seed = if quality_turn {
                r.search
                    .take_quality_seed(&mut exploration, &measured)
                    .or_else(|| pending.pop_front())
            } else {
                pending.pop_front()
            };
            if let Some(seed) = seed {
                if !r.search.schedules_plan(&seed) {
                    r.search.stats.capacity_skipped_windows += 1;
                    continue;
                }
                let key = window_key(&r.search.cache, &seed)?;
                if measured.insert(key) {
                    batch.push(seed);
                }
                continue;
            }
            let targets = r.demands();
            let probe = if improving_cp {
                discovery.next_for_improvement(&targets, &frames, &minimum)
            } else {
                discovery.next(&targets, &frames, &minimum)
            };
            let Some(probe) = probe else {
                if let Some(seed) = r.search.take_quality_seed(&mut exploration, &measured) {
                    pending.push_back(seed);
                    continue;
                }
                status = "generated_probe_scope_exhausted";
                break;
            };
            r.scheduler.pool_cursor = discovery.pool_cursor;
            if !r.search.schedules_transition(probe.from, probe.to) {
                continue;
            }
            #[cfg(feature = "cp-search")]
            if let Some(cp) = &mut r.cp {
                cp.note_probe(probe);
            }
            r.scheduler.proposed_pairs.insert((probe.from, probe.to));
            let proposals =
                r.search
                    .proposals(probe.from, probe.to, probe.after, probe.tier, cancel)?;
            if probe.tier == 1 {
                pending.extend(proposals);
            } else {
                exploration.extend(proposals);
            }
            probes += 1;
            if probes >= 32 && batch.is_empty() && !exploration.is_empty() {
                pending.push_back(exploration.pop_front().unwrap());
            }
        }
        if batch.is_empty() {
            if status == "generated_probe_scope_exhausted" {
                #[cfg(feature = "cp-search")]
                if r.cp.as_ref().is_some_and(cp_search::CpSearch::has_cue_work) {
                    status = "refinement_budget";
                    continue;
                }
                break;
            }
            continue;
        }
        if status == "generated_probe_scope_exhausted" {
            status = "refinement_budget";
        }
        for seed in &batch {
            let from = r
                .search
                .pool
                .iter()
                .position(|t| t.track_id == seed.placements[0].track_id)
                .unwrap();
            let to = r
                .search
                .pool
                .iter()
                .position(|t| t.track_id == seed.placements[1].track_id)
                .unwrap();
            r.scheduler.measured_pairs.insert((from, to));
        }
        for result in r.search.measure_batch(&batch, cancel)? {
            if r.search.continuations.is_some() {
                r.search.queue_continuations(
                    &result.sources,
                    result.continuations,
                    false,
                    false,
                )?;
            } else {
                exploration.extend(result.continuations);
            }
        }
        #[cfg(feature = "cp-search")]
        if let Some(page) = cue_page {
            r.cp.as_mut().unwrap().commit_cue_page(&r.search, page)?;
        }
        r.sync_edges()?;
        #[cfg(feature = "cp-search")]
        r.bootstrap_measured_incumbent(
            deadline.min(Instant::now() + std::time::Duration::from_secs(4)),
            cancel,
        )?;
        // A small fair mutation slice after every batch prevents both cached
        // permutations and high-ranked early edges from starving new bridges.
        r.repair(
            cancel,
            deadline.min(Instant::now() + std::time::Duration::from_millis(400)),
        )?;
    }
    // Reconsider the final measured variants even after the PCM quota closes.
    #[cfg(feature = "cp-search")]
    r.propose_cp_route(deadline, cancel)?;
    r.drain_pending(
        cancel,
        deadline,
        generation_limit.saturating_sub(r.scheduler.generation - generation_start),
    )?;
    #[cfg(feature = "cp-search")]
    while Instant::now() < deadline && r.close_cp_routes(deadline, cancel)? {}
    let stop = if !r.search.budget_open() {
        StopReason::WallClock
    } else if status == "generated_probe_scope_exhausted" {
        StopReason::GeneratedProbeScope
    } else {
        StopReason::MeasurementWindows
    };
    Ok((status, stop))
}

fn publish_result(
    mut r: Refiner<'_>,
    status: &str,
    stop: StopReason,
    budget_seconds: u64,
    measurement_limit: usize,
    preparation_seconds: f64,
    cancel: &Cancellation,
) -> Result<Value> {
    let finalization = Instant::now();
    if matches!(stop, StopReason::Cancelled) {
        r.search.deadline = Some(finalization);
    }
    let verification = cancel
        .check()
        .and_then(|()| r.search.snapshot.verify_read_snapshot(cancel));
    let (status, stop) = match verification {
        Err(error) if error.code == "CANCELLED" => {
            // Revalidate before publishing any retained result. No further
            // search work is scheduled after this boundary.
            r.search.deadline = Some(finalization);
            r.search
                .snapshot
                .verify_read_snapshot(&Cancellation::default())?;
            ("cancelled", StopReason::Cancelled)
        }
        result => {
            result?;
            (status, stop)
        }
    };
    let store = r.search.store;
    r.scheduler.frontier_saved = None;
    r.draft_report.header.extensions.insert(
        "budget".into(),
        json!({
            "configured_seconds": budget_seconds,
            "pool_seconds": r.search.stats.pool_seconds,
            "preparation_seconds": preparation_seconds,
            "search_stopped_seconds": finalization.duration_since(r.search.started).as_secs_f64(),
            "measurement_limit_reached": r.search.stats.measured_windows >= measurement_limit,
        }),
    );
    r.checkpoint_with_stop(status, Some(stop))?;
    let path = store.path(&format!("reports/{}.json", r.draft_report.report_id))?;
    let draft = r.draft_report.header.extensions["draft"].clone();
    let mut search = r.draft_report.header.extensions["search"].clone();
    search["budget"] = r.draft_report.header.extensions["budget"].clone();
    if let Some(plan) = &mut r.search.incumbent {
        plan.header
            .extensions
            .insert("set_refinement".into(), search.clone());
        store.write(&format!("plans/{}.json", plan.plan_id), plan)?;
    }
    search["elapsed_seconds"] = json!(r.search.started.elapsed().as_secs_f64());
    search["finalization_seconds"] = json!(finalization.elapsed().as_secs_f64());
    let result_status = if r.search.incumbent.is_some() {
        "ready"
    } else if search["termination"] == "infeasible_within_domain" {
        "infeasible"
    } else if draft.is_null() {
        "pending"
    } else {
        "draft"
    };
    Ok(json!({"status":result_status,"plan":r.search.incumbent,
        "draft":draft,"draft_path":path,"search":search,
        "export_gate":"An approved plan is returned only after every nil join is replaced and musical bookends, duration, pacing and required pairs pass."}))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    #[test]
    fn a_window_execution_failure_does_not_discard_other_worker_results() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[360; 3]);
        r.search.continuations = Some(continuation::Frontier::default());
        let failed = canonical_hash(&"failed window").unwrap().digest;
        let reviewed = canonical_hash(&"reviewed window").unwrap().digest;
        let cancelled = canonical_hash(&"cancelled window").unwrap().digest;
        let unknown = canonical_hash(&"unknown window").unwrap().digest;
        let outcomes = vec![
            (
                0,
                failed.clone(),
                WindowMeasurement::Original,
                Err(Error::new("IO", "fixture read failure")),
            ),
            (
                1,
                reviewed.clone(),
                WindowMeasurement::Original,
                Ok(Measurement {
                    evidence: vec![json!({"ranked": [], "rejected": []})],
                    ..Measurement::default()
                }),
            ),
            (
                2,
                cancelled.clone(),
                WindowMeasurement::Original,
                Err(Error::new("CANCELLED", "fixture cancellation")),
            ),
            (
                3,
                unknown.clone(),
                WindowMeasurement::Original,
                Ok(Measurement {
                    rejected: BTreeMap::from([("TACTIC_FACTS".into(), 1)]),
                    ..Measurement::default()
                }),
            ),
        ];
        let mut batch = vec![Measurement::default(); outcomes.len()];
        assert_eq!(
            r.search
                .commit_batch_results(outcomes, &mut batch)
                .unwrap_err()
                .code,
            "CANCELLED"
        );
        assert_eq!(
            r.search.measured_keys,
            BTreeSet::from([reviewed.clone(), unknown.clone()])
        );
        assert_eq!(
            r.search.negative_windows,
            BTreeSet::from([reviewed.clone()])
        );
        assert_eq!(r.search.oracle.evidence()["execution_failures"]["IO"], 1);
        r.search.measured_keys.clear();
        r.search.negative_windows.clear();
        r.search.oracle = oracle::Ledger::default();
        r.search.load_known(&Cancellation::default()).unwrap();
        assert!(r.search.measured_keys.contains(&reviewed));
        assert!(r.search.oracle.is_unknown(&unknown));
        assert!(!r.search.measured_keys.contains(&failed));
        assert!(!r.search.measured_keys.contains(&cancelled));
        assert!(!r.search.negative_windows.contains(&unknown));
    }

    #[test]
    fn selector_refresh_retains_old_admission_and_retries_only_execution_errors() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[360; 3]);
        let search = &mut r.search;
        search.continuations = Some(continuation::Frontier::default());
        let mut plan = super::super::tests::tactic_plan();
        for (track, placement) in search.pool.iter_mut().zip(&mut plan.placements) {
            track.active_analysis = Some(placement.analysis.clone());
            placement.track_id = track.track_id.clone();
            placement.audio_hash = track.audio_hash.clone();
        }
        plan.transitions[0].from_track_id = plan.placements[0].track_id.clone();
        plan.transitions[0].to_track_id = plan.placements[1].track_id.clone();
        search.cache = Cache::open(&policy, &search.pool).unwrap();
        let key = window_key(&search.cache, &plan).unwrap();
        let sources: BTreeMap<_, _> = plan
            .placements
            .iter()
            .map(|p| {
                (
                    p.track_id.clone(),
                    search.cache.sources[&p.track_id].clone(),
                )
            })
            .collect();
        let old = Measurement {
            sources: sources.clone(),
            edges: vec![MeasuredEdge {
                plan,
                exit: 0,
                entry_end: 0,
                score: 5.,
                last_resort: false,
                dense_quality_seconds: 0.,
                handoff_quality_seconds: 0.,
            }],
            evidence: vec![json!({"ranked": [{}], "rejected": []})],
            ..Default::default()
        };
        search.cache.write(&store, "windows", &key, &old).unwrap();
        let cancel = Cancellation::default();
        search.load_known(&cancel).unwrap();
        assert!(search.oracle.needs_selector_refresh(&key));
        assert_eq!(search.stats.known_good_choices, 1);

        let mut results = [Measurement::default()];
        let kind = WindowMeasurement::SelectorRefresh;
        search
            .commit_batch_results(
                vec![(0, key.clone(), kind, Err(Error::new("IO", "read failed")))],
                &mut results,
            )
            .unwrap();
        assert!(search.oracle.needs_selector_refresh(&key));
        assert!(search
            .cache
            .keys(&store, kind.cache_kind())
            .unwrap()
            .is_empty());
        assert_eq!(search.oracle.evidence()["execution_failures"]["IO"], 1);

        let missing = Measurement {
            sources,
            rejected: BTreeMap::from([("TACTIC_FACTS".into(), 1)]),
            ..Default::default()
        };
        search
            .commit_batch_results(vec![(0, key.clone(), kind, Ok(missing))], &mut results)
            .unwrap();
        assert_eq!(search.stats.selector_refresh_attempts, 2);
        assert_eq!(search.stats.measured_windows, 2);
        assert!(search.oracle.is_unknown(&key));
        assert!(!search.oracle.needs_selector_refresh(&key));
        assert_eq!(search.oracle.evidence()["windows_with_verified_choices"], 1);
        assert_eq!(search.oracle.evidence()["execution_failures"], json!({}));

        search.oracle = oracle::Ledger::default();
        search.measured_keys.clear();
        search.load_known(&cancel).unwrap();
        assert!(search.oracle.is_unknown(&key));
        assert!(!search.oracle.needs_selector_refresh(&key));
        assert_eq!(search.stats.known_good_choices, 1);
        assert_eq!(
            search.oracle.evidence()["selector_realizations"]["refreshed_without_partition"],
            1
        );
        search.observe_window(&key, &old, WindowMeasurement::Original);
        assert!(search.oracle.is_unknown(&key));
    }

    #[test]
    fn measured_parent_restores_unmeasured_children_with_current_pins() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[360; 3]);
        let search = &mut r.search;
        search.continuations = Some(continuation::Frontier::default());
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
        for (track, placement) in search.pool.iter_mut().zip(&mut plan.placements) {
            track.active_analysis = Some(placement.analysis.clone());
            placement.track_id = track.track_id.clone();
            placement.audio_hash = track.audio_hash.clone();
        }
        plan.transitions[0].from_track_id = plan.placements[0].track_id.clone();
        plan.transitions[0].to_track_id = plan.placements[1].track_id.clone();
        crate::search_cache::rebind(&mut plan, &policy).unwrap();
        search.cache = Cache::open(&policy, &search.pool).unwrap();
        let variant = |seconds| {
            let mut next = plan.clone();
            next.plan_id = id();
            next.header.extensions.insert(
                "tactic_cue_candidate".into(),
                json!({"fixture_endpoint": seconds}),
            );
            next
        };
        let child = variant(12);
        let other = variant(16);
        let resolved = variant(8);
        let mut stale = variant(20);
        stale.placements[0].override_revision += 1;
        let child_key = window_key(&search.cache, &child).unwrap();
        let resolved_key = window_key(&search.cache, &resolved).unwrap();
        for (name, children) in [
            (
                "first parent",
                vec![child.clone(), other.clone(), resolved, stale],
            ),
            ("duplicate parent", vec![variant(12)]),
        ] {
            let parent_key = canonical_hash(&name).unwrap().digest;
            search
                .cache
                .write(
                    &store,
                    "windows",
                    &parent_key,
                    &Measurement {
                        sources: search.cache.sources.clone(),
                        continuations: children,
                        ..Measurement::default()
                    },
                )
                .unwrap();
        }
        search
            .cache
            .write(&store, "windows", &resolved_key, &Measurement::default())
            .unwrap();
        let cancel = Cancellation::default();
        search.load_known(&cancel).unwrap();
        let evidence = search
            .continuations
            .as_ref()
            .unwrap()
            .evidence(&search.measured_keys);
        assert_eq!(evidence["restored"], 2);
        assert_eq!(evidence["stale_or_invalid"], 1);
        assert_eq!(search.stats.measured_windows, 0);

        let mut ranked = VecDeque::from([variant(10)]);
        let scheduled = BTreeSet::new();
        let first = search.take_quality_seed(&mut ranked, &scheduled).unwrap();
        assert_ne!(window_key(&search.cache, &first).unwrap(), resolved_key);
        assert_eq!(
            search
                .take_quality_seed(&mut ranked, &scheduled)
                .unwrap()
                .header
                .extensions["tactic_cue_candidate"]["fixture_endpoint"],
            10
        );
        let second = search.take_quality_seed(&mut ranked, &scheduled).unwrap();
        let recovered: BTreeSet<_> = [first, second]
            .iter()
            .map(|p| window_key(&search.cache, p).unwrap())
            .collect();
        assert_eq!(
            recovered,
            BTreeSet::from([
                child_key.clone(),
                window_key(&search.cache, &other).unwrap()
            ])
        );

        // Only one of the two jobs was committed before shutdown.
        search
            .cache
            .write(&store, "windows", &child_key, &Measurement::default())
            .unwrap();
        search.continuations = Some(continuation::Frontier::default());
        search.load_known(&cancel).unwrap();
        let pending = search.take_quality_seed(&mut ranked, &scheduled).unwrap();
        assert_eq!(
            window_key(&search.cache, &pending).unwrap(),
            window_key(&search.cache, &other).unwrap()
        );
        assert!(search.take_quality_seed(&mut ranked, &scheduled).is_none());
    }

    pub(super) fn fixture<'a>(
        store: &'a Store,
        policy: &'a MixingConfig,
        durations: &[u64],
    ) -> Refiner<'a> {
        let root = store.path("fixture-sources").unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let root_id = id();
        let mut library: Library = store.read("library.json").unwrap();
        library.source_roots.push(SourceRoot {
            root_id: root_id.clone(),
            display_name: "Fixture sources".into(),
            location_kind: "local".into(),
            path: root.to_string_lossy().into_owned(),
            available: true,
            require_finalized_sidecar: false,
        });
        store.write("library.json", &library).unwrap();
        let pool: Vec<_> = durations
            .iter()
            .enumerate()
            .map(|(i, &duration)| {
                let filename = format!("song-{i}.wav");
                // Deliberately not playable PCM: planning an all-nil baseline
                // must never invoke an audio reader or claim source validation.
                let bytes = format!("metadata-only source {i}");
                std::fs::write(root.join(&filename), &bytes).unwrap();
                let audio_hash = hash_bytes(bytes.as_bytes());
                Track {
                    header: Header::default(),
                    track_id: id(),
                    audio_revision: 1,
                    audio_hash: audio_hash.clone(),
                    format: AudioFormat {
                        sample_rate_hz: 48000,
                        channels: 2,
                        encoding: Encoding::IeeeFloat,
                        container_bits: 32,
                        valid_bits: 32,
                        channel_mask: 3,
                        frames: duration * 48000,
                    },
                    source_locations: vec![SourceLocation {
                        root_id: root_id.clone(),
                        relative_path: filename,
                        fingerprint: Fingerprint {
                            size: bytes.len() as u64,
                            modified_ns: "0".into(),
                            file_identity: format!("fixture-{i}"),
                        },
                        file_hash: audio_hash,
                        available: true,
                    }],
                    active_analysis: None,
                    active_overrides: None,
                    override_revision: 0,
                    lifecycle: Lifecycle::Available,
                }
            })
            .collect();
        let request = Request::new(60, "story").unwrap();
        let search = Search {
            store,
            snapshot: store.read_snapshot(),
            policy,
            context: 12,
            request,
            composer: Context::fixture(&pool, request),
            required: required_pairs::RequiredPairs::default(),
            excluded_transitions: BTreeSet::new(),
            #[cfg(feature = "cp-search")]
            capacity_proof: None,
            pacing: pool.iter().map(|_| TrackPacing::unknown(48000)).collect(),
            cache: Cache::open(policy, &pool).unwrap(),
            facts: CandidateEvidenceCache::new(),
            event_cues: EventCueIndex::new(12),
            baseline_cues: BaselineCueIndex::new(12),
            worker_facts: vec![],
            measured_keys: BTreeSet::new(),
            oracle: oracle::Ledger::default(),
            continuations: None,
            stats: Stats::default(),
            started: Instant::now(),
            deadline: None,
            progress: Report::new("test_search"),
            history: vec![],
            incumbent: None,
            graph: vec![vec![]; pool.len() * pool.len()],
            known: BTreeSet::new(),
            negative_windows: BTreeSet::new(),
            pool,
        };
        Refiner {
            search,
            edges: vec![],
            edge_keys: BTreeSet::new(),
            blocks: BTreeMap::new(),
            population: vec![],
            best: None,
            seen: BTreeSet::new(),
            attempted_repairs: BTreeSet::new(),
            attempted_final: BTreeSet::new(),
            draft_report: Report::new("test_refinement"),
            history: vec![],
            rounds: 0,
            rejected_layouts: 0,
            scheduler: Scheduler::default(),
            #[cfg(feature = "cp-search")]
            cp: None,
        }
    }

    pub(super) fn edge(r: &mut Refiner<'_>, a: usize, b: usize, score: f64, dense: f64) -> usize {
        let mut plan: MixPlan =
            serde_json::from_str(include_str!("../../../examples/mix-plan.json")).unwrap();
        plan.placements.truncate(2);
        plan.placements[0].track_id = r.search.pool[a].track_id.clone();
        plan.placements[1].track_id = r.search.pool[b].track_id.clone();
        let index = r.edges.len();
        r.edges.push(Arc::new(MeasuredEdge {
            plan,
            exit: 0,
            entry_end: 0,
            score,
            last_resort: false,
            dense_quality_seconds: dense,
            handoff_quality_seconds: 0.,
        }));
        index
    }

    #[test]
    fn exact_measured_choices_preserve_pacing_and_score_variants() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[1200; 3]);
        let index = edge(&mut r, 0, 1, 35., 0.);
        let original = (*r.edges.remove(index)).clone();
        let mut duplicate = original.clone();
        duplicate.plan.plan_id = id();
        duplicate.plan.placements[0].time_map.map_id = id();
        duplicate
            .plan
            .header
            .extensions
            .insert("set_config".into(), json!({"name": "another recipe"}));
        let mut scored = original.clone();
        scored.score += 1.;
        let mut paced = original.clone();
        paced.entry_end += 1;
        let mut departed = original.clone();
        departed.exit += 1;
        for choice in [&original, &duplicate, &scored, &paced, &departed] {
            r.search.register(choice).unwrap();
        }
        assert_eq!(r.search.stats.duplicate_measured_choices, 1);
        assert_eq!(r.search.stats.known_good_choices, 4);
        assert!(r.sync_edges().unwrap());
        assert!(!r.sync_edges().unwrap());
        let keys = (0..r.edges.len())
            .map(|i| r.persistent_key(i).unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(keys.len(), 4);
        #[cfg(feature = "cp-search")]
        {
            let mut cp = cp_search::CpSearch::default();
            for i in [0, 0, 1, 2, 3] {
                cp.closing
                    .queue(&[i], r.closing_route_keys(&[i]).unwrap())
                    .unwrap();
            }
            assert_eq!(cp.closing.owned_routes().count(), 4);
            assert_eq!(cp.closing.evidence()["pending_jobs"], 4);
            assert_eq!(cp.closing.evidence()["duplicate_route_requests"], 1);
        }
    }

    #[test]
    fn measured_credit_cannot_exceed_its_overlap_clock() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[1200; 3]);
        let edge_id = edge(&mut r, 0, 1, 35., 0.);
        let mut record = (*r.edges[edge_id]).clone();
        let join = &record.plan.transitions[0];
        let span = (join.end_output_frame - join.start_output_frame) as f64
            / f64::from(record.plan.output.sample_rate_hz);
        record.dense_quality_seconds = span + 1.;
        record.plan.header.extensions.insert(
            "dense_quality_seconds".into(),
            json!(record.dense_quality_seconds),
        );
        assert_eq!(r.search.register(&record).unwrap_err().code, "LAZY_BOUND");
        assert_eq!(r.search.stats.known_good_choices, 0);

        record.dense_quality_seconds = span;
        record
            .plan
            .header
            .extensions
            .insert("dense_quality_seconds".into(), json!(span));
        r.search.register(&record).unwrap();
        assert_eq!(r.search.stats.known_good_choices, 1);
    }

    #[cfg(feature = "cp-search")]
    #[test]
    fn cp_route_keeps_unmeasured_bridges_and_requests_audio_for_them() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[1800; 3]);
        let hold = edge(&mut r, 0, 1, 60., 40.);
        r.blocks.insert(
            vec![hold],
            Some(Arc::new(Block {
                tracks: vec![0, 1],
                edges: vec![hold],
                plan: None,
                minimum: 180 * 48000,
                maximum: 3500 * 48000,
            })),
        );
        let mut cp = cp_search::CpSearch::default();
        cp.restore_cue_frontiers(&mut r.search, &Cancellation::default())
            .unwrap();
        let draft = cp
            .step(
                &mut r,
                Instant::now() + std::time::Duration::from_secs(3),
                &Cancellation::default(),
            )
            .unwrap()
            .unwrap();
        r.admit(draft, &Cancellation::default()).unwrap();
        let draft = r.best.as_ref().unwrap();
        assert_eq!(draft.edges(), vec![hold]);
        assert_eq!(draft.nil(), 1);
        assert!(cp.demands().iter().any(|p| p.tier == 0));
        assert!(cp.demands().iter().all(|p| (p.from, p.to) != (0, 1)));
        cp.save_cue_frontiers(&r.search).unwrap();
        let mut resumed = cp_search::CpSearch::default();
        resumed
            .restore_cue_frontiers(&mut r.search, &Cancellation::default())
            .unwrap();
        assert_eq!(resumed.evidence()["bar_cue_domain"]["queued_bar_scans"], 1);
        assert!(r.search.incumbent.is_none());
    }

    #[test]
    fn empty_graph_immediately_produces_and_persists_a_zero_point_full_draft() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[360; 12]);
        let draft = r.candidate(&[], 0).unwrap().unwrap();
        assert_eq!(draft.score, 0.);
        assert_eq!(draft.nil(), 9);
        assert_eq!(draft.lengths.iter().sum::<u64>(), 3510 * 48000);
        assert!(draft.edges().is_empty());
        r.admit(draft, &Cancellation::default()).unwrap();
        assert!(r.search.incumbent.is_none());
        assert_eq!(r.search.stats.measured_windows, 0);
        assert_eq!(r.search.stats.cached_windows, 0);
        let saved: Report = store
            .read(&format!("reports/{}.json", r.draft_report.report_id))
            .unwrap();
        assert_eq!(saved.header.extensions["draft"]["score"], json!(0.));
        assert_eq!(
            saved.header.extensions["draft"]["duration_seconds"],
            json!(3510.)
        );
        assert_eq!(saved.header.extensions["draft"]["exportable"], json!(false));
        assert!(saved.header.extensions["draft"]["joins"]
            .as_array()
            .unwrap()
            .iter()
            .all(|j| j["choice"] == "nil" && j["score"] == 0.));
        assert!(store.records::<MixPlan>("plans", None).unwrap().is_empty());
    }

    #[test]
    fn zero_nil_count_alone_does_not_authorize_a_plan() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[3600, 360, 360]);
        let draft = r.candidate(&[], 0).unwrap().unwrap();
        assert_eq!(draft.nil(), 0);
        assert_eq!(draft.lengths.iter().sum::<u64>(), 3510 * 48000);
        r.admit(draft, &Cancellation::default()).unwrap();
        assert!(
            r.search.incumbent.is_none(),
            "musical bookends still need approval"
        );
        assert!(store.records::<MixPlan>("plans", None).unwrap().is_empty());
    }

    #[test]
    fn cyclic_or_overlapping_repairs_leave_the_complete_incumbent_intact() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[360; 12]);
        let initial = r.candidate(&[], 0).unwrap().unwrap();
        let key = initial.key.clone();
        r.admit(initial, &Cancellation::default()).unwrap();
        let a = edge(&mut r, 0, 1, 40., 60.);
        let b = edge(&mut r, 1, 0, 40., 60.);
        let c = edge(&mut r, 2, 1, 40., 60.);
        assert!(r.candidate(&[a, b], 0).unwrap().is_none());
        assert!(r.candidate(&[a, c], 0).unwrap().is_none());
        assert_eq!(r.best.as_ref().unwrap().key, key);
        assert_eq!(
            r.best.as_ref().unwrap().lengths.iter().sum::<u64>(),
            3510 * 48000
        );
        assert_eq!(r.history.len(), 1);
        assert!(r.search.incumbent.is_none());
    }

    #[test]
    fn merging_resolved_blocks_counts_each_measured_join_once() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[360; 12]);
        let a = edge(&mut r, 0, 1, 50., 60.);
        let b = edge(&mut r, 1, 2, 25., 30.);
        // Stub the already validated block compiler, keeping the production
        // candidate construction, full-duration refit and score calculation.
        for (edges, tracks) in [(vec![a], vec![0, 1]), (vec![a, b], vec![0, 1, 2])] {
            let count = tracks.len() as u64;
            r.blocks.insert(
                edges.clone(),
                Some(Arc::new(Block {
                    tracks,
                    edges,
                    plan: None,
                    minimum: count * 90 * 48000,
                    maximum: count * 300 * 48000,
                })),
            );
        }
        let first = r.candidate(&[a], 0).unwrap().unwrap();
        let merged = r.candidate(&[a, b], 0).unwrap().unwrap();
        assert!((first.score - 62. / 58.5).abs() < 1e-12);
        assert!((merged.score - 93. / 58.5).abs() < 1e-12);
        assert_eq!(merged.edges(), vec![a, b]);
        assert_eq!(merged.lengths.iter().sum::<u64>(), 3510 * 48000);
        assert!(
            r.candidate(&[a, a], 0).unwrap().is_none(),
            "repeated edge cannot earn points twice"
        );
    }

    #[test]
    fn certified_branch_bound_prunes_only_that_branch_and_preserves_the_full_draft() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[360; 12]);
        let draft = r.candidate(&[], 0).unwrap().unwrap();
        let key = draft.key.clone();
        let branch = Branch::new(draft.tracks(), r.assignments(&draft)).unwrap();
        let bound = branch
            .upper_bound_with_fit(|_| unreachable!(), |_| unreachable!(), 3420., 3600.)
            .unwrap();
        r.admit(draft, &Cancellation::default()).unwrap();
        // Stand in for the already approved incumbent. This test targets the
        // scheduler's comparison domain; plan admission has separate coverage.
        let mut approved: MixPlan =
            serde_json::from_str(include_str!("../../../examples/mix-plan.json")).unwrap();
        approved
            .header
            .extensions
            .insert("set_composition".into(), json!({"objective":bound}));
        r.search.incumbent = Some(approved);
        r.scheduler.branches.push_back(branch);
        r.branch_step(
            &Cancellation::default(),
            Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(r.scheduler.bound_pruned, 1);
        assert_eq!(r.scheduler.branches_expanded, 0);
        assert!(r.scheduler.branches.is_empty());
        assert_eq!(r.best.as_ref().unwrap().key, key);
        assert_eq!(r.population.len(), 1);
        assert_eq!(
            r.best.as_ref().unwrap().lengths.iter().sum::<u64>(),
            3510 * 48000
        );
    }

    #[test]
    fn locking_a_nil_is_infeasible_for_export_but_does_not_delete_its_draft() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[360; 12]);
        let draft = r.candidate(&[], 0).unwrap().unwrap();
        let key = draft.key.clone();
        let branch = Branch::new(draft.tracks(), r.assignments(&draft)).unwrap();
        r.admit(draft, &Cancellation::default()).unwrap();
        r.scheduler.branches.push_back(branch);
        r.branch_step(
            &Cancellation::default(),
            Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(r.scheduler.impossible_branches, 1);
        assert_eq!(r.scheduler.bound_pruned, 0);
        assert!(r.scheduler.branches.is_empty());
        assert_eq!(r.best.as_ref().unwrap().key, key);
        assert_eq!(r.population.len(), 1);
        assert!(r.search.incumbent.is_none());
        let saved: Report = store
            .read(&format!("reports/{}.json", r.draft_report.report_id))
            .unwrap();
        assert_eq!(
            saved.header.extensions["draft"]["duration_seconds"],
            json!(3510.)
        );
    }

    #[test]
    fn fixed_order_validates_required_membership_without_reordering_heuristics() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[360; 12]);
        let ids = ["0000000000000000000010", "0000000000000000000011"];
        for (i, id) in [10, 11].into_iter().zip(ids) {
            let source = store.source(&r.search.pool[i]).unwrap();
            let sidecar = PathBuf::from(format!("{}.json", source.display()));
            std::fs::write(sidecar, serde_json::to_vec(&json!({"trackID":id})).unwrap()).unwrap();
        }
        let mut configured = policy.clone();
        configured
            .header
            .extensions
            .insert(required_pairs::KEY.into(), json!([ids]));
        configured
            .header
            .extensions
            .insert("required_pair_mode".into(), json!("anywhere"));
        r.search.required = required_pairs::RequiredPairs::resolve(
            &r.search.snapshot,
            &configured,
            &r.search.pool,
            r.search.pool.len(),
        )
        .unwrap();
        let order: Vec<_> = (0..12).collect();
        let singleton_blocks = order.iter().map(|&i| vec![i]).collect::<Vec<_>>();
        assert_ne!(
            refinement_layout::order(&singleton_blocks, &r.search.required, 12),
            Some(order.clone())
        );
        let draft = r.fixed_candidate(&order, &[None; 11]).unwrap().unwrap();
        assert_eq!(draft.tracks(), order);
        assert_eq!(draft.lengths.iter().sum::<u64>(), 3510 * 48000);
        assert!(r
            .fixed_candidate(&(0..10).collect::<Vec<_>>(), &[None; 9])
            .unwrap()
            .is_none());
        assert!(r
            .fixed_candidate(&order, &[Some(999); 11])
            .unwrap()
            .is_none());
    }

    #[test]
    fn parking_and_resume_preserve_complete_layouts_and_reject_stale_sources() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[360; 12]);
        for salt in 0..40 {
            let draft = r.candidate(&[], salt).unwrap().unwrap();
            r.admit(draft, &Cancellation::default()).unwrap();
        }
        r.prune();
        assert!(r.population.len() <= 32);
        assert!(!r.scheduler.parked.is_empty());
        let before = r
            .population
            .iter()
            .chain(&r.scheduler.parked)
            .map(|d| d.key.clone())
            .collect::<BTreeSet<_>>();
        r.scheduler.generation = 19;
        r.save_frontier().unwrap();

        // Drop all volatile scheduling state as a fresh process would.
        r.population.clear();
        r.scheduler = Scheduler::default();
        r.best = None;
        r.seen.clear();
        r.history.clear();
        r.search.cache = Cache::open(&policy, &r.search.pool).unwrap();
        r.search.snapshot = store.read_snapshot();
        r.restore_frontier(&Cancellation::default()).unwrap();
        let after = r
            .population
            .iter()
            .chain(&r.scheduler.parked)
            .map(|d| d.key.clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(before, after);
        assert_eq!(r.scheduler.generation, 19);
        assert!(r.scheduler.cache_loaded);
        assert!(r.population.iter().chain(&r.scheduler.parked).all(|d| d
            .lengths
            .iter()
            .sum::<u64>()
            == 3510 * 48000));

        let retained = r.best.as_ref().unwrap().key.clone();
        let id = r.search.pool[0].track_id.clone();
        r.search
            .cache
            .sources
            .insert(id, "changed-source-signature".into());
        let invalid = r.search.cache.invalid;
        r.restore_frontier(&Cancellation::default()).unwrap();
        assert_eq!(r.search.cache.invalid, invalid + 1);
        assert_eq!(r.best.as_ref().unwrap().key, retained);
    }

    #[test]
    fn measured_frontier_best_survives_budget_and_recipe_rebinding() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut changed_policy = policy.clone();
        changed_policy
            .header
            .extensions
            .insert("lazy_search_seconds".into(), json!(1));
        changed_policy
            .header
            .extensions
            .insert("lazy_measurement_budget".into(), json!(77));
        changed_policy.header.extensions.insert(
            "set_config".into(),
            json!({"name":"Warm resume","path":"Sets/resume.json"}),
        );
        let mut r = fixture(&store, &policy, &[360; 12]);
        let baseline = r.candidate(&[], 0).unwrap().unwrap();
        r.admit(baseline, &Cancellation::default()).unwrap();

        let mut plan = super::super::tests::tactic_plan();
        for (i, placement) in plan.placements.iter_mut().enumerate() {
            placement.track_id = r.search.pool[i].track_id.clone();
        }
        plan.transitions[0].from_track_id = r.search.pool[0].track_id.clone();
        plan.transitions[0].to_track_id = r.search.pool[1].track_id.clone();
        crate::search_cache::rebind(&mut plan, &policy).unwrap();
        r.edges.push(Arc::new(MeasuredEdge {
            plan,
            exit: 0,
            entry_end: 0,
            score: 5.,
            last_resort: false,
            dense_quality_seconds: 0.,
            handoff_quality_seconds: 0.,
        }));
        // The measured edge has an executable, nonempty tactic schedule. Stub
        // only block compilation so this persistence test needs no source PCM.
        let block = Arc::new(Block {
            tracks: vec![0, 1],
            edges: vec![0],
            plan: None,
            minimum: 180 * 48000,
            maximum: 700 * 48000,
        });
        r.blocks.insert(vec![0], Some(block.clone()));
        let resolved = r.candidate(&[0], 0).unwrap().unwrap();
        r.admit(resolved, &Cancellation::default()).unwrap();
        let before = r.best.clone().unwrap();
        assert_eq!(before.edges(), vec![0]);
        assert!(before.score > 0.);
        let old_edge_key = measured_edge_key(&r.edges[0]).unwrap();
        r.save_frontier().unwrap();

        let mut rebound = (*r.edges[0]).clone();
        crate::search_cache::rebind(&mut rebound.plan, &changed_policy).unwrap();
        rebound.plan.plan_id = id();
        rebound.plan.transitions[0].transition_id = id();
        for placement in &mut rebound.plan.placements {
            placement.time_map.map_id = id();
        }
        assert_eq!(measured_edge_key(&rebound).unwrap(), old_edge_key);
        r.edges[0] = Arc::new(rebound);
        r.population.clear();
        r.scheduler = Scheduler::default();
        r.best = None;
        r.seen.clear();
        r.history.clear();
        r.blocks.clear();
        r.blocks.insert(vec![0], Some(block));
        r.search.policy = &changed_policy;
        r.search.cache = Cache::open(&changed_policy, &r.search.pool).unwrap();
        r.search.snapshot = store.read_snapshot();
        r.restore_frontier(&Cancellation::default()).unwrap();
        let after = r.best.as_ref().unwrap();
        assert_eq!(after.tracks(), before.tracks());
        assert_eq!(
            after.edges(),
            vec![0],
            "restoring only nil candidates would lose the measured best"
        );
        assert_eq!(after.nil(), before.nil());
        assert_eq!(after.score, before.score);
        assert_eq!(after.lengths, before.lengths);
        assert!(r
            .scheduler
            .branches
            .iter()
            .any(|branch| branch.choices.contains(&Some(0))));
    }

    #[test]
    fn closing_capacity_keeps_replay_eligible_components_below_the_hour_floor() {
        let rate = 48_000.;
        // A fifty-minute component can reach the 57-minute floor with legal
        // phrase replays. Requiring 57 native minutes would discard it first.
        assert!(closing_capacity(50 * 60 * 48_000, rate, 57. * 60.));
        assert!(!closing_capacity(49 * 60 * 48_000, rate, 57. * 60.));
        assert!(closing_capacity(57 * 60 * 48_000, rate, 57. * 60.));
        // The gate follows the requested set clock and actual output rate.
        assert!(!closing_capacity(50 * 60 * 48_000, rate, 117. * 60.));
        assert!(closing_capacity(102 * 60 * 44_100, 44_100., 117. * 60.));
    }

    #[test]
    fn final_assembly_uses_a_last_measured_bridge_without_more_pcm_work() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[360; 12]);
        let initial = r.candidate(&[], 0).unwrap().unwrap();
        let initial_nil = initial.nil();
        r.admit(initial, &Cancellation::default()).unwrap();
        let measured = edge(&mut r, 0, 1, 25., 0.);
        // The bridge has already been measured; block compilation is stubbed
        // so an accidental PCM read of these metadata-only sources would fail.
        r.blocks.insert(
            vec![measured],
            Some(Arc::new(Block {
                tracks: vec![0, 1],
                edges: vec![measured],
                plan: None,
                minimum: 180 * 48_000,
                maximum: 700 * 48_000,
            })),
        );
        r.drain_pending(
            &Cancellation::default(),
            Instant::now() + std::time::Duration::from_secs(5),
            0,
        )
        .unwrap();
        let best = r.best.as_ref().unwrap();
        assert!(best.nil() < initial_nil);
        assert_eq!(best.edges(), vec![measured]);
        assert_eq!(r.search.stats.measured_windows, 0);
        assert_eq!(r.search.stats.cached_windows, 0);
        assert!(r.scheduler.branches.is_empty());
        assert!(!r.attempted_repairs.is_empty());
        assert_eq!(r.scheduler.generation, 0);
    }

    #[test]
    fn final_assembly_preserves_pending_work_when_the_deadline_has_passed() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[360; 12]);
        let initial = r.candidate(&[], 0).unwrap().unwrap();
        r.admit(initial, &Cancellation::default()).unwrap();
        edge(&mut r, 0, 1, 25., 0.);
        let branch = Branch::new(vec![0, 1], vec![Some(0)]).unwrap();
        r.scheduler.branches.push_back(branch.clone());
        r.drain_pending(&Cancellation::default(), Instant::now(), 8)
            .unwrap();
        assert_eq!(r.scheduler.branches, VecDeque::from([branch]));
        assert!(r.attempted_repairs.is_empty());
        assert_eq!(r.scheduler.generation, 0);
    }

    #[test]
    fn elapsed_budget_skips_new_measurement_and_keeps_the_previous_checkpoint() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[360; 12]);
        let initial = r.candidate(&[], 0).unwrap().unwrap();
        let cancel = Cancellation::default();
        r.admit(initial, &cancel).unwrap();
        r.save_frontier().unwrap();
        r.scheduler.generation = 7;
        r.search.deadline = Some(Instant::now());
        let seed = super::super::tests::tactic_plan();
        let windows = r.search.measure_batch(&[seed], &cancel).unwrap();
        assert!(windows[0].edges.is_empty());
        assert!(r.search.worker_facts.is_empty());
        assert_eq!(r.search.stats.measured_windows, 0);

        r.save_frontier().unwrap();
        let key = Cache::set_key(&policy).unwrap();
        let saved = r
            .search
            .cache
            .read::<Value>(&store, "refinement-frontiers", &key)
            .unwrap()
            .unwrap();
        assert_eq!(saved["generation"], 0);
        assert_eq!(r.scheduler.generation, 7);
        r.checkpoint_with_stop("refinement_budget", Some(StopReason::WallClock))
            .unwrap();
        let search = &r.draft_report.header.extensions["search"];
        assert_eq!(search["stop_reason"], "wall_clock");
        assert_eq!(search["checkpoint_deadline_reached"], true);
    }

    #[cfg(feature = "cp-search")]
    #[test]
    fn cp_queues_a_draft_route_without_running_legacy_closing() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[1200; 3]);
        r.cp = Some(cp_search::CpSearch::default());
        let join = edge(&mut r, 0, 1, 25., 0.);
        let plan = r.edges[join].plan.clone();
        let cancel = Cancellation::default();
        r.try_close(&[join], &plan, "fixture", &cancel).unwrap();
        r.try_close(&[join], &plan, "fixture", &cancel).unwrap();
        assert_eq!(r.scheduler.closing_attempts, 0);
        assert!(r.attempted_final.is_empty());
        assert_eq!(r.cp.unwrap().closing.evidence()["pending_jobs"], 1);
    }
    #[test]
    fn stable_frontier_chunks_survive_reordering_and_localize_membership_changes() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let cache = Cache::open(&MixingConfig::default(), &[]).unwrap();
        let mut rows = (0..24).map(|i| json!({"candidate":i})).collect::<Vec<_>>();
        let before = frontier_parts(&cache, &store, rows.clone(), None)
            .unwrap()
            .unwrap();
        rows.reverse();
        assert_eq!(
            frontier_parts(&cache, &store, rows.clone(), None)
                .unwrap()
                .unwrap(),
            before
        );
        rows.remove(0);
        rows.insert(0, json!({"candidate":"new best"}));
        let after = frontier_parts(&cache, &store, rows, None).unwrap().unwrap();
        let reused = before.iter().filter(|key| after.contains(key)).count();
        assert!(
            reused >= before.len().saturating_sub(2),
            "only changed hash buckets need new chunks"
        );
    }

    #[test]
    fn throttled_frontier_still_restores_a_newer_best_candidate() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[360; 12]);
        let initial = r.candidate(&[], 0).unwrap().unwrap();
        r.admit(initial, &Cancellation::default()).unwrap();
        r.save_frontier().unwrap();
        let key = Cache::set_key(&policy).unwrap();
        let old_frontier = r
            .search
            .cache
            .read::<Value>(&store, "refinement-frontiers", &key)
            .unwrap();
        let measured = edge(&mut r, 0, 1, 25., 0.);
        r.blocks.insert(
            vec![measured],
            Some(Arc::new(Block {
                tracks: vec![0, 1],
                edges: vec![measured],
                plan: None,
                minimum: 180 * 48_000,
                maximum: 700 * 48_000,
            })),
        );
        r.scheduler.cache_loaded = true;
        r.scheduler.frontier_saved = Some(Instant::now());
        let improved = r.candidate(&[measured], 0).unwrap().unwrap();
        r.admit(improved, &Cancellation::default()).unwrap();
        assert_eq!(
            r.search
                .cache
                .read::<Value>(&store, "refinement-frontiers", &key)
                .unwrap(),
            old_frontier
        );
        assert!(r
            .search
            .cache
            .read::<Value>(&store, "refinement-best", &key)
            .unwrap()
            .is_some());
        r.population.clear();
        r.scheduler = Scheduler::default();
        r.best = None;
        r.seen.clear();
        r.history.clear();
        r.search.snapshot = store.read_snapshot();
        r.restore_frontier(&Cancellation::default()).unwrap();
        assert_eq!(r.best.as_ref().unwrap().edges(), vec![measured]);
    }
    #[test]
    fn duration_retention_preserves_distinct_terminal_tracks_and_entry_states() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[360; 12]);
        let make = |key: String, edge: usize, end: usize, maximum: u64, score: f64| Draft {
            blocks: vec![Arc::new(Block {
                tracks: vec![0, end],
                edges: vec![edge],
                plan: None,
                minimum: 100,
                maximum,
            })],
            lengths: vec![maximum],
            score,
            key,
        };
        let dominant = edge(&mut r, 0, 1, 25., 0.);
        // Both quality quotas are filled by variants of one long component.
        for i in 0..24 {
            r.population.push(make(
                format!("dominant-{i}"),
                dominant,
                1,
                3_000,
                100. + i as f64,
            ));
        }
        for last in 2..12 {
            let measured = edge(&mut r, 0, last, 5., 0.);
            r.population.push(make(
                format!("endpoint-{last}"),
                measured,
                last,
                2_900 - last as u64,
                1.,
            ));
        }
        r.prune();
        let terminals = r
            .population
            .iter()
            .filter_map(|d| r.continuation_state(d).map(|s| s.0))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            terminals.len(),
            9,
            "the duration lane adds eight useful endpoints"
        );
        assert_eq!(r.population.len(), 32);
        assert_eq!(
            r.scheduler.parked.len(),
            2,
            "unselected candidates remain resumable"
        );

        // With no new terminal song, an earlier measured entry is still a
        // distinct continuation opportunity and must survive cue variants.
        r.population.retain(|d| d.key.starts_with("dominant-"));
        let earlier = edge(&mut r, 0, 1, 5., 0.);
        Arc::make_mut(&mut r.edges[dominant]).entry_end = 200;
        Arc::make_mut(&mut r.edges[earlier]).entry_end = 100;
        r.population
            .push(make("earlier-entry".into(), earlier, 1, 2_900, 1.));
        r.prune();
        assert!(r.population.iter().any(|d| d.key == "earlier-entry"));
    }

    #[test]
    fn resumed_incumbent_prevents_weaker_restored_completion_from_replacing_cache() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[1_260; 3]);
        let mut good: MixPlan =
            serde_json::from_str(include_str!("../../../examples/mix-plan.json")).unwrap();
        // Stretch the metadata-only fixture's complete arrangement to58:48.
        // No source PCM or closing detector is needed to test resume ordering.
        for (i, placement) in good.placements.iter_mut().enumerate() {
            placement.track_id = r.search.pool[i].track_id.clone();
            placement.audio_hash = r.search.pool[i].audio_hash.clone();
            placement.source_start_frame *= 63;
            placement.source_end_frame *= 63;
            for anchor in &mut placement.time_map.anchors {
                anchor.source_frame *= 63;
                anchor.output_frame *= 63;
            }
            for point in &mut placement.automation {
                point.output_frame *= 63;
            }
        }
        for (i, transition) in good.transitions.iter_mut().enumerate() {
            transition.from_track_id = r.search.pool[i].track_id.clone();
            transition.to_track_id = r.search.pool[i + 1].track_id.clone();
            transition.start_output_frame *= 63;
            transition.end_output_frame *= 63;
        }
        good.total_output_frames *= 63;
        good.header
            .extensions
            .insert("chain_joins".into(), json!([{"score":25.},{"score":25.}]));
        let mut weak = good.clone();
        weak.header
            .extensions
            .insert("chain_joins".into(), json!([{"score":5.},{"score":5.}]));
        assert!(r
            .search
            .save(good, "test_previous_run", &Cancellation::default())
            .unwrap());
        let previous_score = crate::composition::score(r.search.incumbent.as_ref().unwrap());
        r.search.incumbent = None;
        r.restore_incumbent(&Cancellation::default()).unwrap();
        r.restore_frontier(&Cancellation::default()).unwrap();
        let resumed = r.search.incumbent.as_ref().unwrap();
        let resumed_id = resumed.plan_id.clone();
        assert_eq!(crate::composition::score(resumed), previous_score);
        // This is the same save path used when restoring a closable component.
        assert!(!r
            .search
            .save(weak, "whole_set_refinement", &Cancellation::default())
            .unwrap());
        assert_eq!(r.search.incumbent.as_ref().unwrap().plan_id, resumed_id);
        let cached = r.search.cache.saved_set(&store, &policy).unwrap().unwrap();
        assert_eq!(
            cached.plan_id, resumed_id,
            "the durable winner must also remain monotone"
        );

        let draft = r.candidate(&[], 0).unwrap().unwrap();
        r.admit(draft, &Cancellation::default()).unwrap();
        let cancel = Cancellation::default();
        cancel.cancel();
        let output = publish_result(
            r,
            "refinement_budget",
            StopReason::WallClock,
            30,
            12,
            0.,
            &cancel,
        )
        .unwrap();
        assert_eq!(output["search"]["termination"], "cancelled_with_incumbent");
        assert_eq!(output["plan"]["plan_id"], resumed_id);
        let published: MixPlan = store.read(&format!("plans/{resumed_id}.json")).unwrap();
        assert_eq!(
            published.header.extensions["set_refinement"]["stop_reason"],
            "cancelled"
        );
    }

    #[test]
    #[cfg(feature = "cp-search")]
    fn cp_stop_distinguishes_directed_infeasibility_from_no_draft_at_deadline() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let mut policy = MixingConfig::default();
        policy
            .header
            .extensions
            .insert(crate::set_config::ENGINE_KEY.into(), json!("cp"));
        policy
            .header
            .extensions
            .insert("lazy_search_seconds".into(), json!(0));
        let cancel = Cancellation::default();
        for (seconds, fork, reason) in [
            ([1200; 3], true, "directed_duration_capacity"),
            ([100; 3], false, "directed_duration_capacity"),
            // Replay or tempo changes could reach the floor. An all-nil
            // layout fits only original material at its native speed.
            ([1100; 3], false, "wall_clock"),
        ] {
            let mut r = fixture(&store, &policy, &seconds);
            r.cp = Some(cp_search::CpSearch::default());
            if fork {
                r.search
                    .excluded_transitions
                    .extend([(1, 0), (1, 2), (2, 0), (2, 1)]);
            }
            let output = run_prepared(r, &cancel).unwrap();
            assert_eq!(output["search"]["stop_reason"], reason, "{output}");
            let proved = reason == "directed_duration_capacity";
            assert_eq!(
                output["status"],
                if proved { "infeasible" } else { "pending" }
            );
            assert_eq!(
                output["search"]["termination"],
                if proved {
                    "infeasible_within_domain"
                } else {
                    "budget_without_incumbent"
                }
            );
            assert_eq!(output["search"]["measured_windows"], 0);
            assert!(output["plan"].is_null() && output["draft"].is_null());
            let cp = &output["search"]["cp_search"];
            assert_eq!(cp["attempts"], 0);
            assert!(cp["domain_manifest"]["id"].is_string());
            assert_eq!(output["search"]["infeasibility_proof"].is_object(), proved);
            assert_eq!(
                cp["score_coverage"]["routes_with_unmeasured_joins_empty"],
                proved
            );
            assert_eq!(
                output["search"]["certified_global_upper_bound"].is_null(),
                proved
            );
            let saved: Report = serde_json::from_slice(
                &std::fs::read(output["draft_path"].as_str().unwrap()).unwrap(),
            )
            .unwrap();
            assert_eq!(
                saved.header.extensions["search"]["termination"],
                output["search"]["termination"]
            );
        }
    }

    #[test]
    #[cfg(feature = "cp-search")]
    fn cp_rejects_required_chain_with_no_common_direction() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let mut policy = MixingConfig::default();
        policy
            .header
            .extensions
            .insert(crate::set_config::ENGINE_KEY.into(), json!("cp"));
        let mut r = fixture(&store, &policy, &[1200; 3]);
        r.cp = Some(cp_search::CpSearch::default());
        let ids = [0, 1, 2].map(|i| r.search.pool[i].track_id.clone());
        r.search.required = required_pairs::RequiredPairs::new(
            required_pairs::Mode::Either,
            [[0, 1], [1, 2]]
                .map(|indices| required_pairs::Pair {
                    indices,
                    spotify_ids: indices.map(|i| format!("spotify-{i}")),
                    track_ids: indices.map(|i| ids[i].clone()),
                })
                .to_vec(),
            3,
        )
        .unwrap();
        // Each pair has a permitted direction, but both require track 1
        // as the destination. Routes in the relaxed graph can still use
        // all tracks by visiting 0 and 2 consecutively.
        r.search.excluded_transitions.extend([(1, 0), (1, 2)]);
        assert!(r.search.permits_transition(0, 1) && r.search.permits_transition(2, 1));
        assert_ne!(r.search.directed_capacity().unwrap().viable_tracks, 0);
        let result = run_prepared(r, &Cancellation::default()).unwrap();
        let search = &result["search"];
        assert_eq!(result["status"], "infeasible");
        assert_eq!(search["termination"], "infeasible_within_domain");
        assert_eq!(search["stop_reason"], "required_adjacency_chain");
        assert_eq!(search["measured_windows"], 0);
        assert_eq!(search["cp_search"]["attempts"], 0);
        assert!(search["certified_global_upper_bound"].is_null());
        let proof = &search["infeasibility_proof"];
        assert_eq!(proof["chain_track_ids"], json!(ids));
        assert_eq!(proof["forward_blockers"][0]["to"], ids[2]);
        assert_eq!(proof["reverse_blockers"][0]["to"], ids[0]);
    }

    #[test]
    fn cancelled_draft_is_published_without_an_approved_plan() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = fixture(&store, &policy, &[360; 12]);
        let cancel = Cancellation::default();
        let draft = r.candidate(&[], 0).unwrap().unwrap();
        r.admit(draft, &cancel).unwrap();
        cancel.cancel();
        let output =
            publish_result(r, "cancelled", StopReason::Cancelled, 30, 12, 0., &cancel).unwrap();
        assert_eq!(
            output["search"]["termination"],
            "cancelled_without_incumbent"
        );
        assert_eq!(output["search"]["measured_windows"], 0);
        assert!(output["plan"].is_null());
        assert_eq!(output["draft"]["exportable"], false);
        let saved: Report =
            serde_json::from_slice(&std::fs::read(output["draft_path"].as_str().unwrap()).unwrap())
                .unwrap();
        assert_eq!(
            saved.header.extensions["search"]["stop_reason"],
            "cancelled"
        );
    }
}
