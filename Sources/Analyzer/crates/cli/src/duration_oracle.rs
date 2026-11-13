//! Interpret cached window observations without learning a rejection from
//! missing evidence. Positive realized choices remain independently usable.
use super::*;
use mixlib_planner::{
    selector_realization_domain, RealizationDecision, SelectorVariant, TacticRejection,
    SELECTOR_REALIZATION_PROFILE,
};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Evidence {
    Reviewed,
    Unknown(String),
}

#[derive(Debug, PartialEq, Eq)]
struct Observation {
    evidence: Evidence,
    verified_choices: usize,
    continuation_windows: usize,
    realizations: Option<Vec<RealizationObservation>>,
    legacy_review: bool,
    refreshed: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct RealizationObservation {
    tactic: TacticId,
    variant: SelectorVariant,
    outcome: &'static str,
    unresolved: bool,
    eligibility_reasons: BTreeSet<String>,
}

fn legacy_selector_review(review: &Value) -> bool {
    matches!(
        review["profile"].as_str(),
        Some(
            "pinned-window-selector/1"
                | "pinned-window-selector/2"
                | "pinned-window-selector/3"
                | "pinned-window-selector/4"
                | "pinned-window-selector/5"
                | "pinned-window-selector/6"
        )
    )
}

fn selector_review(
    measurement: &Measurement,
) -> std::result::Result<Option<Vec<RealizationObservation>>, &'static str> {
    let [diagnostic] = measurement.evidence.as_slice() else {
        return Ok(None);
    };
    let Some(value) = diagnostic.get("selector_review") else {
        return Ok(None);
    };
    if legacy_selector_review(value) {
        return Ok(None);
    }
    if !value["rhythmic_exit_frames"].is_array() || !value["swift_exit_frames"].is_array() {
        return Err("invalid_selector_review");
    }
    let review: crate::tactics::SelectorReview =
        serde_json::from_value(value.clone()).map_err(|_| "invalid_selector_review")?;
    if review.profile != SELECTOR_REALIZATION_PROFILE {
        return Err("unsupported_selector_review");
    }
    if [&review.rhythmic_exit_frames, &review.swift_exit_frames]
        .into_iter()
        .any(|frames| frames.windows(2).any(|w| w[0] >= w[1]))
    {
        return Err("invalid_selector_partition");
    }
    let mut expected = BTreeSet::new();
    for (tactic, variant) in selector_realization_domain() {
        let (frames, complete) = match variant {
            SelectorVariant::Standard => (&[][..], true),
            SelectorVariant::Swift => (
                review.swift_exit_frames.as_slice(),
                review.swift_scan_complete,
            ),
            SelectorVariant::Rhythmic => (
                review.rhythmic_exit_frames.as_slice(),
                review.rhythmic_scan_complete,
            ),
        };
        expected.extend(frames.iter().map(|&frame| (tactic, variant, Some(frame))));
        if frames.is_empty() || !complete {
            expected.insert((tactic, variant, None));
        }
    }
    let mut schedules = BTreeMap::new();
    for edge in &measurement.edges {
        let plans = read_tactics(&edge.plan).map_err(|_| "invalid_selector_schedule")?;
        let [tactic] = plans.as_slice() else {
            return Err("invalid_selector_schedule");
        };
        let hash = canonical_hash(tactic)
            .map_err(|_| "invalid_selector_schedule")?
            .digest;
        if schedules
            .insert(
                (tactic.tactic_id, hash),
                (
                    edge.last_resort,
                    tactic.phrase_exit.as_ref().map(|exit| {
                        (
                            match exit.kind {
                                PhraseExitKind::RhythmicHandoff => SelectorVariant::Rhythmic,
                                PhraseExitKind::AccompanimentPause
                                | PhraseExitKind::CrescendoRelease => SelectorVariant::Swift,
                            },
                            exit.start_frame,
                        )
                    }),
                ),
            )
            .is_some()
        {
            return Err("ambiguous_selector_schedule");
        }
    }
    // Diagnostics explain unresolved rows. They never certify a rejection.
    let rejected = diagnostic["rejected"].as_array().into_iter().flatten();
    let mut reasons = BTreeMap::new();
    for rejection in
        rejected.filter_map(|v| serde_json::from_value::<TacticRejection>(v.clone()).ok())
    {
        reasons
            .entry((
                rejection.tactic_id,
                rejection.variant,
                rejection.exit_start_frame,
            ))
            .or_insert_with(BTreeSet::new)
            .extend(rejection.reasons);
    }
    let mut observations = Vec::with_capacity(expected.len());
    for row in review.variants {
        let tactic = row.realization.tactic_id;
        let variant = row.realization.variant;
        let exit = row.realization.exit_start_frame;
        if !expected.remove(&(tactic, variant, exit)) {
            return Err("invalid_selector_partition");
        }
        use RealizationDecision::*;
        let (outcome, unresolved) = match (row.realization.decision, row.schedule_hash) {
            (Scheduled, Some(hash)) => match schedules.remove(&(tactic, hash)) {
                Some((_, actual)) if exit.map(|frame| (variant, frame)) != actual => {
                    return Err("invalid_selector_schedule")
                }
                Some((false, _)) => ("verified", false),
                Some((true, _)) => ("last_resort", false),
                None => ("schedule_not_recorded", true),
            },
            (DisabledByPolicy, None) => ("disabled_by_policy", false),
            (MissingCapability, None) => ("missing_capability", false),
            (IncompatibleTimingBasis, None) => ("incompatible_timing_basis", false),
            (IncompatibleBarCount, None) => ("incompatible_bar_count", false),
            (IncompatibleFixedClock, None) => ("incompatible_fixed_clock", false),
            (MeasuredPeakExceeded, None) => ("measured_peak_exceeded", false),
            (MeasuredGridMismatch, None) => ("measured_grid_mismatch", false),
            (MeasuredBuildInterrupted, None) => ("measured_build_interrupted", false),
            (MeasuredIsolationFailed, None) => ("measured_isolation_failed", false),
            (MeasuredTonalConflict, None) => ("measured_tonal_conflict", false),
            (MeasuredHoldIncompatible, None) => ("measured_hold_incompatible", false),
            (MeasuredFilterCrowdingInsufficient, None) => {
                ("measured_filter_crowding_insufficient", false)
            }
            (MeasuredLandingMismatch, None) => ("measured_landing_mismatch", false),
            (MeasuredBassInactive, None) => ("measured_bass_inactive", false),
            (MeasuredVocalAbsent, None) => ("measured_vocal_absent", false),
            (MissingExitEvidence, None) => ("missing_exit_evidence", true),
            (NoDetectedExit, None)
                if exit.is_none()
                    && match variant {
                        SelectorVariant::Standard => false,
                        SelectorVariant::Swift => {
                            review.swift_scan_complete && review.swift_exit_frames.is_empty()
                        }
                        SelectorVariant::Rhythmic => {
                            review.rhythmic_scan_complete && review.rhythmic_exit_frames.is_empty()
                        }
                    } =>
            {
                ("no_detected_exit", false)
            }
            (EligibilityUnresolved, None) => ("eligibility_unresolved", true),
            (ScheduleUnresolved, None) => ("schedule_unresolved", true),
            _ => return Err("invalid_selector_review"),
        };
        observations.push(RealizationObservation {
            tactic,
            variant,
            outcome,
            unresolved,
            eligibility_reasons: if row.realization.decision == EligibilityUnresolved {
                reasons.remove(&(tactic, variant, exit)).unwrap_or_default()
            } else {
                BTreeSet::new()
            },
        });
    }
    if !expected.is_empty() || !schedules.is_empty() {
        return Err("invalid_selector_partition");
    }
    Ok(Some(observations))
}

