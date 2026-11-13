//! Fair scheduling and durable timing coverage for one cue family.
use super::*;
use mixlib_planner::{BarCueDomain, StructuralCueDomain, BAR_CUE_PROFILE, STRUCTURAL_CUE_PROFILE};

#[derive(Clone, Copy)]
pub(in super::super) enum CueFamily {
    Bar,
    Structural,
}

impl CueFamily {
    fn profile(self) -> &'static str {
        match self {
            Self::Bar => BAR_CUE_PROFILE,
            Self::Structural => STRUCTURAL_CUE_PROFILE,
        }
    }

    fn cache_kind(self) -> &'static str {
        match self {
            Self::Bar => "cp-bar-frontiers",
            Self::Structural => "cp-structural-frontiers",
        }
    }

    fn open(
        self,
        store: &Store,
        from: &str,
        to: &str,
        context: usize,
        policy: &MixingConfig,
        cancel: &Cancellation,
    ) -> Result<Option<Domain>> {
        match self {
            Self::Bar => Ok(Some(Domain::Bar(BarCueDomain::open(
                store, from, to, context, policy, cancel,
            )?))),
            Self::Structural => Ok(StructuralCueDomain::open(
                store, from, to, context, policy, cancel,
            )?
            .map(Domain::Structural)),
        }
    }
}

enum Domain {
    Bar(BarCueDomain),
    Structural(StructuralCueDomain),
}

impl Domain {
    fn range(&self) -> BarCueRange {
        match self {
            Self::Bar(domain) => domain.range(),
            Self::Structural(domain) => domain.range(),
        }
    }

    fn page(
        &self,
        store: &Store,
        range: &BarCueRange,
        scan_limit: usize,
        plan_limit: usize,
        cancel: &Cancellation,
    ) -> Result<BarCuePage> {
        match self {
            Self::Bar(domain) => domain.page(store, range, scan_limit, plan_limit, cancel),
            Self::Structural(domain) => domain.page(range, scan_limit, plan_limit, cancel),
        }
    }
}

struct Scan {
    pair: (usize, usize),
    domain: Domain,
    pages: usize,
}

#[derive(Default, Serialize, Deserialize)]
struct SavedFrontier {
    queued: VecDeque<(usize, usize)>,
    finished: Vec<((usize, usize), BarCueRange)>,
    #[serde(skip)]
    pair_cursor: usize,
    starts: usize,
}

pub(super) struct CueScans {
    family: CueFamily,
    frontier: SavedFrontier,
    key: Option<String>,
    seen: BTreeSet<(usize, usize)>,
    total_pairs: usize,
    scan: Option<Scan>,
    deferred: Vec<(usize, usize)>,
    missing_analysis: usize,
    progress: BTreeMap<(usize, usize), BarCoverage>,
    run_counts: BarCounts,
}

pub(super) struct TimingScans {
    pub(super) bar: CueScans,
    pub(super) structural: CueScans,
}

impl Default for TimingScans {
    fn default() -> Self {
        Self {
            bar: CueScans::new(CueFamily::Bar),
            structural: CueScans::new(CueFamily::Structural),
        }
    }
}

impl CueScans {
    fn new(family: CueFamily) -> Self {
        Self {
            family,
            frontier: SavedFrontier::default(),
            key: None,
            seen: BTreeSet::new(),
            total_pairs: 0,
            scan: None,
            deferred: vec![],
            missing_analysis: 0,
            progress: BTreeMap::new(),
            run_counts: BarCounts::default(),
        }
    }

    pub(super) fn evidence(&self) -> Value {
        let mut coverage = BarCounts::default();
        let mut finished = 0;
        let mut proven_empty = 0;
        for progress in self.progress.values() {
            coverage.accumulate(&progress.counts);
            finished += usize::from(progress.finished());
            proven_empty += usize::from(progress.proven_empty());
        }
        let queued = self.frontier.queued.len() + usize::from(self.scan.is_some());
        let unstarted = self.total_pairs.saturating_sub(self.seen.len());
        let mut report = json!({
            "profile": self.family.profile(),
            "scanned_cells": self.run_counts.total(),
            "block_pruned_cells": self.run_counts.block_rejected,
            "row_pruned_cells": self.run_counts.row_rejected,
            "processed_timing_windows": self.run_counts.measured + self.run_counts.unknown,
            "unknown_timing_windows": self.run_counts.unknown,
            "selector_partitioned_timing_windows": self.run_counts.selector_partitioned,
            "selector_resolved_timing_windows": self.run_counts.selector_resolved,
            "selector_empty_timing_windows": self.run_counts.selector_empty,
            "verified_loaded_coverage": coverage,
            "eligible_directed_pairs": self.total_pairs,
            "finished_pairs": finished,
            "proven_empty_pinned_domains": proven_empty,
            "queued_pairs": queued,
            "unstarted_pairs": unstarted,
            "deferred_pairs": self.deferred.len(),
            "missing_analysis_pairs": self.missing_analysis,
        });
        if matches!(self.family, CueFamily::Bar) {
            report["finished_bar_scans"] = json!(finished);
            report["queued_bar_scans"] = json!(queued);
            report["unstarted_bar_scans"] = json!(unstarted);
            report["deferred_bar_scans"] = json!(self.deferred.len());
        }
        report
    }

