//! Cheap full-library proposals for the gradual stereo baseline. Track summaries
//! are cached in the source snapshot; every proposed join still needs ordinary
//! schedule-specific measurement and admission before it can become audible.
use super::*;

const VOCAL_GUARD_SECONDS: f64 = 0.150;
const VOCAL_SEPARATION_SECONDS: f64 = 0.300;

struct Window {
    passage: Passage,
    chroma: [f64; 12],
    rms: f64,
    entry_quiet_margin: u64,
    exit_quiet_margin: u64,
    possible_vocals: u64,
}
struct IndexedTrack {
    source: Analyzed,
    windows: BTreeMap<usize, [Vec<Window>; 2]>,
}

/// A reusable timing/proposal index, not a cache of approved transitions.
pub struct BaselineCueIndex {
    context: usize,
    tracks: BTreeMap<String, IndexedTrack>,
}

fn quiet_margin(spans: &[(u64, u64)], frame: u64) -> u64 {
    spans
        .iter()
        .filter(|&&(start, end)| start <= frame && frame <= end)
        .map(|&(start, end)| (frame - start).min(end - frame))
        .max()
        .unwrap_or(0)
}

/// Unknown bins remain possibly vocal. The mask only ranks proposals; the
/// actual lane schedules, neighbouring context and tempo map decide admission.
fn vocal_mask(spans: &[(u64, u64)], start: u64, end: u64) -> u64 {
    let mut mask = 0;
    for i in 0..64 {
        let begin = start + ((end - start) as u128 * i as u128 / 64) as u64;
        let finish = start + ((end - start) as u128 * (i + 1) as u128 / 64) as u64;
        if !spans.iter().any(|&(a, b)| a <= begin && b >= finish) {
            mask |= 1u64 << i;
        }
    }
    mask
}

fn vocal_risk(a: u64, b: u64, duration: f64) -> f64 {
    let guard = (VOCAL_SEPARATION_SECONDS / duration.max(1e-6) * 64.).ceil() as u32;
    let mut expanded = a;
    for shift in 1..=guard.min(63) {
        expanded |= (a << shift) | (a >> shift);
    }
    (expanded & b).count_ones() as f64 / 64.
}

fn required_gate(window: &Window, outgoing: bool, guard: u64) -> bool {
    // S13 fades out A at the END and admits B at the START. The opposite ends
    // are continuing music and must not be mistaken for vocal gate locations.
    if outgoing {
        window.exit_quiet_margin >= guard
    } else {
        window.entry_quiet_margin >= guard
    }
}

fn exit_band(fraction: f64) -> u8 {
    if fraction >= 0.8 {
        3
    } else if fraction >= 0.6 {
        2
    } else if fraction >= 0.35 {
        1
    } else {
        0
    }
}
fn entry_band(fraction: f64) -> u8 {
    if fraction >= 0.65 {
        3
    } else if fraction >= 0.4 {
        2
    } else if fraction >= 0.2 {
        1
    } else {
        0
    }
}

#[derive(Clone, Copy)]
struct Choice {
    cost: f64,
    length: usize,
    outgoing: usize,
    incoming: usize,
    exit_band: u8,
    entry_band: u8,
    bars: usize,
    outgoing_frame: u64,
    incoming_frame: u64,
}
fn compare(a: &Choice, b: &Choice) -> std::cmp::Ordering {
    a.cost
        .total_cmp(&b.cost)
        .then(a.outgoing_frame.cmp(&b.outgoing_frame))
        .then(a.incoming_frame.cmp(&b.incoming_frame))
        .then(a.length.cmp(&b.length))
}

