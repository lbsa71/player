//! Durable coverage for the pinned bar timing family.
use super::*;

const CURSORS: &str = "cp-bar-coverage-cursors";
const PAGES: &str = "cp-bar-coverage-pages";

#[derive(Clone, Default, Serialize)]
pub(super) struct Counts {
    pub(super) block_rejected: u64,
    pub(super) row_rejected: u64,
    pub(super) cell_rejected: u64,
    pub(super) map_rejected: u64,
    pub(super) measured: u64,
    pub(super) unknown: u64,
    pub(super) selector_partitioned: u64,
    pub(super) selector_resolved: u64,
    pub(super) selector_empty: u64,
}

impl Counts {
    pub(super) fn total(&self) -> u64 {
        self.block_rejected + self.cell_rejected + self.map_rejected + self.measured + self.unknown
    }

    pub(super) fn accumulate(&mut self, other: &Self) {
        self.block_rejected += other.block_rejected;
        self.row_rejected += other.row_rejected;
        self.cell_rejected += other.cell_rejected;
        self.map_rejected += other.map_rejected;
        self.measured += other.measured;
        self.unknown += other.unknown;
        self.selector_partitioned += other.selector_partitioned;
        self.selector_resolved += other.selector_resolved;
        self.selector_empty += other.selector_empty;
    }
}

#[derive(Serialize, Deserialize)]
pub(in super::super) struct PageProof {
    scanned: BarCueRange,
    outcomes: Vec<Span>,
    pub(in super::super) windows: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct Span {
    start: u64,
    end: u64,
    outcome: String,
}

impl PageProof {
    pub(in super::super) fn new(page: &BarCuePage, windows: Vec<String>) -> Self {
        Self {
            scanned: page.scanned.clone(),
            outcomes: page
                .outcomes
                .iter()
                .map(|span| Span {
                    start: span.start,
                    end: span.end,
                    outcome: span.outcome.label().into(),
                })
                .collect(),
            windows,
        }
    }

    pub(super) fn observations_ready(&self, search: &Search<'_>) -> bool {
        self.windows.iter().all(|key| {
            search.measured_keys.contains(key) && !search.oracle.needs_selector_refresh(key)
        })
    }

    fn check(&self, root: &BarCueRange, search: &Search<'_>) -> Option<Counts> {
        if self.scanned.domain_id != root.domain_id
            || self.scanned.start >= self.scanned.end
            || self.scanned.end > root.end
            || !self.observations_ready(search)
        {
            return None;
        }
        let mut next = self.scanned.start;
        let mut windows = self.windows.iter();
        let mut counts = Counts::default();
        for span in &self.outcomes {
            if span.start != next || span.end <= span.start || span.end > self.scanned.end {
                return None;
            }
            let cells = span.end - span.start;
            match span.outcome.as_str() {
                "block_timing_rejected" => counts.block_rejected += cells,
                "row_timing_rejected" => {
                    counts.block_rejected += cells;
                    counts.row_rejected += cells;
                }
                "cell_timing_rejected" => counts.cell_rejected += cells,
                "pinned_map_rejected" => counts.map_rejected += cells,
                "timing_window" => {
                    let key = windows.next()?;
                    if cells != 1 {
                        return None;
                    }
                    if search.oracle.is_unknown(key) {
                        counts.unknown += 1;
                    } else {
                        counts.measured += 1;
                    }
                    if let Some(resolved) = search.oracle.selector_resolved(key) {
                        counts.selector_partitioned += 1;
                        counts.selector_resolved += u64::from(resolved);
                        counts.selector_empty +=
                            u64::from(resolved && search.oracle.selector_empty(key));
                    }
                }
                _ => return None,
            }
            next = span.end;
        }
        (next == self.scanned.end && windows.next().is_none()).then_some(counts)
    }
}

#[derive(Serialize, Deserialize)]
struct Page {
    previous: Option<String>,
    proof: PageProof,
}

#[derive(Serialize, Deserialize)]
struct Cursor {
    root: BarCueRange,
    next: u64,
    head: Option<String>,
}

pub(super) struct BarCoverage {
    cursor: Cursor,
    pub(super) counts: Counts,
}

impl BarCoverage {
    pub(super) fn restore(
        search: &mut Search<'_>,
        root: BarCueRange,
        cancel: &Cancellation,
    ) -> Result<Self> {
        let saved = search
            .cache
            .read::<Cursor>(search.store, CURSORS, &root.domain_id)?;
        if let Some(cursor) = saved.filter(|cursor| cursor.root == root) {
            if let Some(counts) = Self::validate_chain(search, &cursor, cancel)? {
                return Ok(Self { cursor, counts });
            }
        }
        Ok(Self {
            cursor: Cursor {
                next: root.start,
                root,
                head: None,
            },
            counts: Counts::default(),
        })
    }

