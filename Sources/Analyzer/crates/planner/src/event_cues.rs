//! Propose rhythmic windows from measured source events before ranking generic cues.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Family {
    DrumReturn,
    BuildDrop,
    DropSwap,
}

struct Window {
    passage: Passage,
    drum_return: bool,
    releases: BTreeSet<usize>,
    drops: BTreeSet<usize>,
    groove: f64,
    vocal_support: bool,
}

struct IndexedTrack {
    source: Analyzed,
    windows: BTreeMap<usize, [Vec<Window>; 2]>,
}

/// Prepared once per source snapshot. The index proposes timing only. The
/// normal tactic selector still measures the audible lanes and validates exits.
pub struct EventCueIndex {
    context: usize,
    tracks: BTreeMap<String, IndexedTrack>,
}

impl EventCueIndex {
    pub fn new(context: usize) -> Self {
        Self {
            context,
            tracks: BTreeMap::new(),
        }
    }

    fn prepare(&mut self, store: &Store, id: &str, cancel: &Cancellation) -> Result<()> {
        if self.tracks.contains_key(id) {
            return Ok(());
        }
        let source = load_analyzed(store, store.track(id)?)?;
        let mut windows = BTreeMap::new();
        if let Some(reference) = source.analysis.artifacts.get("tactic_analysis") {
            let record: TacticTrackAnalysis = store.read_ref(reference)?;
            let tolerance = u64::from(record.source_sample_rate_hz) * 8 / 100;
            let returns: Vec<_> = crate::transition_endpoints::arrivals(&record)
                .into_iter()
                .map(|e| e.frame)
                .collect();
            let mut drops = vec![];
            let mut releases = vec![];
            for event in record.signal_evidence["events"]
                .as_array()
                .into_iter()
                .flatten()
            {
                if !event["score"]
                    .as_f64()
                    .is_some_and(|s| (0.6..=1.).contains(&s))
                {
                    continue;
                }
                let Some(frame) = event["frame"].as_u64() else {
                    continue;
                };
                match event["kind"].as_str() {
                    Some("drop") => drops.push(frame),
                    Some("build") => {
                        if let Some(end) = event["end_frame"].as_u64().filter(|&e| e > frame) {
                            releases.push(crate::transition_endpoints::build_release(
                                &record, frame, end,
                            ));
                        }
                    }
                    _ => {}
                }
            }
            let groove: BTreeSet<_> = record.signal_evidence["bars"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|b| {
                    b["roles"]
                        .as_array()
                        .is_some_and(|r| r.iter().any(|v| v == "drum_groove"))
                })
                .filter_map(|b| b["start_frame"].as_u64())
                .collect();
            let downbeats: BTreeSet<_> = source.beats.downbeat_ids.iter().collect();
            let lengths: BTreeSet<_> = source
                .beats
                .events
                .iter()
                .enumerate()
                .filter(|(_, b)| downbeats.contains(&b.event_id))
                .map(|(i, _)| i)
                .collect::<Vec<_>>()
                .windows(2)
                .map(|w| w[1] - w[0])
                .filter(|m| (2..=12).contains(m))
                .flat_map(|m| [2, 4, 8].map(|bars| bars * m))
                .collect();
            for length in lengths {
                let mut sides = [vec![], vec![]];
                for (side, side_windows) in sides.iter_mut().enumerate() {
                    for p in passages_for_clock(
                        &source,
                        length,
                        self.context,
                        side == 0,
                        false,
                        false,
                        cancel,
                    )? {
                        let drum_return = returns.iter().any(|f| f.abs_diff(p.start) <= tolerance);
                        // Interior landmarks leave a real lead-in and room after the handoff.
                        let landmark = |events: &[u64]| -> BTreeSet<usize> {
                            p.downbeats
                                .iter()
                                .enumerate()
                                .skip(1)
                                .take(p.downbeats.len() - 2)
                                .filter(|&(_, &beat)| {
                                    events
                                        .iter()
                                        .any(|f| f.abs_diff(p.beat_frames[beat]) <= tolerance)
                                })
                                .map(|(bar, _)| bar)
                                .collect()
                        };
                        let releases = landmark(&releases);
                        let drops = landmark(&drops);
                        let bars = p.downbeats.len() - 1;
                        let groove_fraction = p.downbeats[..bars]
                            .iter()
                            .filter(|&&beat| groove.contains(&p.beat_frames[beat]))
                            .count() as f64
                            / bars as f64;
                        let useful = if side == 0 {
                            (bars >= 4 && drum_return) || !releases.is_empty() || !drops.is_empty()
                        } else {
                            (bars >= 4 && groove_fraction >= 0.5) || !drops.is_empty()
                        };
                        if useful {
                            let vocal_support = crate::tactic_cues::vocal_window_supported(
                                Some(&record),
                                p.start,
                                p.end,
                            );
                            side_windows.push(Window {
                                passage: p,
                                drum_return,
                                releases,
                                drops,
                                groove: groove_fraction,
                                vocal_support,
                            });
                        }
                    }
                }
                windows.insert(length, sides);
            }
        }
        self.tracks
            .insert(id.into(), IndexedTrack { source, windows });
        Ok(())
    }