    pub(super) fn prioritize(&mut self, pair: (usize, usize)) {
        if self.seen.insert(pair) {
            self.frontier.queued.push_back(pair);
        }
    }

    pub(super) fn restore(&mut self, search: &mut Search<'_>, cancel: &Cancellation) -> Result<()> {
        let ordered_sources = search
            .pool
            .iter()
            .map(|t| (&t.track_id, &search.cache.sources[&t.track_id]))
            .collect::<Vec<_>>();
        let key = canonical_hash(&json!({
            "frontier": "all-directed-pairs/2",
            "method": self.family.profile(),
            "sources": ordered_sources,
            "context": search.context,
            "set": Cache::set_key(search.policy)?,
        }))?
        .digest;
        let n = search.pool.len();
        let eligible = (0..n)
            .flat_map(|from| (0..n).map(move |to| (from, to)))
            .filter(|&(from, to)| search.schedules_transition(from, to))
            .collect::<BTreeSet<_>>();
        self.total_pairs = eligible.len();
        self.frontier = search
            .cache
            .read::<SavedFrontier>(search.store, self.family.cache_kind(), &key)?
            .unwrap_or_default();
        for (pair, root) in std::mem::take(&mut self.frontier.finished) {
            cancel.check()?;
            if !search.budget_open() {
                return Ok(());
            }
            if !eligible.contains(&pair) || self.progress.contains_key(&pair) {
                continue;
            }
            let progress = BarCoverage::restore(search, root, cancel)?;
            if progress.finished() {
                self.seen.insert(pair);
            } else {
                self.frontier.queued.push_back(pair);
            }
            self.progress.insert(pair, progress);
        }
        self.frontier
            .queued
            .retain(|pair| eligible.contains(pair) && self.seen.insert(*pair));
        self.key = Some(key);
        Ok(())
    }

    pub(super) fn save(&self, search: &Search<'_>) -> Result<()> {
        if let Some(key) = &self.key {
            let queued = self
                .scan
                .iter()
                .map(|scan| scan.pair)
                .chain(self.frontier.queued.iter().copied())
                .chain(self.deferred.iter().copied())
                .collect();
            let frontier = SavedFrontier {
                queued,
                finished: self
                    .progress
                    .iter()
                    .filter(|(_, progress)| progress.finished())
                    .map(|(&pair, progress)| (pair, progress.root().clone()))
                    .collect(),
                pair_cursor: 0,
                starts: self.frontier.starts,
            };
            search
                .cache
                .write(search.store, self.family.cache_kind(), key, &frontier)?;
        }
        Ok(())
    }

    pub(super) fn has_work(&self) -> bool {
        self.scan.is_some()
            || !self.frontier.queued.is_empty()
            || self.seen.len() < self.total_pairs
    }

    fn next_pair(&mut self, search: &Search<'_>) -> Option<(usize, usize)> {
        let n = search.pool.len();
        let width = n.saturating_sub(1);
        if self.frontier.queued.is_empty() || self.frontier.starts % 4 == 3 {
            while self.frontier.pair_cursor < n * width {
                let index = self.frontier.pair_cursor;
                self.frontier.pair_cursor += 1;
                let from = index / width;
                let col = index % width;
                let to = col + usize::from(col >= from);
                if search.schedules_transition(from, to) && self.seen.insert((from, to)) {
                    self.frontier.starts += 1;
                    return Some((from, to));
                }
            }
        }
        let pair = self.frontier.queued.pop_front()?;
        self.frontier.starts += 1;
        Some(pair)
    }