impl Observation {
    fn from_measurement(measurement: &Measurement) -> Self {
        let diagnostic = match measurement.evidence.as_slice() {
            [row] => row["ranked"].as_array().zip(row["rejected"].as_array()),
            _ => None,
        };
        let mut evidence = match diagnostic {
            Some((ranked, _)) if ranked.len() == measurement.edges.len() => Evidence::Reviewed,
            Some(_) => Evidence::Unknown("ranked_realization_not_recorded".into()),
            None => {
                let reason = [
                    "TACTIC_FACTS",
                    "TACTIC_GRID",
                    "TACTIC_BAR_WINDOW",
                    "TACTIC_NATIVE_GRID",
                    "TACTIC_BOUNDARIES",
                ]
                .into_iter()
                .find(|code| measurement.rejected.contains_key(*code))
                .unwrap_or("missing_selection_diagnostic");
                Evidence::Unknown(reason.into())
            }
        };
        let legacy_review = diagnostic.is_some()
            && measurement.evidence[0]
                .get("selector_review")
                .is_none_or(legacy_selector_review);
        let realizations = match selector_review(measurement) {
            Ok(rows) => rows,
            Err(reason) => {
                evidence = Evidence::Unknown(reason.into());
                None
            }
        };
        Self {
            evidence,
            verified_choices: measurement
                .edges
                .iter()
                .filter(|edge| !edge.last_resort)
                .count(),
            continuation_windows: measurement.continuations.len(),
            realizations,
            legacy_review,
            refreshed: false,
        }
    }

