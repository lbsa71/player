//! Pending tactic endpoint proposals are recoverable from their parent
//! measurement records. Only a child's own measurement resolves its window.
use super::*;
use std::collections::VecDeque;

pub(super) const ENDPOINT_PROFILE: &str = "detected-endpoint-extensions/1";
pub(super) const MAX_ENDPOINT_PULSES: usize = 256;
const ENDPOINT_CACHE: &str = "cp-endpoint-expansions";

#[derive(Serialize, Deserialize)]
struct EndpointFamily {
    tracks: [String; 2],
    pins: [String; 2],
    cue: [u64; 4],
    context: usize,
    policy: String,
}

impl EndpointFamily {
    fn key(&self) -> Result<String> {
        Ok(canonical_hash(&(ENDPOINT_PROFILE, self))?.digest)
    }

    fn sources(&self) -> BTreeMap<String, String> {
        self.tracks
            .iter()
            .cloned()
            .zip(self.pins.iter().cloned())
            .collect()
    }
}

#[derive(Serialize, Deserialize)]
struct EndpointExpansion {
    family: EndpointFamily,
    windows: Vec<MixPlan>,
}

#[derive(Default)]
pub(super) struct Frontier {
    pending: VecDeque<(String, MixPlan)>,
    seen: BTreeSet<String>,
    restored: usize,
    discovered: usize,
    scheduled: usize,
    stale_or_invalid: usize,
    prefer_ranked: bool,
    expanded: BTreeMap<String, usize>,
    restored_families: usize,
    #[cfg(any(feature = "cp-search", test))]
    unknown: BTreeMap<String, String>,
}

impl Frontier {
    pub(super) fn insert(&mut self, key: String, plan: MixPlan, restored: bool, prioritized: bool) {
        if self.seen.insert(key.clone()) {
            if prioritized {
                self.pending.push_front((key, plan));
            } else {
                self.pending.push_back((key, plan));
            }
            if !restored {
                self.discovered += 1;
            }
        } else if prioritized {
            if let Some(index) = self.pending.iter().position(|(old, _)| *old == key) {
                let existing = self.pending.remove(index).unwrap();
                self.pending.push_front(existing);
            }
        }
    }

    pub(super) fn invalid(&mut self) {
        self.stale_or_invalid += 1;
    }

    pub(super) fn finish_restore(&mut self, measured: &BTreeSet<String>) {
        self.pending.retain(|(key, _)| !measured.contains(key));
        self.restored = self.pending.len();
    }

    pub(super) fn take_quality_seed(
        &mut self,
        ranked: &mut VecDeque<MixPlan>,
        measured: &BTreeSet<String>,
        scheduled: &BTreeSet<String>,
    ) -> Option<MixPlan> {
        if self.prefer_ranked {
            if let Some(seed) = ranked.pop_front() {
                self.prefer_ranked = false;
                return Some(seed);
            }
        }
        while let Some((key, seed)) = self.pending.pop_front() {
            if !measured.contains(&key) && !scheduled.contains(&key) {
                self.scheduled += 1;
                self.prefer_ranked = true;
                return Some(seed);
            }
        }
        ranked.pop_front()
    }

    #[cfg(any(feature = "cp-search", test))]
    pub(super) fn evidence(&self, measured: &BTreeSet<String>) -> Value {
        json!({
            "restored": self.restored,
            "discovered": self.discovered,
            "scheduled": self.scheduled,
            "pending": self.pending.iter().filter(|(key, _)| !measured.contains(key)).count(),
            "stale_or_invalid": self.stale_or_invalid,
            "endpoint_expansion": {
                "profile": ENDPOINT_PROFILE,
                "restored_families": self.restored_families,
                "enumerated_families": self.expanded.len(),
                "timing_windows_generated": self.expanded.values().sum::<usize>(),
                "unknown_families": self.unknown.len(),
                "unknown_reasons": self.unknown.values().fold(BTreeMap::new(), |mut counts, reason| {
                    *counts.entry(reason).or_insert(0_usize) += 1;
                    counts
                }),
            },
            "scope": "Discovered continuation windows only; ungenerated endpoints and other tactic variants remain unresolved",
        })
    }
}

