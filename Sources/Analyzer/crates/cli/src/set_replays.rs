//! Select bounded, measured repeats without changing admitted joins or song order.
use super::*;
use std::{cell::RefCell, collections::BTreeMap};

#[derive(Default)]
pub(super) struct ReplayCache(RefCell<BTreeMap<String, Vec<mixlib_planner::MeasuredReplay>>>);
impl ReplayCache {
    fn placement_options(
        &self,
        store: &Store,
        plan: &MixPlan,
        index: usize,
        ranked: bool,
        cancel: &Cancellation,
    ) -> Result<Vec<(PassageReplay, f64)>> {
        let base = plan.total_output_frames;
        let p = &plan.placements[index];
        cancel.check()?;
        let key = canonical_hash(&serde_json::json!([
            p.track_id,
            p.audio_hash,
            p.analysis,
            p.override_revision
        ]))?
        .digest;
        if !self.0.borrow().contains_key(&key) {
            let measured = mixlib_planner::measure_set_replays(store, p, cancel)?;
            self.0.borrow_mut().insert(key.clone(), measured);
        }
        let cache = self.0.borrow();
        let mut options = vec![];
        let mut regions = std::collections::BTreeSet::new();
        for c in &cache[&key] {
            let Some(a) = output_at(&p.time_map, c.start) else {
                continue;
            };
            let Some(b) = output_at(&p.time_map, c.end) else {
                continue;
            };
            let Some(at) = output_at(&p.time_map, c.destination) else {
                continue;
            };
            let replay = PassageReplay {
                placement: index,
                capture_start: a,
                capture_end: b,
                insert_before: at,
                repetitions: 1,
                bars: c.bars,
                declick_frames: u64::from(plan.output.sample_rate_hz) / 200,
                evidence: c.evidence.clone(),
            };
            let arrangement = SetArrangement {
                version: 1,
                base_output_frames: base,
                replays: vec![replay.clone()],
            };
            let mut candidate = plan.clone();
            candidate.total_output_frames = base + arrangement.added_frames();
            candidate.renderer_version = ARRANGEMENT_RENDERER.into();
            if arrangement.validate_for(&candidate).is_err() {
                continue;
            }
            if ranked && !regions.insert((c.bars, c.destination != c.end)) {
                continue;
            }
            options.push((replay, c.quality));
            if ranked && options.len() >= 6 {
                break;
            }
        }
        Ok(options)
    }

    #[cfg(feature = "cp-search")]
    pub(super) fn advance_preparation(
        &self,
        store: &Store,
        plan: &MixPlan,
        prep: &mut ReplayPreparation,
        cancel: &Cancellation,
    ) -> Result<Option<ReplayDomain>> {
        if prep.next < plan.placements.len() {
            let options = self.placement_options(store, plan, prep.next, false, cancel)?;
            prep.next += 1;
            if !options.is_empty() {
                let choices = std::iter::once(None)
                    .chain(options.into_iter().flat_map(|(replay, _)| {
                        (1..=3).map(move |repetitions| {
                            Some(PassageReplay {
                                repetitions,
                                ..replay.clone()
                            })
                        })
                    }))
                    .collect();
                prep.options.push(choices);
            }
        }
        Ok((prep.next == plan.placements.len()).then(|| prep.finish()))
    }

    pub fn arrange(
        &self,
        store: &Store,
        plan: &MixPlan,
        minimum: f64,
        maximum: f64,
        cancel: &Cancellation,
    ) -> Result<Option<MixPlan>> {
        let rate = f64::from(plan.output.sample_rate_hz);
        let base = plan.total_output_frames;
        let low = (minimum * rate).ceil() as u64;
        let high = (maximum * rate).floor() as u64;
        if base >= low {
            return Ok(Some(plan.clone()));
        }
        let budget =
            ((base as f64 * MAX_REPLAY_FRACTION).floor() as u64).min(high.saturating_sub(base));
        if base + budget < low {
            return Ok(None);
        }
        let mut choices = Vec::new();
        for index in 0..plan.placements.len() {
            let options = self.placement_options(store, plan, index, true, cancel)?;
            if !options.is_empty() {
                choices.push(options);
            }
        }
        // Bounded multiple-choice knapsack. A song gets one replay region and
        // at most three additional cycles. Keep exact frames, bucket by second.
        let mut states = BTreeMap::from([(0u64, (0u64, 0f64, Vec::<PassageReplay>::new()))]);
        for options in choices {
            cancel.check()?;
            let mut next = states.clone();
            for (added, cost, replays) in states.values() {
                for (r, quality) in &options {
                    for repetitions in 1..=3 {
                        let mut r = r.clone();
                        r.repetitions = repetitions;
                        let total = added + r.added_frames();
                        if total > budget {
                            continue;
                        }
                        let score = cost
                            + r.added_frames() as f64 / rate
                                * (1. - quality + 0.15 * f64::from(repetitions))
                            + if r.insert_before != r.capture_end {
                                1.
                            } else {
                                0.
                            };
                        let bucket = total / u64::from(plan.output.sample_rate_hz);
                        // Keep satisfying and not-yet-satisfying states separate
                        // even when they occupy the same second.
                        let bucket = 2 * bucket + u64::from(base + total >= low);
                        if next.get(&bucket).is_some_and(|(_, old, _)| *old <= score) {
                            continue;
                        }
                        let mut selected = replays.clone();
                        selected.push(r);
                        next.insert(bucket, (total, score, selected));
                    }
                }
            }
            states = next;
        }
        let Some((_, _, selected)) = states
            .into_values()
            .filter(|(n, _, _)| base + n >= low)
            .min_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)))
        else {
            return Ok(None);
        };
        materialize(plan, selected).map(Some)
    }
}