    fn reviewed_empty(&self) -> bool {
        self.evidence == Evidence::Reviewed
            && self.verified_choices == 0
            && self.continuation_windows == 0
    }

    #[cfg(any(feature = "cp-search", test))]
    fn selector_resolved(&self) -> Option<bool> {
        let rows = self.realizations.as_ref()?;
        Some(self.evidence == Evidence::Reviewed && rows.iter().all(|row| !row.unresolved))
    }

    #[cfg(any(feature = "cp-search", test))]
    fn selector_empty(&self) -> bool {
        self.verified_choices == 0 && self.selector_resolved() == Some(true)
    }
}

#[derive(Default)]
pub(super) struct Ledger {
    observations: BTreeMap<String, Observation>,
    execution_failures: BTreeMap<String, String>,
}

impl Ledger {
    pub(super) fn observe(&mut self, key: &str, measurement: &Measurement) -> bool {
        self.record(key, measurement, false)
    }

    pub(super) fn observe_refresh(&mut self, key: &str, measurement: &Measurement) -> bool {
        self.record(key, measurement, true)
    }

    fn record(&mut self, key: &str, measurement: &Measurement, refreshed: bool) -> bool {
        let mut observation = Observation::from_measurement(measurement);
        observation.refreshed = refreshed && !observation.legacy_review;
        if observation.refreshed || !observation.legacy_review {
            self.execution_failures.remove(key);
        }
        if let Some(current) = self.observations.get_mut(key) {
            // The refresh has its own selector partition. The original
            // cache record still supplies any independent positive facts.
            if refreshed || current.refreshed {
                let verified = observation.verified_choices.max(current.verified_choices);
                let continuations = observation
                    .continuation_windows
                    .max(current.continuation_windows);
                if refreshed && !(observation.legacy_review && current.realizations.is_some()) {
                    *current = observation;
                }
                current.verified_choices = verified;
                current.continuation_windows = continuations;
            } else {
                *current = observation;
            }
            return current.reviewed_empty();
        }
        let reviewed_empty = observation.reviewed_empty();
        self.observations.insert(key.into(), observation);
        reviewed_empty
    }

    pub(super) fn needs_selector_refresh(&self, key: &str) -> bool {
        self.observations
            .get(key)
            .is_some_and(|row| row.legacy_review && !row.refreshed)
    }

    #[cfg(feature = "cp-search")]
    pub(super) fn selector_refresh_failed(&self, key: &str) -> bool {
        self.needs_selector_refresh(key) && self.execution_failures.contains_key(key)
    }

    pub(super) fn fail(&mut self, key: String, error: Error) {
        self.execution_failures.insert(key, error.code);
    }

    #[cfg(any(feature = "cp-search", test))]
    pub(super) fn is_unknown(&self, key: &str) -> bool {
        self.observations
            .get(key)
            .is_some_and(|observation| matches!(observation.evidence, Evidence::Unknown(_)))
    }

    #[cfg(feature = "cp-search")]
    pub(super) fn selector_resolved(&self, key: &str) -> Option<bool> {
        self.observations.get(key)?.selector_resolved()
    }

    #[cfg(feature = "cp-search")]
    pub(super) fn selector_empty(&self, key: &str) -> bool {
        self.observations
            .get(key)
            .is_some_and(Observation::selector_empty)
    }

