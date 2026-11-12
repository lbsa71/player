//! Whole-set local refinement. Nil is an unresolved planning edge, never an
//! executable or admitted musical tactic. Only a fully validated plan exports.
use super::*;
use crate::{discovery_schedule::{Probe, Schedule}, refinement_branches::Branch, refinement_layout};
use std::collections::VecDeque;

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
    proposed_pairs: BTreeSet<(usize,usize)>,
    measured_pairs: BTreeSet<(usize,usize)>,
    closing_attempts: usize,
    bound_pruned: usize,
    cache_loaded: bool,
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
    pool_len.saturating_sub(1) as f64 * mixlib_planner::MAX_TACTIC_CONTINUATION_SECONDS
        / (request.min() / 60.)
}
fn closing_capacity(maximum:u64,rate:f64,minimum_seconds:f64)->bool {
    maximum as f64/rate*(1.+MAX_REPLAY_FRACTION)+1e-6>=minimum_seconds
}
fn shuffled_key(index: usize, generation: usize) -> u64 {
    let mut x = (index as u64).wrapping_add((generation as u64).wrapping_mul(0x9e3779b97f4a7c15));
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
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
}
impl Refiner<'_> {
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
        for edge in self.search.graph.iter().flatten() {
            if self.edge_keys.insert(route_key(&edge.plan)?) {
                self.edges.push(edge.clone());
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
    fn block(&mut self, edges: &[usize]) -> Result<Option<Arc<Block>>> {
        if let Some(cached) = self.blocks.get(edges) {
            return Ok(cached.clone());
        }
        let mut plan = None;
        let mut tracks = vec![self.ends(edges[0]).0];
        let mut entry = 0;
        for &id in edges {
            let (from, to) = self.ends(id);
            if tracks.last() != Some(&from) || tracks.contains(&to) {
                return Ok(None);
            }
            let next = self.search.composer.extend(
                plan.as_ref(),
                &self.edges[id],
                from,
                entry,
                &self.search.pacing[from],
            )?;
            if next.is_none() {
                self.blocks.insert(edges.to_vec(), None);
                return Ok(None);
            }
            plan = next;
            entry = self.edges[id].entry_end;
            tracks.push(to);
        }
        let p = plan.as_ref().unwrap();
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
                plan,
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
    fn branch_step(&mut self, cancel: &Cancellation) -> Result<()> {
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
        let best = self.best.as_ref().unwrap();
        self.draft_report
            .header
            .extensions
            .insert("draft".into(), self.draft_value(best)?);
        self.draft_report.header.extensions.insert("search".into(),json!({"method":"complete-set-refinement/1","status":status,
            "prepared_tracks":self.search.pool.len(),"elapsed_seconds":self.search.started.elapsed().as_secs_f64(),
            "measured_windows":self.search.stats.measured_windows,"cached_windows":self.search.stats.cached_windows,
            "known_good_choices":self.edges.len(),"complete_drafts_examined":self.seen.len(),"rounds":self.rounds,
            "rejected_layouts":self.rejected_layouts,"history":self.history,
            "generated_layouts":self.scheduler.generation,"global_pair_cursor":self.scheduler.pool_cursor,"proposed_ordered_pairs":self.scheduler.proposed_pairs.len(),"measured_ordered_pairs":self.scheduler.measured_pairs.len(),"closing_attempts":self.scheduler.closing_attempts,"parked_candidates":self.scheduler.parked.len(),
            "bound_pruned":self.scheduler.bound_pruned,"fixed_order_branches":self.scheduler.branches.len(),"branches_expanded":self.scheduler.branches_expanded,"infeasible_committed_nil_branches":self.scheduler.impossible_branches,"optimistic_objective_bound":unrestricted_upper_bound(self.search.pool.len(),self.search.request),
            "bound_scope":"Unrestricted replacements; at most 120 sustained quality-seconds per possible unique-song join, all future penalties relaxed. Loose until more choices are fixed.",
            "best_plan_id":self.search.incumbent.as_ref().map(|p|&p.plan_id)}));
        self.search.store.report(&self.draft_report)?;
        if self.scheduler.cache_loaded {
            self.save_frontier()?;
        }
        eprintln!("set refinement: {status} | full {:.2} min draft | {} nil / {} measured joins | score {:.4} | {} fresh / {} cached windows | {:.1}s",
            best.lengths.iter().sum::<u64>() as f64/self.rate()/60.,best.nil(),best.edges().len(),best.score,
            self.search.stats.measured_windows,self.search.stats.cached_windows,self.search.started.elapsed().as_secs_f64());
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
            if !closing_capacity(block.maximum,self.rate(),self.search.request.min()) {continue;}
            if let Some(plan)=&block.plan {
                if self.search.required.satisfied(plan) && self.attempted_final.insert(block.edges.clone()) {
                    self.scheduler.closing_attempts+=1;
                    if let Some(finished)=self.search.composer.close(&self.search.snapshot,plan,cancel)? {
                        self.search.save(finished,"whole_set_refinement",cancel)?;
                    }
                }
            }
        }
        if self.best.as_ref().is_none_or(|old| draft.better(old)) {
            self.best = Some(draft.clone());
            self.history.push(json!({"nil_joins":draft.nil(),"resolved_joins":draft.edges().len(),"score":draft.score,
                "elapsed_seconds":self.search.started.elapsed().as_secs_f64(),"track_ids":draft.tracks().iter().map(|&i| &self.search.pool[i].track_id).collect::<Vec<_>>()}));
            self.checkpoint(if self.history.len() == 1 {
                "FULL_DRAFT_FOUND"
            } else {
                "BETTER_DRAFT_FOUND"
            })?;
        }
        self.seed_branch(&draft)?;
        self.population.push(draft);
        Ok(())
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
        self.population.sort_by(|a,b| {
            let length=|d:&Draft|d.blocks.iter().filter(|b|!b.edges.is_empty()).map(|b|b.maximum).max().unwrap_or(0);
            length(b).cmp(&length(a)).then_with(||b.score.total_cmp(&a.score))
        });
        for d in self.population.iter().take(8) {
            if !kept.iter().any(|k|k.key==d.key){kept.push(d.clone());}
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
    fn persistent_key(&self, index: usize) -> Result<String> {
        if let Some(key) = self.scheduler.identities.borrow().get(&index) {
            return Ok(key.clone());
        }
        let key = route_key(&self.edges[index].plan)?;
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
        let mut parts = vec![];
        for chunk in seeds.chunks(128) {
            let payload = json!(chunk);
            let part = canonical_hash(&payload)?.digest;
            self.search.cache.write(
                self.search.store,
                "refinement-frontier-parts",
                &part,
                &payload,
            )?;
            parts.push(part);
        }
        // Persist fixed-order work separately. Rebuild/rescore every restored
        // completion; no stale draft admission is trusted.
        let branches=self.scheduler.branches.iter().map(|b| {
            let choices=b.choices.iter().map(|i|i.map(|i|self.persistent_key(i)).transpose()).collect::<Result<Vec<_>>>()?;
            Ok(json!({"choices":choices,"tracks":b.order.iter().map(|&i|&self.search.pool[i].track_id).collect::<Vec<_>>(),"locked":b.locked}))
        }).collect::<Result<Vec<_>>>()?;
        let mut branch_parts = vec![];
        for chunk in branches.chunks(128) {
            let payload = json!(chunk);
            let part = canonical_hash(&payload)?.digest;
            self.search.cache.write(
                self.search.store,
                "refinement-frontier-parts",
                &part,
                &payload,
            )?;
            branch_parts.push(part);
        }
        self.search.cache.write(self.search.store,"refinement-frontiers",&key,&json!({
            "version":2,"sources":self.search.cache.sources,"generation":self.scheduler.generation,"pool_cursor":self.scheduler.pool_cursor,"parts":parts,"branch_parts":branch_parts}))
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
                self.scheduler.pool_cursor=saved["pool_cursor"].as_u64().unwrap_or(0) as usize;
                let mut lookup = self
                    .edges
                    .iter()
                    .enumerate()
                    .map(|(i, e)| Ok((route_key(&e.plan)?, i)))
                    .collect::<Result<BTreeMap<_, _>>>()?;
                for (legacy, key) in &self.search.edge_aliases {
                    if let Some(&index) = lookup.get(key) {
                        lookup.insert(legacy.clone(), index);
                    }
                }
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
                        let Some(seeds) = self.search.cache.read::<Vec<Value>>(
                            self.search.store,
                            "refinement-frontier-parts",
                            &part,
                        )?
                        else {
                            continue;
                        };
                        for seed in seeds {
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
        self.scheduler.cache_loaded = true;
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
            let cursor=self.scheduler.repair_cursor % ids.len().max(1);
            ids.rotate_left(cursor);
            for (offset,&edge) in ids.iter().enumerate() {
                self.scheduler.repair_cursor=(cursor+offset+1)%ids.len().max(1);
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
    fn minimum_after(&self,track:usize,entry:u64)->u64 {
        let rate=f64::from(self.search.pool[track].format.sample_rate_hz);
        let development=(90f64).min(self.track_seconds(track)*0.45)*(1.+self.search.policy.max_speed_deviation);
        self.search.pacing[track].earliest_exit(entry).max(entry.saturating_add((development*rate).ceil() as u64))
    }
    fn demands(&self)->Vec<Probe> {
        let mut output=vec![];let mut seen=BTreeSet::new();
        let drafts=self.best.iter().chain(self.population.iter().take(12));
        for draft in drafts {
            // Direct unresolved boundaries first, then alternatives that reorder
            // retained components. A separate global lane cannot be starved.
            let mut pairs=(0..draft.blocks.len().saturating_sub(1)).map(|i|(i,i+1)).collect::<Vec<_>>();
            for a in 0..draft.blocks.len(){for b in 0..draft.blocks.len(){if a!=b{pairs.push((a,b));}}}
            for (a,b) in pairs {
                let from=*draft.blocks[a].tracks.last().unwrap();let to=draft.blocks[b].tracks[0];
                let entry=draft.blocks[a].edges.last().map_or(0,|&i|self.edges[i].entry_end);
                let minimum=self.minimum_after(from,entry);
                let frames=self.search.pool[from].format.frames;
                for fraction in [0.60,0.80,0.35,0.] {
                    let probe=Probe{from,to,after:minimum.max((frames as f64*fraction) as u64),tier:1};
                    if probe.after<frames && seen.insert(probe){output.push(probe);}
                }
                // Event/long-hold proposals may earn development credit and keep
                // the original phrase-aware lower bound available as well.
                for tier in [0,24] {
                    let probe=Probe{from,to,after:self.search.pacing[from].earliest_exit(entry),tier};
                    if seen.insert(probe){output.push(probe);}
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
    pool: Vec<Track>,
    request: Request,
    cancel: &Cancellation,
) -> Result<Value> {
    let search = new_search(store, policy, context, pool, request, cancel)?;
    let mut r = Refiner {
        search,
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
    };
    r.search.snapshot.verify_read_snapshot(cancel)?;
    for salt in 0..8 {
        if let Some(d) = r.candidate(&[], salt)? {
            r.admit(d, cancel)?;
        }
    }
    require(
        r.best.is_some(),
        "SET_DRAFT_CAPACITY",
        "cannot fit a full draft from available source capacity and required adjacencies",
    )?;
    // Baseline exists before cache loading or new PCM work. The budget now bounds
    // the entire refinement phase, even when no approved full set can be found.
    let option = |key: &str, default: u64| {
        policy
            .header
            .extensions
            .get(key)
            .and_then(Value::as_u64)
            .unwrap_or(default)
    };
    let deadline =
        Instant::now() + std::time::Duration::from_secs(option("lazy_search_seconds", 1200));
    let measurement_limit = option("lazy_measurement_budget", 1536) as usize;
    r.search.load_known(cancel)?;
    r.sync_edges()?;
    r.restore_frontier(cancel)?;
    if let Some(plan) = r.search.cache.saved_set(store, policy)? {
        if r.search.required.satisfied(&plan) {
            r.search.save(plan, "cached_complete_set", cancel)?;
        }
    }
    r.repair(
        cancel,
        deadline.min(Instant::now() + std::time::Duration::from_secs(1)),
    )?;
    let mut discovery=Schedule::new(r.scheduler.pool_cursor);
    let frames=r.search.pool.iter().map(|t|t.format.frames).collect::<Vec<_>>();
    let minimum=(0..r.search.pool.len()).map(|i|r.minimum_after(i,0)).collect::<Vec<_>>();
    let mut measured=BTreeSet::new();
    let mut pending=VecDeque::new();
    let mut exploration=VecDeque::from(crate::historical_seeds::load(store,policy,&r.search.pool,&r.search.cache,cancel)?);
    let mut progress=Instant::now();
    let mut status="refinement_budget";
    let generation_limit=measurement_limit.saturating_mul(2).clamp(64,10000);
    let generation_start=r.scheduler.generation;
    let mut turns=0usize;
    while Instant::now()<deadline && r.search.stats.measured_windows<measurement_limit {
        cancel.check()?;
        if progress.elapsed().as_secs()>=10 {r.checkpoint("refining")?;progress=Instant::now();}
        turns+=1;
        r.branch_step(cancel)?;
        if turns%4==0 && r.scheduler.generation-generation_start<generation_limit {
            r.regenerate(cancel)?;
        }
        let mut batch=vec![];
        let mut probes=0;
        while batch.len()<4.min(measurement_limit-r.search.stats.measured_windows) && Instant::now()<deadline {
            // Reserve one of every four measurements for event/dynamic and
            // historical proposals. Baseline coverage keeps three lanes.
            let seed=if batch.len()==3 && !exploration.is_empty(){exploration.pop_front()}else{pending.pop_front()};
            if let Some(seed)=seed {
                let key=window_key(&r.search.cache,&seed)?;
                if measured.insert(key){batch.push(seed);}
                continue;
            }
            let targets=r.demands();
            let Some(probe)=discovery.next(&targets,&frames,&minimum) else {
                if let Some(seed)=exploration.pop_front(){pending.push_back(seed);continue;}
                status="generated_probe_scope_exhausted";break;
            };
            r.scheduler.pool_cursor=discovery.pool_cursor;
            r.scheduler.proposed_pairs.insert((probe.from,probe.to));
            let proposals=r.search.proposals(probe.from,probe.to,probe.after,probe.tier,cancel)?;
            if probe.tier==1 {pending.extend(proposals);} else {exploration.extend(proposals);}
            probes+=1;
            if probes>=32 && batch.is_empty() && !exploration.is_empty(){pending.push_back(exploration.pop_front().unwrap());}
        }
        if batch.is_empty(){if status=="generated_probe_scope_exhausted"{break;}continue;}
        for seed in &batch {
            let from=r.search.pool.iter().position(|t|t.track_id==seed.placements[0].track_id).unwrap();
            let to=r.search.pool.iter().position(|t|t.track_id==seed.placements[1].track_id).unwrap();
            r.scheduler.measured_pairs.insert((from,to));
        }
        for result in r.search.measure_batch(&batch,cancel)? {exploration.extend(result.continuations);}
        r.sync_edges()?;
        // A small fair mutation slice after every batch prevents both cached
        // permutations and high-ranked early edges from starving new bridges.
        r.repair(cancel,deadline.min(Instant::now()+std::time::Duration::from_millis(400)))?;
    }
    r.search.snapshot.verify_read_snapshot(cancel)?;
    r.checkpoint(status)?;
    let path = store.path(&format!("reports/{}.json", r.draft_report.report_id))?;
    let draft = r.draft_report.header.extensions["draft"].clone();
    let search = r.draft_report.header.extensions["search"].clone();
    if let Some(plan) = &mut r.search.incumbent {
        plan.header
            .extensions
            .insert("set_refinement".into(), search.clone());
        store.write(&format!("plans/{}.json", plan.plan_id), plan)?;
    }
    Ok(
        json!({"status":if r.search.incumbent.is_some(){"ready"}else{"draft"},"plan":r.search.incumbent,
        "draft":draft,"draft_path":path,"search":search,
        "export_gate":"An approved plan is returned only after every nil join is replaced and musical bookends, duration, pacing and required pairs pass."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture<'a>(store: &'a Store, policy: &'a MixingConfig, durations: &[u64]) -> Refiner<'a> {
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
            pacing: pool.iter().map(|_| TrackPacing::unknown(48000)).collect(),
            cache: Cache::open(policy, &pool).unwrap(),
            facts: CandidateEvidenceCache::new(),
            event_cues: EventCueIndex::new(12),
            baseline_cues: BaselineCueIndex::new(12),
            worker_facts: vec![],
            measured_keys: BTreeSet::new(),
            stats: Stats::default(),
            started: Instant::now(),
            progress: Report::new("test_search"),
            history: vec![],
            incumbent: None,
            graph: vec![vec![]; pool.len() * pool.len()],
            known: BTreeSet::new(),
            edge_aliases: BTreeMap::new(),
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
        }
    }

    fn edge(r: &mut Refiner<'_>, a: usize, b: usize, score: f64, dense: f64) -> usize {
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
        r.branch_step(&Cancellation::default()).unwrap();
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
        r.branch_step(&Cancellation::default()).unwrap();
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
        let old_edge_key = route_key(&r.edges[0].plan).unwrap();
        r.save_frontier().unwrap();

        let mut rebound = (*r.edges[0]).clone();
        crate::search_cache::rebind(&mut rebound.plan, &changed_policy).unwrap();
        rebound.plan.plan_id = id();
        rebound.plan.transitions[0].transition_id = id();
        for placement in &mut rebound.plan.placements {
            placement.time_map.map_id = id();
        }
        assert_eq!(route_key(&rebound.plan).unwrap(), old_edge_key);
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
}