#[cfg(feature = "cp-search")]
#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct ReplayCheckpoint {
    next_placement: usize,
    options: Vec<Vec<Option<PassageReplay>>>,
    cursor: Option<mixlib_set_search::clock_choices::ClockCursor>,
}

#[cfg(feature = "cp-search")]
pub(super) struct ReplayPreparation {
    minimum_added: u64,
    budget: u64,
    next: usize,
    options: Vec<Vec<Option<PassageReplay>>>,
    cursor: Option<mixlib_set_search::clock_choices::ClockCursor>,
}

#[cfg(feature = "cp-search")]
impl ReplayPreparation {
    pub(super) fn new(plan: &MixPlan, minimum: f64, maximum: f64) -> Self {
        let rate = f64::from(plan.output.sample_rate_hz);
        let base = plan.total_output_frames;
        let low = (minimum * rate).ceil() as u64;
        let high = (maximum * rate).floor() as u64;
        let budget =
            ((base as f64 * MAX_REPLAY_FRACTION).floor() as u64).min(high.saturating_sub(base));
        let minimum_added = if base > high {
            1
        } else {
            low.saturating_sub(base)
        };
        Self {
            minimum_added,
            budget,
            next: if minimum_added > budget || budget == 0 {
                plan.placements.len()
            } else {
                0
            },
            options: Vec::new(),
            cursor: None,
        }
    }

    pub(super) fn checkpoint(&self) -> ReplayCheckpoint {
        ReplayCheckpoint {
            next_placement: self.next,
            options: self.options.clone(),
            cursor: self.cursor.clone(),
        }
    }

    pub(super) fn restore(&mut self, plan: &MixPlan, saved: ReplayCheckpoint) {
        let n = plan.placements.len();
        if saved.next_placement > n
            || (saved.cursor.is_some() && saved.next_placement != n)
            || saved.options.len() > saved.next_placement
        {
            return;
        }
        let mut previous = None;
        for group in &saved.options {
            let Some(Some(first)) = group.get(1) else {
                return;
            };
            if !matches!(group.first(), Some(None))
                || first.placement >= saved.next_placement
                || previous.is_some_and(|old| first.placement <= old)
                || group.iter().skip(1).any(|replay| {
                    replay.as_ref().is_none_or(|r| {
                        r.placement != first.placement
                            || r.capture_end <= r.capture_start
                            || r.capture_end > r.insert_before
                            || r.insert_before >= plan.total_output_frames
                            || !(1..=3).contains(&r.repetitions)
                    })
                })
            {
                return;
            }
            previous = Some(first.placement);
        }
        self.next = saved.next_placement;
        self.options = saved.options;
        self.cursor = saved.cursor;
    }

    fn finish(&mut self) -> ReplayDomain {
        let options = std::mem::take(&mut self.options);
        let durations = options
            .iter()
            .map(|group| {
                group
                    .iter()
                    .map(|r| r.as_ref().map_or(0, PassageReplay::added_frames))
                    .collect()
            })
            .collect();
        let mut clock = mixlib_set_search::clock_choices::ClockChoices::new(
            durations,
            self.minimum_added,
            self.budget,
        );
        if let Some(cursor) = self.cursor.take() {
            clock.restore(cursor);
        }
        ReplayDomain { options, clock }
    }
}

#[cfg(feature = "cp-search")]
pub(super) struct ReplayDomain {
    options: Vec<Vec<Option<PassageReplay>>>,
    pub(super) clock: mixlib_set_search::clock_choices::ClockChoices,
}

#[cfg(feature = "cp-search")]
impl ReplayDomain {
    pub(super) fn checkpoint(&self, placements: usize) -> ReplayCheckpoint {
        ReplayCheckpoint {
            next_placement: placements,
            options: self.options.clone(),
            cursor: Some(self.clock.cursor()),
        }
    }

    pub(super) fn plan(&self, base: &MixPlan, choices: &[usize]) -> Result<MixPlan> {
        let selected = self
            .options
            .iter()
            .zip(choices)
            .filter_map(|(group, &choice)| group[choice].clone())
            .collect();
        materialize(base, selected)
    }
}