    fn validate_chain(
        search: &mut Search<'_>,
        cursor: &Cursor,
        cancel: &Cancellation,
    ) -> Result<Option<Counts>> {
        if cursor.next < cursor.root.start || cursor.next > cursor.root.end {
            return Ok(None);
        }
        let mut next = cursor.next;
        let mut head = cursor.head.clone();
        let mut totals = Counts::default();
        while let Some(key) = head {
            cancel.check()?;
            if !search.budget_open() {
                return Ok(None);
            }
            if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Ok(None);
            }
            let Some(page) = search.cache.read::<Page>(search.store, PAGES, &key)? else {
                return Ok(None);
            };
            if page.proof.scanned.end != next
                || page.proof.scanned.start < cursor.root.start
                || canonical_hash(&page)?.digest != key
            {
                return Ok(None);
            }
            let Some(counts) = page.proof.check(&cursor.root, search) else {
                return Ok(None);
            };
            totals.accumulate(&counts);
            next = page.proof.scanned.start;
            head = page.previous;
        }
        Ok((next == cursor.root.start).then_some(totals))
    }

    pub(super) fn root(&self) -> &BarCueRange {
        &self.cursor.root
    }

    pub(super) fn remainder(&self) -> BarCueRange {
        BarCueRange {
            start: self.cursor.next,
            ..self.cursor.root.clone()
        }
    }

    pub(super) fn finished(&self) -> bool {
        self.cursor.next == self.cursor.root.end
    }

    pub(super) fn proven_empty(&self) -> bool {
        self.finished() && self.counts.measured + self.counts.unknown == self.counts.selector_empty
    }