impl Search<'_> {
    pub(super) fn restore_endpoint_expansions(&mut self, cancel: &Cancellation) -> Result<()> {
        if self.continuations.is_none() {
            return Ok(());
        }
        let policy = crate::search_cache::policy_key(self.policy)?;
        for key in self.cache.keys(self.store, ENDPOINT_CACHE)? {
            cancel.check()?;
            if !self.budget_open() {
                break;
            }
            let Some(expansion) =
                self.cache
                    .read::<EndpointExpansion>(self.store, ENDPOINT_CACHE, &key)?
            else {
                continue;
            };
            let sources = expansion.family.sources();
            if expansion.family.key()? != key
                || expansion.family.policy != policy
                || expansion.family.context != self.context
                || sources
                    .iter()
                    .any(|(id, pin)| self.cache.sources.get(id) != Some(pin))
                || expansion.windows.len() > MAX_ENDPOINT_PULSES
                || !expansion.windows.iter().all(|plan| {
                    plan.placements
                        .iter()
                        .map(|p| &p.track_id)
                        .eq(expansion.family.tracks.iter())
                        && plan.validate().is_ok()
                        && plan.placements.iter().all(|placement| {
                            self.cached_placement_track(placement, &sources).is_some()
                        })
                        && crate::tactics::cue_key(plan)[..2] == expansion.family.cue[..2]
                })
            {
                continue;
            }
            self.record_endpoint_expansion(key, expansion, true)?;
        }
        Ok(())
    }

    #[cfg(feature = "cp-search")]
    pub(super) fn expand_anchor_endpoints(
        &mut self,
        anchor: &MixPlan,
        cancel: &Cancellation,
    ) -> Result<()> {
        let Some(frontier) = &self.continuations else {
            return Ok(());
        };
        let [a, b] = anchor.placements.as_slice() else {
            return Ok(());
        };
        let family = EndpointFamily {
            tracks: [a.track_id.clone(), b.track_id.clone()],
            pins: [
                self.cache.sources[&a.track_id].clone(),
                self.cache.sources[&b.track_id].clone(),
            ],
            cue: crate::tactics::cue_key(anchor),
            context: self.context,
            policy: crate::search_cache::policy_key(self.policy)?,
        };
        let key = family.key()?;
        if frontier.expanded.contains_key(&key) || frontier.unknown.contains_key(&key) {
            return Ok(());
        }
        let build = || {
            for placement in &anchor.placements {
                cancel.check()?;
                require(
                    mixlib_planner::load_tactic_track_analysis(&self.snapshot, placement)?
                        .is_some(),
                    "ENDPOINT_ANALYSIS_MISSING",
                    "pinned endpoint analysis is unavailable",
                )?;
            }
            let request = StitchRequest {
                from: &a.track_id,
                to: &b.track_id,
                overlap_beats: 8,
                context_beats: self.context,
                outgoing_after_frame: 0,
            };
            // There are at most 256 distinct outgoing pulse lengths.
            // The planner checks each detected candidate and stops only
            // after emitting this many valid timing windows.
            mixlib_planner::tactic_continuations(
                &self.snapshot,
                &request,
                self.policy,
                anchor,
                MAX_ENDPOINT_PULSES,
                cancel,
            )
        };
        let windows = match build() {
            Ok(windows) => windows,
            Err(error) => {
                cancel.check()?;
                self.continuations
                    .as_mut()
                    .unwrap()
                    .unknown
                    .insert(key, error.code);
                return Ok(());
            }
        };
        let expansion = EndpointExpansion { family, windows };
        cancel.check()?;
        self.cache
            .write(self.store, ENDPOINT_CACHE, &key, &expansion)?;
        self.record_endpoint_expansion(key, expansion, false)
    }

    fn record_endpoint_expansion(
        &mut self,
        key: String,
        mut expansion: EndpointExpansion,
        restored: bool,
    ) -> Result<()> {
        let count = expansion.windows.len();
        // Front insertion in reverse keeps the planner's short/long ordering.
        expansion.windows.reverse();
        self.queue_continuations(
            &expansion.family.sources(),
            expansion.windows,
            restored,
            true,
        )?;
        let frontier = self.continuations.as_mut().unwrap();
        frontier.expanded.insert(key, count);
        frontier.restored_families += usize::from(restored);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_family_identity_changes_with_cues_or_source_pins() {
        let mut family = EndpointFamily {
            tracks: ["a".into(), "b".into()],
            pins: ["analysis-1".into(), "analysis-2".into()],
            cue: [10, 20, 30, 40],
            context: 12,
            policy: "timing".into(),
        };
        let original = family.key().unwrap();
        family.cue[3] += 1;
        let different_pulse_span = family.key().unwrap();
        assert_ne!(original, different_pulse_span);
        family.pins[0] = "analysis-3".into();
        assert_ne!(different_pulse_span, family.key().unwrap());
    }

    #[test]
    fn enumerated_anchor_promotes_an_existing_unmeasured_window_once() {
        let plan: MixPlan =
            serde_json::from_str(include_str!("../../../examples/mix-plan.json")).unwrap();
        let mut ordinary = plan.clone();
        ordinary.plan_id = id();
        let mut frontier = Frontier::default();
        frontier.insert("ordinary".into(), ordinary.clone(), true, false);
        frontier.insert("anchor".into(), plan.clone(), true, false);
        frontier.insert("anchor".into(), plan.clone(), false, true);
        let mut ranked = VecDeque::new();
        let measured = BTreeSet::new();
        let scheduled = BTreeSet::new();
        for expected in [plan.plan_id, ordinary.plan_id] {
            let next = frontier
                .take_quality_seed(&mut ranked, &measured, &scheduled)
                .unwrap();
            assert_eq!(next.plan_id, expected);
        }
        assert!(frontier
            .take_quality_seed(&mut ranked, &measured, &scheduled)
            .is_none());
    }
}