    #[cfg(any(feature = "cp-search", test))]
    pub(super) fn evidence(&self) -> Value {
        let mut unknown = BTreeMap::new();
        let mut failures = BTreeMap::new();
        let mut verified = 0;
        let mut refined = 0;
        let mut reviewed_empty = 0;
        let mut reviewed_partitions = 0;
        let mut empty_partitions = 0;
        let mut unresolved_partitions = 0;
        let mut refreshed = 0;
        let mut refreshed_without_partition = 0;
        let mut legacy_reviews_pending = 0;
        let mut realization_outcomes = BTreeMap::new();
        let mut by_tactic = BTreeMap::new();
        let mut unresolved_eligibility_reasons = BTreeMap::new();
        for observation in self.observations.values() {
            empty_partitions += usize::from(observation.selector_empty());
            legacy_reviews_pending +=
                usize::from(observation.legacy_review && !observation.refreshed);
            refreshed += usize::from(observation.refreshed);
            refreshed_without_partition +=
                usize::from(observation.refreshed && observation.realizations.is_none());
            verified += usize::from(observation.verified_choices > 0);
            refined += usize::from(observation.continuation_windows > 0);
            reviewed_empty += usize::from(observation.reviewed_empty());
            if let Evidence::Unknown(reason) = &observation.evidence {
                *unknown.entry(reason).or_insert(0_usize) += 1;
            }
            if let Some(rows) = &observation.realizations {
                reviewed_partitions += 1;
                unresolved_partitions += usize::from(rows.iter().any(|row| row.unresolved));
                for row in rows {
                    for reason in &row.eligibility_reasons {
                        *unresolved_eligibility_reasons
                            .entry(reason)
                            .or_insert(0_usize) += 1;
                    }
                    *realization_outcomes.entry(row.outcome).or_insert(0_usize) += 1;
                    *by_tactic
                        .entry(row.tactic)
                        .or_insert_with(BTreeMap::new)
                        .entry(row.variant)
                        .or_insert_with(BTreeMap::new)
                        .entry(row.outcome)
                        .or_insert(0_usize) += 1;
                }
            }
        }
        for code in self.execution_failures.values() {
            *failures.entry(code).or_insert(0_usize) += 1;
        }
        json!({
            "observed_windows": self.observations.len(),
            "windows_with_verified_choices": verified,
            "windows_with_continuations": refined,
            "reviewed_without_admission_or_continuation": reviewed_empty,
            "unknown_windows": unknown.values().sum::<usize>(),
            "unknown_reasons": unknown,
            "execution_failures": failures,
            "selector_realizations": {
                "profile": SELECTOR_REALIZATION_PROFILE,
                "windows_with_partition": reviewed_partitions,
                "windows_with_proven_empty_partition": empty_partitions,
                "partitions_with_unresolved_realizations": unresolved_partitions,
                "windows_without_partition": self.observations.len() - reviewed_partitions,
                "refreshed_windows": refreshed,
                "refreshed_without_partition": refreshed_without_partition,
                "legacy_reviews_pending": legacy_reviews_pending,
                "outcomes": realization_outcomes,
                "unresolved_eligibility_reasons": unresolved_eligibility_reasons,
                "by_tactic_and_variant": by_tactic,
                "scope": "Deterministic selector modes for exact observed windows, with other endpoints and timing choices unresolved",
            },
            "scope": "Exact window observations; review without admission does not close other realizations",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires the Analyzer library and pinned cached pair windows"]
    fn cached_windows_expose_all_detected_rhythmic_exits() {
        audit_cached_exits(SelectorVariant::Rhythmic, &[TacticId::M01]);
    }

    #[test]
    #[ignore = "requires the Analyzer library and pinned cached pair windows"]
    fn cached_windows_expose_all_detected_accompaniment_exits() {
        audit_cached_exits(
            SelectorVariant::Swift,
            &[TacticId::M01, TacticId::M02, TacticId::S05, TacticId::S06],
        );
    }

    fn pinned_audit_window(store: &Store, file: &std::path::Path) -> Measurement {
        let relative = file.strip_prefix(&store.root).unwrap().to_str().unwrap();
        let report: Report = store.read(relative).unwrap();
        let payload = &report.header.extensions["payload"];
        assert_eq!(
            report.header.extensions["digest"],
            json!(canonical_hash(payload).unwrap())
        );
        let measurement: Measurement = serde_json::from_value(payload.clone()).unwrap();
        let plan = &measurement.edges.first().unwrap().plan;
        let tracks = plan
            .placements
            .iter()
            .map(|p| store.track(&p.track_id).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            measurement.sources,
            Cache::open(&plan.policy, &tracks).unwrap().sources
        );
        measurement
    }

    #[test]
    #[ignore = "requires the Analyzer library and pinned cached pair windows with admitted edges"]
    fn cached_admission_survives_geometry_preflight() {
        let library = std::env::var_os("MIXLIB_AUDIT_LIBRARY").unwrap();
        let files = std::env::var_os("MIXLIB_AUDIT_WINDOWS").unwrap();
        let store = Store::open(std::path::Path::new(&library)).unwrap();
        let snapshot = store.read_snapshot();
        let cancel = Cancellation::default();
        let mut cache = CandidateEvidenceCache::new();
        let mut checked = 0;
        for file in std::env::split_paths(&files) {
            let window = pinned_audit_window(&snapshot, &file);
            let seed = &window.edges[0].plan;
            let mut policy = crate::tactics::policy(
                seed.policy
                    .header
                    .extensions
                    .get("allow_expressive_tactics")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            );
            policy.normal_speed_deviation = seed.policy.max_speed_deviation;
            let facts = cache.candidate_facts(&snapshot, seed, 0, &cancel).unwrap();
            let selection = mixlib_planner::select_tactics(&facts, &policy).unwrap();
            assert_eq!(window.edges.len(), selection.candidates.len(), "{file:?}");
            for (before, after) in window.edges.iter().zip(&selection.candidates) {
                let saved = read_tactics(&before.plan).unwrap();
                assert_eq!(
                    canonical_hash(&saved[0]).unwrap(),
                    canonical_hash(&after.plan).unwrap(),
                    "{file:?}"
                );
                assert_eq!(before.last_resort, after.last_resort, "{file:?}");
                assert!((before.score - after.score).abs() < 1e-10, "{file:?}");
            }
            let profiles_skipped = facts
                .pair
                .profiles
                .values()
                .filter(|p| p.preflight_rejection.is_some())
                .count();
            eprintln!(
                "{}",
                json!({"window": file.file_stem().unwrap().to_str(), "verified_schedules": selection.candidates.len(), "profiles_skipped": profiles_skipped, "cut_band_pcm_checks": facts.pair.cut_band_pcm_checks})
            );
            checked += 1;
        }
        snapshot.verify_read_snapshot(&cancel).unwrap();
        assert!(checked > 0);
    }

    fn audit_cached_exits(variant: SelectorVariant, tactics: &[TacticId]) {
        let library = std::env::var_os("MIXLIB_AUDIT_LIBRARY").unwrap();
        let files = std::env::var_os("MIXLIB_AUDIT_WINDOWS").unwrap();
        let store = Store::open(std::path::Path::new(&library)).unwrap();
        let snapshot = store.read_snapshot();
        let cancel = Cancellation::default();
        let mut cache = CandidateEvidenceCache::new();
        let mut multi = 0;
        for file in std::env::split_paths(&files) {
            let measurement = pinned_audit_window(&snapshot, &file);
            let plan = &measurement.edges.first().unwrap().plan;
            let facts = cache.candidate_facts(&snapshot, plan, 0, &cancel).unwrap();
            let mut policy = crate::tactics::policy(false);
            policy.normal_speed_deviation = plan.policy.max_speed_deviation;
            let selection = mixlib_planner::select_tactics(&facts, &policy).unwrap();
            let frames = match variant {
                SelectorVariant::Swift => facts.swift_exit_frames().collect::<Vec<_>>(),
                SelectorVariant::Rhythmic => facts.pair.rhythmic_exit_frames().collect(),
                SelectorVariant::Standard => unreachable!(),
            };
            let decisions = selection
                .realizations
                .iter()
                .filter(|row| row.variant == variant)
                .collect::<Vec<_>>();
            if !frames.is_empty() {
                for tactic in tactics {
                    assert_eq!(
                        decisions
                            .iter()
                            .filter(|row| row.tactic_id == *tactic)
                            .filter_map(|row| row.exit_start_frame)
                            .collect::<Vec<_>>(),
                        frames
                    );
                }
            }
            multi += usize::from(frames.len() > 1);
            let rejected = selection
                .rejected
                .iter()
                .filter(|row| row.variant == variant)
                .collect::<Vec<_>>();
            eprintln!(
                "{}",
                json!({"window": file.file_stem().unwrap().to_str(), "variant": variant, "frames": frames, "decisions": decisions, "rejections": rejected})
            );
        }
        snapshot.verify_read_snapshot(&cancel).unwrap();
        assert!(
            multi > 0,
            "audit fixture requires at least one window with multiple detected exits"
        );
    }

    fn exact_review() -> Measurement {
        let plan = super::super::tests::tactic_plan();
        let hash = canonical_hash(&read_tactics(&plan).unwrap()[0])
            .unwrap()
            .digest;
        let variants = selector_realization_domain().map(|(tactic, variant)| {
            if tactic == TacticId::S13 {
                json!({"tactic_id": tactic, "variant": variant, "decision": "scheduled", "schedule_hash": hash})
            } else {
                json!({"tactic_id": tactic, "variant": variant, "decision": "eligibility_unresolved"})
            }
        }).collect::<Vec<_>>();
        Measurement {
            edges: vec![MeasuredEdge {
                plan,
                exit: 0,
                entry_end: 0,
                score: 5.,
                last_resort: false,
                dense_quality_seconds: 0.,
                handoff_quality_seconds: 0.,
            }],
            evidence: vec![json!({"ranked": [{}], "rejected": [], "selector_review": {
                "profile": SELECTOR_REALIZATION_PROFILE, "rhythmic_exit_frames": [], "swift_exit_frames": [],
                "swift_scan_complete": true, "rhythmic_scan_complete": true, "variants": variants,
            }})],
            ..Default::default()
        }
    }

    #[test]
    fn eligibility_diagnostics_only_count_the_matching_unresolved_realization() {
        let mut window = exact_review();
        window.evidence[0]["rejected"] = json!([
            {"tactic_id":"M01", "variant":"standard", "reasons":["missing headroom", "missing headroom"]},
            {"tactic_id":"M01", "variant":"swift", "exit_start_frame":42, "reasons":["different frame"]},
            {"tactic_id":"S13", "variant":"standard", "reasons":["scheduled"]}
        ]);
        let mut ledger = Ledger::default();
        ledger.observe("sample", &window);
        let report = &ledger.evidence()["selector_realizations"];
        assert_eq!(
            report["unresolved_eligibility_reasons"],
            json!({"missing headroom": 1})
        );
        assert_eq!(report["partitions_with_unresolved_realizations"], 1);
    }

    #[test]
    fn an_absence_claim_requires_a_complete_detector_and_no_detected_frames() {
        let mut window = exact_review();
        let review = &mut window.evidence[0]["selector_review"];
        let rhythmic = review["variants"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|row| row["variant"] == "rhythmic")
            .unwrap();
        rhythmic["decision"] = json!("no_detected_exit");
        assert!(selector_review(&window).unwrap().is_some());
        window.evidence[0]["selector_review"]["rhythmic_scan_complete"] = json!(false);
        assert_eq!(
            selector_review(&window).unwrap_err(),
            "invalid_selector_review"
        );

        let review = &mut window.evidence[0]["selector_review"];
        review["rhythmic_exit_frames"] = json!([3500]);
        let variants = review["variants"].as_array_mut().unwrap();
        variants
            .iter_mut()
            .find(|row| row["variant"] == "rhythmic")
            .unwrap()["decision"] = json!("missing_exit_evidence");
        variants.push(json!({"tactic_id":"M01", "variant":"rhythmic", "exit_start_frame":3500, "decision":"measured_hold_incompatible"}));
        assert!(selector_review(&window).unwrap().is_some());
        window.evidence[0]["selector_review"]["variants"]
            .as_array_mut()
            .unwrap()
            .retain(|row| row["variant"] != "rhythmic" || row["exit_start_frame"].is_u64());
        assert_eq!(
            selector_review(&window).unwrap_err(),
            "invalid_selector_partition"
        );
    }

    #[test]
    fn each_accompaniment_exit_needs_one_row_per_sustained_tactic() {
        let mut window = exact_review();
        let review = &mut window.evidence[0]["selector_review"];
        review["swift_exit_frames"] = json!([3500, 3520]);
        let variants = review["variants"].as_array_mut().unwrap();
        variants.retain(|row| row["variant"] != "swift");
        for id in ["M01", "M02", "S05", "S06"] {
            for frame in [3500, 3520] {
                variants.push(json!({"tactic_id": id, "variant": "swift", "exit_start_frame": frame, "decision": "eligibility_unresolved"}));
            }
        }
        assert_eq!(selector_review(&window).unwrap().unwrap().len(), 46);
        window.evidence[0]["selector_review"]["variants"]
            .as_array_mut()
            .unwrap()
            .pop();
        assert_eq!(
            selector_review(&window).unwrap_err(),
            "invalid_selector_partition"
        );
    }

    #[test]
    fn detected_rhythmic_exit_frames_require_distinct_coverage_rows() {
        let mut window = exact_review();
        let review = &mut window.evidence[0]["selector_review"];
        review["rhythmic_exit_frames"] = json!([3500, 3750]);
        let variants = review["variants"].as_array_mut().unwrap();
        variants.retain(|row| row["variant"] != "rhythmic");
        for frame in [3500, 3750] {
            variants.push(json!({"tactic_id": "M01", "variant": "rhythmic", "exit_start_frame": frame, "decision": "eligibility_unresolved"}));
        }
        assert_eq!(selector_review(&window).unwrap().unwrap().len(), 43);
        window.evidence[0]["selector_review"]["variants"]
            .as_array_mut()
            .unwrap()
            .pop();
        assert_eq!(
            selector_review(&window).unwrap_err(),
            "invalid_selector_partition"
        );

        // Receipts from the previous selector preserve admissions and
        // explicitly remain eligible for refresh, including old overlays.
        let mut old = exact_review();
        old.evidence[0]["selector_review"]["profile"] = json!("pinned-window-selector/2");
        let mut ledger = Ledger::default();
        ledger.observe_refresh("legacy", &old);
        assert!(ledger.needs_selector_refresh("legacy"));
        assert_eq!(ledger.evidence()["windows_with_verified_choices"], 1);
        ledger.observe_refresh("legacy", &exact_review());
        assert!(!ledger.needs_selector_refresh("legacy"));
    }

    #[test]
    fn cached_selector_partition_verifies_the_schedule_by_hash() {
        let mut ledger = Ledger::default();
        let window = exact_review();
        let serialized = serde_json::to_vec(&window).unwrap();
        let mut window: Measurement = serde_json::from_slice(&serialized).unwrap();
        assert!(!ledger.observe("matching", &window));
        let evidence = ledger.evidence();
        assert_eq!(
            evidence["selector_realizations"]["by_tactic_and_variant"]["S13"]["standard"]
                ["verified"],
            1
        );
        assert_eq!(
            evidence["selector_realizations"]["partitions_with_unresolved_realizations"],
            1
        );

        let mut schedule = read_tactics(&window.edges[0].plan).unwrap();
        schedule[0].selection_reason.push('!');
        write_tactics(&mut window.edges[0].plan, schedule).unwrap();
        assert!(!ledger.observe("wrong_schedule", &window));
        assert!(ledger.is_unknown("wrong_schedule"));
        assert_eq!(
            ledger.evidence()["unknown_reasons"]["invalid_selector_partition"],
            1
        );

        window.edges.clear();
        assert!(!ledger.observe("missing_schedule", &window));
        assert!(ledger.is_unknown("missing_schedule"));
        assert_eq!(
            ledger.evidence()["selector_realizations"]["outcomes"]["schedule_not_recorded"],
            1
        );

        window.evidence[0]["selector_review"]["variants"]
            .as_array_mut()
            .unwrap()
            .pop();
        assert!(!ledger.observe("missing_variant", &window));
        assert!(ledger.is_unknown("missing_variant"));
        assert_eq!(
            ledger.evidence()["selector_realizations"]["windows_with_partition"],
            2
        );
    }

    #[test]
    fn a_missing_ranked_realization_does_not_create_an_empty_review() {
        let mut ledger = Ledger::default();
        let window = Measurement {
            evidence: vec![json!({"ranked": [{"tactic_id": "M01"}], "rejected": []})],
            ..Measurement::default()
        };
        assert!(!ledger.observe("missing", &window));
        assert!(ledger.is_unknown("missing"));
        assert_eq!(
            ledger.evidence()["unknown_reasons"]["ranked_realization_not_recorded"],
            1
        );
        assert_eq!(
            ledger.evidence()["reviewed_without_admission_or_continuation"],
            0
        );
    }
}