fn materialize(plan: &MixPlan, mut selected: Vec<PassageReplay>) -> Result<MixPlan> {
    if selected.is_empty() {
        return Ok(plan.clone());
    }
    selected.sort_by_key(|r| r.insert_before);
    let arrangement = SetArrangement {
        version: 1,
        base_output_frames: plan.total_output_frames,
        replays: selected,
    };
    let mut result = plan.clone();
    result.total_output_frames += arrangement.added_frames();
    result.renderer_version = ARRANGEMENT_RENDERER.into();
    result.header.extensions.insert(
        ARRANGEMENT_EXTENSION.into(),
        serde_json::to_value(arrangement)?,
    );
    result.validate()?;
    Ok(result)
}

fn output_at(map: &TimeMap, source: u64) -> Option<u64> {
    if source < map.anchors.first()?.source_frame || source > map.anchors.last()?.source_frame {
        return None;
    }
    let mut a = map.anchors.first()?.output_frame;
    let mut b = map.anchors.last()?.output_frame;
    while b - a > 1 {
        let m = a + (b - a) / 2;
        if map.source_at(m as f64) < source as f64 {
            a = m;
        } else {
            b = m;
        }
    }
    Some(
        if (map.source_at(a as f64) - source as f64).abs()
            < (map.source_at(b as f64) - source as f64).abs()
        {
            a
        } else {
            b
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duration_selection_uses_whole_cycles_and_obeys_repetition_budget() {
        let mut plan: MixPlan =
            serde_json::from_str(include_str!("../../../examples/mix-plan.json")).unwrap();
        plan.placements.truncate(1);
        plan.transitions.clear();
        plan.total_output_frames = 20 * 48000;
        let p = &plan.placements[0];
        let key = canonical_hash(&serde_json::json!([
            p.track_id,
            p.audio_hash,
            p.analysis,
            p.override_revision
        ]))
        .unwrap()
        .digest;
        let cache = ReplayCache::default();
        cache.0.borrow_mut().insert(
            key,
            vec![
                mixlib_planner::MeasuredReplay {
                    start: 2 * 48000,
                    end: 3 * 48000,
                    destination: 5 * 48000,
                    bars: 4,
                    quality: 0.95,
                    evidence: serde_json::json!({"fixture":true}),
                },
                mixlib_planner::MeasuredReplay {
                    start: 2 * 48000,
                    end: 3 * 48000 + 3360,
                    destination: 5 * 48000,
                    bars: 4,
                    quality: 0.90,
                    evidence: serde_json::json!({"fixture":true}),
                },
            ],
        );
        let temp = tempfile::tempdir().unwrap();
        let store = Store::init(temp.path()).unwrap();
        let cancel = Cancellation::default();
        let chosen = cache
            .arrange(&store, &plan, 21.1, 22., &cancel)
            .unwrap()
            .unwrap();
        assert_eq!(
            chosen.total_output_frames,
            22 * 48000,
            "select two full cycles, never a cropped cycle"
        );
        let arrangement = read_arrangement(&chosen).unwrap().unwrap();
        assert_eq!(arrangement.replays[0].repetitions, 2);
        assert_eq!(arrangement.replays[0].insert_before, 5 * 48000);
        assert_eq!(arrangement.output_at(6 * 48000), 8 * 48000);
        assert!(
            cache
                .arrange(&store, &plan, 21.1, 21.9, &cancel)
                .unwrap()
                .is_none(),
            "no whole cycle fits this window"
        );
        assert!(
            cache
                .arrange(&store, &plan, 24., 25., &cancel)
                .unwrap()
                .is_none(),
            "do not manufacture a set by unlimited repeats"
        );
        #[cfg(feature = "cp-search")]
        {
            use mixlib_set_search::clock_choices::Advance;
            assert!(cache
                .arrange(&store, &plan, 21.06, 21.08, &cancel)
                .unwrap()
                .is_none());
            let mut prep = ReplayPreparation::new(&plan, 21.06, 21.08);
            let mut exact = cache
                .advance_preparation(&store, &plan, &mut prep, &cancel)
                .unwrap()
                .unwrap();
            let Advance::Candidate(choices) = exact.clock.advance(32) else {
                panic!("the second four-bar region fits the narrow clock");
            };
            let arranged = exact.plan(&plan, &choices).unwrap();
            assert_eq!(arranged.total_output_frames, 21 * 48000 + 3360);
            assert_eq!(exact.clock.advance(32), Advance::Exhausted);

            let mut prep = ReplayPreparation::new(&plan, 21., 22.);
            let mut original = cache
                .advance_preparation(&store, &plan, &mut prep, &cancel)
                .unwrap()
                .unwrap();
            assert!(matches!(original.clock.advance(32), Advance::Candidate(_)));
            let serialized =
                serde_json::to_vec(&original.checkpoint(plan.placements.len())).unwrap();
            let mut prep = ReplayPreparation::new(&plan, 21., 22.);
            prep.restore(&plan, serde_json::from_slice(&serialized).unwrap());
            let mut resumed = ReplayCache::default()
                .advance_preparation(&store, &plan, &mut prep, &cancel)
                .unwrap()
                .unwrap();
            loop {
                let next = original.clock.advance(1);
                assert_eq!(next, resumed.clock.advance(1));
                if next == Advance::Exhausted {
                    break;
                }
            }
        }
    }
}