    pub fn candidates(
        &mut self,
        store: &Store,
        request: &StitchRequest<'_>,
        policy: &MixingConfig,
        per_family: usize,
        cancel: &Cancellation,
    ) -> Result<Vec<MixPlan>> {
        self.prepare(store, request.from, cancel)?;
        self.prepare(store, request.to, cancel)?;
        let a = &self.tracks[request.from];
        let b = &self.tracks[request.to];
        if a.source.track.audio_hash == b.source.track.audio_hash
            || a.source.overrides.operations.iter().any(|o| {
                matches!(o, Correction::ExcludeTransition { to_track_id } if to_track_id == request.to)
            }) {
            return Ok(vec![]);
        }
        let mut ranked: BTreeMap<Family, Vec<_>> = BTreeMap::new();
        for (length, [outgoing, _]) in &a.windows {
            for incoming_length in [*length, *length * 2, *length / 2] {
                let Some([_, incoming]) = b.windows.get(&incoming_length) else {
                    continue;
                };
                for x in outgoing
                    .iter()
                    .filter(|w| w.passage.start >= request.outgoing_after_frame)
                {
                    cancel.check()?;
                    for y in incoming {
                        let bars = x.passage.downbeats.len() - 1;
                        let incoming_bars = y.passage.downbeats.len() - 1;
                        let families = [
                            (
                                Family::DrumReturn,
                                bars >= 4 && x.drum_return && y.groove >= 0.5,
                            ),
                            (
                                Family::BuildDrop,
                                x.releases.iter().any(|release| {
                                    let phase = release * incoming_bars;
                                    y.drops.iter().any(|drop| {
                                        (phase..=phase + incoming_bars).contains(&(drop * bars))
                                    })
                                }),
                            ),
                            (
                                Family::DropSwap,
                                x.drops
                                    .iter()
                                    .any(|a| y.drops.iter().any(|b| a * incoming_bars == b * bars)),
                            ),
                        ];
                        if !families.iter().any(|(_, matches)| *matches) {
                            continue;
                        }
                        let Some((_, deviation, skew)) =
                            mapped_pair_timing(&x.passage, &y.passage, policy)
                        else {
                            continue;
                        };
                        let entry_fraction =
                            y.passage.end as f64 / b.source.track.format.frames as f64;
                        let exit_fraction =
                            x.passage.start as f64 / a.source.track.format.frames as f64;
                        let cost = 4. * skew + deviation + 0.35 * entry_fraction
                            - 0.35 * exit_fraction
                            + 0.2 * (f64::from(!x.vocal_support) + f64::from(!y.vocal_support));
                        for (family, matches) in families {
                            if matches {
                                ranked.entry(family).or_default().push((
                                    cost,
                                    x,
                                    y,
                                    *length,
                                    incoming_length,
                                ));
                            }
                        }
                    }
                }
            }
        }
        let mut seen = BTreeSet::new();
        let mut plans = vec![];
        for (family, mut choices) in ranked {
            choices.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut count = 0;
            for (cost, x, y, length, incoming_length) in choices {
                if count == per_family {
                    break;
                }
                let key = [
                    x.passage.start,
                    x.passage.end,
                    y.passage.start,
                    y.passage.end,
                ];
                if !seen.insert(key) {
                    continue;
                }
                let pinned = StitchRequest {
                    overlap_beats: length,
                    context_beats: self.context,
                    ..*request
                };
                let result = build_stitch_from_tracks(
                    store,
                    [&a.source, &b.source],
                    &pinned,
                    policy,
                    None,
                    None,
                    StitchStyle::PinnedTactic {
                        outgoing: key[0],
                        incoming: key[2],
                        incoming_overlap: incoming_length,
                    },
                    cancel,
                );
                let (mut plan, _) = match result {
                    Ok(plan) => plan,
                    Err(e)
                        if matches!(
                            e.code.as_str(),
                            "STITCH_INFEASIBLE" | "TIME_MAP_SPEED" | "TIME_MAP_ACCELERATION"
                        ) =>
                    {
                        continue
                    }
                    Err(e) => return Err(e),
                };
                plan.header.extensions.insert("tactic_cue_candidate".into(), serde_json::json!({
                    "event_family":format!("{family:?}"),"outgoing_source_frame":key[0],"incoming_source_frame":key[2],
                    "timing_basis":"bars","overlap_beats":length,"incoming_overlap_beats":incoming_length,
                    "pulse_ratio":incoming_length as f64/length as f64,
                    "bar_beats":x.passage.bar_beats,"proposal_cost":cost,
                }));
                plans.push(plan);
                count += 1;
            }
        }
        Ok(plans)
    }
}