/// Keep different exit regions before spending all proposals on interchangeable
/// versions of one cheap early passage. Every fallback remains rankable.
fn diversified(mut choices: Vec<Choice>) -> Vec<Choice> {
    choices.sort_by(compare);
    let mut ordered = vec![];
    let mut used = BTreeSet::new();
    let mut take = |index: usize| {
        if used.insert(index) {
            ordered.push(choices[index]);
        }
    };
    if choices.is_empty() {
        return ordered;
    }
    take(0);
    // Reserve an alternative late exit even when the absolute cheapest window
    // is near the beginning. Higher limits retain the 80/60/35% regions too.
    for band in [3, 2, 1, 0] {
        if choices[0].exit_band != band {
            if let Some(index) = choices.iter().position(|c| c.exit_band == band) {
                take(index);
            }
        }
    }
    // The strongest cue for each duration and incoming runway also survives.
    for bars in [4, 8] {
        for entry in [0, 1, 2, 3] {
            if let Some(index) = choices
                .iter()
                .position(|c| c.bars == bars && c.entry_band == entry)
            {
                take(index);
            }
        }
    }
    for index in 0..choices.len() {
        take(index);
    }
    ordered
}

impl BaselineCueIndex {
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
        let features = ReturnFeatures::read(store, &source, cancel)?;
        let record: Option<TacticTrackAnalysis> = source
            .analysis
            .artifacts
            .get("tactic_analysis")
            .map(|r| store.read_ref(r))
            .transpose()?;
        let quiet = record
            .as_ref()
            .and_then(|r| r.vocal_evidence.get("track"))
            .map(|v| {
                crate::cached_tactic_evidence::source_quiet_spans(
                    v,
                    source.track.format.sample_rate_hz,
                )
            })
            .unwrap_or_default();
        let downbeats: BTreeSet<_> = source.beats.downbeat_ids.iter().collect();
        let lengths: BTreeSet<_> = source
            .beats
            .events
            .iter()
            .enumerate()
            .filter(|(_, beat)| downbeats.contains(&beat.event_id))
            .map(|(i, _)| i)
            .collect::<Vec<_>>()
            .windows(2)
            .map(|w| w[1] - w[0])
            .filter(|m| (2..=12).contains(m))
            .flat_map(|meter| [4 * meter, 8 * meter])
            .collect();
        let mut windows = BTreeMap::new();
        for length in lengths {
            let mut sides = [vec![], vec![]];
            for (side, result) in sides.iter_mut().enumerate() {
                for passage in passages_for_clock(
                    &source,
                    length,
                    self.context,
                    side == 0,
                    false,
                    false,
                    cancel,
                )? {
                    let bars = passage.downbeats.len() - 1;
                    if !matches!(bars, 4 | 8) {
                        continue;
                    }
                    let frames = features.between(passage.start, passage.end);
                    let norm = frames.iter().map(|f| f.rms).sum::<f64>().max(1e-9);
                    let chroma = std::array::from_fn(|c| {
                        frames.iter().map(|f| f.chroma[c] * f.rms).sum::<f64>() / norm
                    });
                    result.push(Window {
                        chroma,
                        rms: features.rms(passage.start, passage.end),
                        entry_quiet_margin: quiet_margin(&quiet, passage.start),
                        exit_quiet_margin: quiet_margin(&quiet, passage.end),
                        possible_vocals: vocal_mask(&quiet, passage.start, passage.end),
                        passage,
                    });
                }
            }
            windows.insert(length, sides);
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
        limit: usize,
        cancel: &Cancellation,
    ) -> Result<Vec<MixPlan>> {
        policy.validate()?;
        require(
            (1..=64).contains(&limit),
            "TACTIC_BUDGET",
            "baseline proposal limit must be 1..64",
        )?;
        self.prepare(store, request.from, cancel)?;
        self.prepare(store, request.to, cancel)?;
        let a = &self.tracks[request.from];
        let b = &self.tracks[request.to];
        if request.from==request.to || a.source.track.audio_hash==b.source.track.audio_hash
            || a.source.overrides.operations.iter().any(|o| matches!(o,Correction::ExcludeTransition{to_track_id} if to_track_id==request.to)) {
            return Ok(vec![]);
        }
        let gate_guard = |track: &Analyzed| {
            (VOCAL_GUARD_SECONDS
                * (1. + policy.max_speed_deviation)
                * f64::from(track.track.format.sample_rate_hz))
            .ceil() as u64
        };
        let a_guard = gate_guard(&a.source);
        let b_guard = gate_guard(&b.source);
        // Bound retained ranking data independently of source duration. All
        // windows are screened, but nearby copies cannot fill the result quota.
        let mut buckets: BTreeMap<(u8, u8, usize), Vec<Choice>> = BTreeMap::new();
        for (&length, [outgoing, _]) in &a.windows {
            let Some([_, incoming]) = b.windows.get(&length) else {
                continue;
            };
            for (ai, x) in outgoing.iter().enumerate().filter(|(_, w)| {
                w.passage.start >= request.outgoing_after_frame && required_gate(w, true, a_guard)
            }) {
                cancel.check()?;
                for (bi, y) in incoming
                    .iter()
                    .enumerate()
                    .filter(|(_, w)| required_gate(w, false, b_guard))
                {
                    let Some((duration, deviation, skew)) =
                        pair_timing(&x.passage, &y.passage, policy)
                    else {
                        continue;
                    };
                    if duration > MAX_TACTIC_CONTINUATION_SECONDS {
                        continue;
                    }
                    let exit_fraction =
                        x.passage.start as f64 / a.source.track.format.frames as f64;
                    let entry_fraction = y.passage.end as f64 / b.source.track.format.frames as f64;
                    let harmony = x
                        .chroma
                        .iter()
                        .zip(y.chroma)
                        .map(|(a, b)| (a - b).abs())
                        .sum::<f64>()
                        / 2.;
                    let level = (x.rms - y.rms).abs() / x.rms.max(y.rms).max(1e-9);
                    let vocal = vocal_risk(x.possible_vocals, y.possible_vocals, duration);
                    let tail = (b.source.track.format.frames - y.passage.end) as f64
                        / f64::from(b.source.track.format.sample_rate_hz);
                    let cost = 4. * skew
                        + deviation
                        + 0.3 * harmony
                        + 0.2 * level
                        + 4. * vocal
                        + 0.4 * entry_fraction
                        - 0.4 * exit_fraction
                        + 0.4 * ((90. - tail) / 90.).max(0.);
                    let choice = Choice {
                        cost,
                        length,
                        outgoing: ai,
                        incoming: bi,
                        exit_band: exit_band(exit_fraction),
                        entry_band: entry_band(entry_fraction),
                        bars: x.passage.downbeats.len() - 1,
                        outgoing_frame: x.passage.start,
                        incoming_frame: y.passage.start,
                    };
                    let bucket = buckets
                        .entry((choice.exit_band, choice.entry_band, choice.bars))
                        .or_default();
                    let a_separation = 2 * u64::from(a.source.track.format.sample_rate_hz);
                    let b_separation = 2 * u64::from(b.source.track.format.sample_rate_hz);
                    if let Some(index) = bucket.iter().position(|old| {
                        old.outgoing_frame.abs_diff(choice.outgoing_frame) < a_separation
                            && old.incoming_frame.abs_diff(choice.incoming_frame) < b_separation
                    }) {
                        if compare(&choice, &bucket[index]).is_lt() {
                            bucket[index] = choice;
                        } else {
                            continue;
                        }
                    } else {
                        bucket.push(choice);
                    }
                    bucket.sort_by(compare);
                    bucket.truncate(4);
                }
            }
        }
        let ordered = diversified(buckets.into_values().flatten().collect());
        let mut plans = vec![];
        for choice in ordered {
            if plans.len() >= limit {
                break;
            }
            let x = &a.windows[&choice.length][0][choice.outgoing];
            let y = &b.windows[&choice.length][1][choice.incoming];
            let pinned = StitchRequest {
                overlap_beats: choice.length,
                context_beats: self.context,
                ..*request
            };
            let (mut plan, _) = match build_stitch_from_tracks(
                store,
                [&a.source, &b.source],
                &pinned,
                policy,
                None,
                None,
                StitchStyle::PinnedTactic {
                    outgoing: x.passage.start,
                    incoming: y.passage.start,
                    incoming_overlap: choice.length,
                },
                cancel,
            ) {
                Ok(p) => p,
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
            plan.header.extensions.insert("tactic_cue_candidate".into(),serde_json::json!({
                "proposal_family":"baseline_s13/1","outgoing_source_frame":x.passage.start,"incoming_source_frame":y.passage.start,
                "timing_basis":"bars","overlap_beats":choice.length,"incoming_overlap_beats":choice.length,"pulse_ratio":1.,
                "bar_beats":x.passage.bar_beats,"proposal_cost":choice.cost,"exit_region":choice.exit_band,
                "entry_region":choice.entry_band,"verified_gate_locations":["outgoing_end","incoming_start"],
                "possible_vocal_overlap_proxy":vocal_risk(x.possible_vocals,y.possible_vocals,(x.passage.seconds+y.passage.seconds)/2.),
                "semantics":"Timing and source-evidence proposal only; schedule-specific audible vocals, harmony, peak and buildup admission remain mandatory."
            }));
            plans.push(plan);
        }
        Ok(plans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn window(entry: u64, exit: u64) -> Window {
        Window {
            passage: Passage {
                start: 100,
                end: 900,
                context: 0,
                seconds: 8.,
                beat_phases: vec![],
                beat_frames: vec![],
                downbeats: vec![],
                grid_error: 0.,
                bar_beats: 4,
                continued_meter: false,
            },
            chroma: [0.; 12],
            rms: 0.,
            entry_quiet_margin: entry,
            exit_quiet_margin: exit,
            possible_vocals: 0,
        }
    }
    #[test]
    fn gradual_fade_proposals_require_the_actual_vocal_gate_ends() {
        let a = window(0, 170);
        let b = window(170, 0);
        assert!(required_gate(&a, true, 159));
        assert!(required_gate(&b, false, 159));
        assert!(!required_gate(&a, false, 159));
        assert!(!required_gate(&b, true, 159));
        assert!(
            !required_gate(&window(500, 0), true, 159),
            "a quiet lead-in cannot substitute for the outgoing end gap"
        );
        assert!(
            !required_gate(&window(0, 500), false, 159),
            "a later incoming gap cannot authorize starting the vocal fade"
        );
    }
    #[test]
    fn unknown_vocals_are_never_misclassified_as_verified_quiet() {
        assert_eq!(vocal_mask(&[], 100, 900), u64::MAX);
        assert_eq!(vocal_mask(&[(0, 1000)], 100, 900), 0);
        assert_eq!(quiet_margin(&[(300, 700)], 500), 200);
        assert_eq!(quiet_margin(&[(300, 700)], 200), 0);
        assert_eq!(vocal_risk(u64::MAX, 0, 16.), 0.);
        assert!(vocal_risk(1 << 20, 1 << 21, 16.) > 0.);
        assert_eq!(vocal_risk(1 << 20, 1 << 24, 16.), 0.);
    }
    #[test]
    fn a_cheaper_early_window_cannot_hide_later_hour_set_exits() {
        let make = |cost, band, frame| Choice {
            cost,
            length: 16,
            outgoing: 0,
            incoming: 0,
            exit_band: band,
            entry_band: 0,
            bars: 4,
            outgoing_frame: frame,
            incoming_frame: 0,
        };
        let ranked = diversified(vec![
            make(0., 0, 10),
            make(0.01, 0, 20),
            make(0.2, 1, 40),
            make(0.3, 2, 70),
            make(0.4, 3, 90),
        ]);
        assert_eq!(
            ranked
                .iter()
                .take(2)
                .map(|c| c.exit_band)
                .collect::<Vec<_>>(),
            vec![0, 3]
        );
        assert_eq!(
            ranked
                .iter()
                .take(4)
                .map(|c| c.exit_band)
                .collect::<Vec<_>>(),
            vec![0, 3, 2, 1]
        );
        assert_eq!(ranked.len(), 5);
        assert_eq!(exit_band(0.35), 1);
        assert_eq!(exit_band(0.6), 2);
        assert_eq!(exit_band(0.8), 3);
    }
}