    pub(super) fn page(
        &mut self,
        search: &mut Search<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<BarCuePage>> {
        if self.scan.is_none() {
            let Some(pair) = self.next_pair(search) else {
                return Ok(None);
            };
            let Some(domain) = self.family.open(
                &search.snapshot,
                &search.pool[pair.0].track_id,
                &search.pool[pair.1].track_id,
                search.context,
                search.policy,
                cancel,
            )?
            else {
                self.missing_analysis += 1;
                self.deferred.push(pair);
                return Ok(None);
            };
            let root = domain.range();
            if self
                .progress
                .get(&pair)
                .is_none_or(|progress| progress.root() != &root)
            {
                self.progress
                    .insert(pair, BarCoverage::restore(search, root, cancel)?);
            }
            if self.progress[&pair].finished() {
                // Empty roots and recovered cursors can finish without a page
                // commit. Record their scheduling position at that boundary.
                self.save(search)?;
                return Ok(None);
            }
            self.scan = Some(Scan {
                pair,
                domain,
                pages: 0,
            });
        }
        let scan = self.scan.as_ref().unwrap();
        let remainder = self.progress[&scan.pair].remainder();
        scan.domain
            .page(&search.snapshot, &remainder, 256, 1, cancel)
            .map(Some)
    }

    pub(super) fn commit(&mut self, search: &Search<'_>, proof: PageProof) -> Result<()> {
        if !proof.observations_ready(search) {
            // An original measurement or legacy review refresh is still
            // pending. Keep this cursor and let other pairs proceed this run.
            let scan = self.scan.take().expect("page belongs to the active scan");
            self.deferred.push(scan.pair);
            return Ok(());
        }
        let scan = self.scan.as_mut().expect("page belongs to the active scan");
        let progress = self
            .progress
            .get_mut(&scan.pair)
            .expect("opened bar domain");
        self.run_counts.accumulate(&progress.commit(search, proof)?);
        scan.pages += 1;
        let rotate = progress.finished() || scan.pages >= 4;
        let save_position = rotate || scan.pages == 1;
        if rotate {
            if !progress.finished() {
                self.frontier.queued.push_back(scan.pair);
            }
            self.scan = None;
        }
        // The cursor is durable. Save its place in the schedule on first
        // progress and rotation, so it survives a deadline before checkpoint.
        if save_position {
            self.save(search)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::required_pairs::test_support;

    #[test]
    fn timing_slice_completes_several_empty_pairs_without_pcm_and_persists_them() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(&directory.path().join("library")).unwrap();
        let music = directory.path().join("music");
        std::fs::create_dir(&music).unwrap();
        for (name, frequency) in [("a.wav", 220.), ("b.wav", 330.), ("c.wav", 440.)] {
            test_support::tone(&music.join(name), 1., frequency);
        }
        let cancel = Cancellation::default();
        let mut config: AnalysisConfig = store.read("config/analysis.json").unwrap();
        config.settling_ms = 0;
        store.write("config/analysis.json", &config).unwrap();
        scan(&store, &[music], false, &cancel).unwrap();
        mixlib_analysis::analyze(&store, true, true, &cancel).unwrap();

        let policy = MixingConfig::default();
        let mut r = super::super::super::tests::fixture(&store, &policy, &[1200; 3]);
        // The mock route search keeps its own duration context. Supply the
        // analyzed short recordings to the timing scanner, which only reads
        // metadata and has no complete overlap with twelve beats of context.
        r.search.pool = store.tracks().unwrap();
        r.search.cache = Cache::open(&policy, &r.search.pool).unwrap();
        r.search.snapshot = store.read_snapshot();
        let mut cp = CpSearch::default();
        cp.restore_cue_frontiers(&mut r.search, &cancel).unwrap();
        let mut scheduled = BTreeSet::new();
        let mut batch = Vec::new();
        assert!(cp
            .schedule_cue_page(&mut r.search, 1, &mut scheduled, &mut batch, &cancel)
            .unwrap()
            .is_none());
        assert!(scheduled.is_empty() && batch.is_empty());
        let completed = cp.timing.bar.evidence()["proven_empty_pinned_domains"]
            .as_u64()
            .unwrap();
        assert!(completed > 0);
        assert_eq!(cp.timing.structural.evidence()["unstarted_pairs"], 6);
        assert_eq!(r.search.stats.measured_windows, 0);
        assert!(r.search.schedules_transition(0, 1));

        let mut resumed = CpSearch::default();
        resumed
            .restore_cue_frontiers(&mut r.search, &cancel)
            .unwrap();
        assert_eq!(
            resumed.timing.bar.evidence()["proven_empty_pinned_domains"],
            completed
        );
        for _ in 0..6 {
            assert!(resumed
                .schedule_cue_page(&mut r.search, 1, &mut scheduled, &mut batch, &cancel)
                .unwrap()
                .is_none());
        }
        assert_eq!(
            resumed.timing.bar.evidence()["proven_empty_pinned_domains"],
            6
        );
        assert!(!resumed.timing.bar.has_work());
    }

    #[test]
    fn capacity_excluded_pair_gets_no_cue_pages_or_window_measurement() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::super::tests::fixture(&store, &policy, &[1200; 3]);
        let cancel = Cancellation::default();
        let allowed = [(0, 1), (1, 2), (0, 2)];
        r.search.excluded_transitions = (0..3)
            .flat_map(|from| (0..3).map(move |to| (from, to)))
            .filter(|pair| !allowed.contains(pair))
            .collect();
        r.search.capacity_proof = r.search.directed_capacity();
        assert_eq!(
            r.search.capacity_proof.as_ref().unwrap().viable_tracks,
            0b0111
        );
        assert!(r.search.permits_transition(0, 2));
        assert!(!r.search.schedules_transition(0, 2));
        for family in [CueFamily::Bar, CueFamily::Structural] {
            let mut scans = CueScans::new(family);
            r.search.capacity_proof = None;
            scans.restore(&mut r.search, &cancel).unwrap();
            scans.prioritize((0, 2));
            scans.save(&r.search).unwrap();
            r.search.capacity_proof = r.search.directed_capacity();
            let mut scans = CueScans::new(family);
            scans.restore(&mut r.search, &cancel).unwrap();
            assert!(scans.frontier.queued.is_empty());
            assert_eq!(scans.total_pairs, 2);
            assert_eq!(scans.next_pair(&r.search), Some((0, 1)));
            assert_eq!(scans.next_pair(&r.search), Some((1, 2)));
            assert_eq!(scans.next_pair(&r.search), None);
            assert!(!scans.has_work());
        }
        assert!(r.search.proposals(0, 2, 0, 1, &cancel).unwrap().is_empty());
        let edge = super::super::super::tests::edge(&mut r, 0, 2, 35., 0.);
        let seed = r.edges[edge].plan.clone();
        r.search
            .measure_batch(std::slice::from_ref(&seed), &cancel)
            .unwrap();
        assert_eq!(r.search.stats.capacity_skipped_windows, 1);
        assert_eq!(r.search.stats.measured_windows, 0);
        assert!(r.search.worker_facts.is_empty());
        assert!(r.search.measured_keys.is_empty());
        let key = window_key(&r.search.cache, &seed).unwrap();
        assert!(r
            .search
            .cache
            .read::<Measurement>(&store, "windows", &key)
            .unwrap()
            .is_none());
    }

    #[test]
    fn each_frontier_reserves_global_turns_and_resumes_without_losing_pairs() {
        for family in [CueFamily::Bar, CueFamily::Structural] {
            check_frontier(family);
        }
    }

    fn check_frontier(family: CueFamily) {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut refiner = super::super::super::tests::fixture(&store, &policy, &[360; 4]);
        let cancel = Cancellation::default();
        let mut cp = CueScans::new(family);
        cp.restore(&mut refiner.search, &cancel).unwrap();
        let proposed = [(3, 0), (3, 1), (3, 2), (2, 1)];
        cp.frontier.queued.extend(proposed);
        cp.seen.extend(proposed);
        for expected in [(3, 0), (3, 1), (3, 2), (0, 1)] {
            assert_eq!(cp.next_pair(&refiner.search), Some(expected));
            let root = BarCueRange {
                domain_id: canonical_hash(&expected).unwrap().digest,
                start: 0,
                end: 0,
            };
            cp.progress.insert(
                expected,
                BarCoverage::restore(&mut refiner.search, root, &cancel).unwrap(),
            );
        }
        cp.save(&refiner.search).unwrap();
        let mut resumed = CueScans::new(family);
        resumed.restore(&mut refiner.search, &cancel).unwrap();
        assert_eq!(resumed.frontier.pair_cursor, 0);
        assert_eq!(resumed.progress.len(), 4);
        assert_eq!(resumed.frontier.queued.front(), Some(&(2, 1)));
        let mut visited = resumed.progress.keys().copied().collect::<BTreeSet<_>>();
        while let Some(pair) = resumed.next_pair(&refiner.search) {
            assert!(visited.insert(pair));
        }
        assert_eq!(visited.len(), 12);
        assert!(!resumed.has_work());
    }

    #[test]
    fn a_finished_pair_without_its_coverage_checkpoint_is_requeued() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut refiner = super::super::super::tests::fixture(&store, &policy, &[360; 3]);
        let cancel = Cancellation::default();
        let mut cp = CueScans::new(CueFamily::Bar);
        cp.restore(&mut refiner.search, &cancel).unwrap();
        let key = cp.key.unwrap();
        let pair = (0, 1);
        let root = BarCueRange {
            domain_id: canonical_hash(&pair).unwrap().digest,
            start: 0,
            end: 7,
        };
        let frontier = SavedFrontier {
            finished: vec![(pair, root)],
            ..SavedFrontier::default()
        };
        refiner
            .search
            .cache
            .write(&store, "cp-bar-frontiers", &key, &frontier)
            .unwrap();
        let mut resumed = CueScans::new(CueFamily::Bar);
        resumed.restore(&mut refiner.search, &cancel).unwrap();
        assert_eq!(resumed.evidence()["finished_bar_scans"], 0);
        assert_eq!(resumed.next_pair(&refiner.search), Some(pair));
    }
}