    pub(super) fn commit(&mut self, search: &Search<'_>, proof: PageProof) -> Result<Counts> {
        let counts = proof
            .check(&self.cursor.root, search)
            .filter(|_| proof.scanned.start == self.cursor.next)
            .ok_or_else(|| {
                Error::new(
                    "CUE_DOMAIN",
                    "bar page lacks a disjoint partition or a durable measured window",
                )
            })?;
        let next = proof.scanned.end;
        let page = json!(Page {
            previous: self.cursor.head.clone(),
            proof
        });
        let key = canonical_hash(&page)?.digest;
        search
            .cache
            .write_immutable(search.store, PAGES, &key, &page)?;
        let cursor = Cursor {
            root: self.cursor.root.clone(),
            next,
            head: Some(key),
        };
        search
            .cache
            .write(search.store, CURSORS, &cursor.root.domain_id, &cursor)?;
        self.cursor = cursor;
        self.counts.accumulate(&counts);
        Ok(counts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_chain_requires_contiguous_cells_and_durable_measurement_on_resume() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::super::tests::fixture(&store, &policy, &[360; 3]);
        let search = &mut r.search;
        let cancel = Cancellation::default();
        let root = BarCueRange {
            domain_id: canonical_hash(&"fixture bar domain").unwrap().digest,
            start: 0,
            end: 4,
        };
        let key = canonical_hash(&"fixture timing window").unwrap().digest;
        let mut coverage = BarCoverage::restore(search, root.clone(), &cancel).unwrap();
        let span = |start, end, outcome: &str| Span {
            start,
            end,
            outcome: outcome.into(),
        };
        let first = || PageProof {
            scanned: BarCueRange {
                end: 3,
                ..root.clone()
            },
            outcomes: vec![
                span(0, 2, "row_timing_rejected"),
                span(2, 3, "timing_window"),
            ],
            windows: vec![key.clone()],
        };
        assert!(coverage.commit(search, first()).is_err());
        assert_eq!(coverage.remainder().start, 0);
        search
            .cache
            .write(&store, "windows", &key, &Measurement::default())
            .unwrap();
        search.load_known(&cancel).unwrap();
        let mut with_gap = first();
        with_gap.outcomes[0].end = 1;
        assert!(coverage.commit(search, with_gap).is_err());
        let committed = coverage.commit(search, first()).unwrap();
        assert_eq!(committed.unknown, 1);
        assert_eq!(committed.measured, 0);
        assert_eq!(committed.block_rejected, 2);
        assert_eq!(committed.row_rejected, 2);
        let mut resumed = BarCoverage::restore(search, root.clone(), &cancel).unwrap();
        assert_eq!(resumed.remainder().start, 3);
        resumed
            .commit(
                search,
                PageProof {
                    scanned: BarCueRange {
                        start: 3,
                        ..root.clone()
                    },
                    outcomes: vec![span(3, 4, "cell_timing_rejected")],
                    windows: vec![],
                },
            )
            .unwrap();
        let finished = BarCoverage::restore(search, root.clone(), &cancel).unwrap();
        assert!(finished.finished());
        assert_eq!(finished.counts.row_rejected, 2);
        assert_eq!(finished.counts.total(), 4);
        assert_eq!(finished.counts.block_rejected, 2);
        assert_eq!(finished.counts.cell_rejected, 1);
        assert_eq!(finished.counts.unknown, 1);
        assert_eq!(finished.counts.selector_partitioned, 0);

        let legacy = Measurement {
            evidence: vec![json!({"ranked": [], "rejected": []})],
            ..Measurement::default()
        };
        search
            .cache
            .write(&store, "windows", &key, &legacy)
            .unwrap();
        search.load_known(&cancel).unwrap();
        assert!(search.measured_keys.contains(&key));
        assert!(!BarCoverage::restore(search, root.clone(), &cancel)
            .unwrap()
            .finished());

        let variants = mixlib_planner::selector_realization_domain()
            .map(|(tactic_id, variant)| {
                json!({"tactic_id": tactic_id, "variant": variant,
                "decision": "eligibility_unresolved"})
            })
            .collect::<Vec<_>>();
        let review = Measurement {
            sources: search
                .cache
                .sources
                .iter()
                .take(2)
                .map(|(id, pin)| (id.clone(), pin.clone()))
                .collect(),
            evidence: vec![json!({"ranked": [], "rejected": [], "selector_review": {
                "profile": mixlib_planner::SELECTOR_REALIZATION_PROFILE,
                "rhythmic_exit_frames": [],
                "swift_exit_frames": [],
                "swift_scan_complete": true,
                "rhythmic_scan_complete": true,
                "variants": variants,
            }})],
            ..Measurement::default()
        };
        search.continuations = Some(Default::default());
        search
            .cache
            .write(&store, "selector-refreshes", &key, &review)
            .unwrap();
        search.load_known(&cancel).unwrap();
        let reviewed = BarCoverage::restore(search, root.clone(), &cancel).unwrap();
        assert!(reviewed.finished());
        assert_eq!(reviewed.counts.selector_partitioned, 1);
        assert_eq!(reviewed.counts.selector_resolved, 0);
        assert_eq!(reviewed.counts.total(), 4);
        assert!(!reviewed.proven_empty());

        let mut closed = review;
        for row in closed.evidence[0]["selector_review"]["variants"]
            .as_array_mut()
            .unwrap()
        {
            row["decision"] = if row["variant"] == "standard" {
                json!("incompatible_timing_basis")
            } else {
                json!("no_detected_exit")
            };
        }
        search
            .cache
            .write(&store, "selector-refreshes", &key, &closed)
            .unwrap();
        search.load_known(&cancel).unwrap();
        let exhausted = BarCoverage::restore(search, root.clone(), &cancel).unwrap();
        assert!(exhausted.proven_empty());
        assert_eq!(exhausted.counts.selector_empty, 1);

        search
            .cache
            .write(&store, "selector-refreshes", &key, &json!("damaged"))
            .unwrap();
        search.measured_keys.clear();
        search.negative_windows.clear();
        search.oracle = Default::default();
        search.load_known(&cancel).unwrap();
        let reopened = BarCoverage::restore(search, root.clone(), &cancel).unwrap();
        assert!(search.measured_keys.contains(&key));
        assert_eq!(reopened.remainder().start, 0);
        assert!(!reopened.finished());

        search
            .cache
            .write(&store, "windows", &key, &Measurement::default())
            .unwrap();
        search.load_known(&cancel).unwrap();
        assert!(BarCoverage::restore(search, root.clone(), &cancel)
            .unwrap()
            .finished());
        let head = finished.cursor.head.unwrap();
        search
            .cache
            .write(&store, PAGES, &head, &json!("damaged"))
            .unwrap();
        assert_eq!(
            BarCoverage::restore(search, root, &cancel)
                .unwrap()
                .remainder()
                .start,
            0
        );
    }
}
