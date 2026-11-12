//! A short, ordered transition between two locally steady passages.
use super::*;

#[path = "event_cues.rs"]
mod event_cues;
pub use event_cues::EventCueIndex;
#[path = "baseline_cues.rs"]
mod baseline_cues;
pub use baseline_cues::BaselineCueIndex;

const MAX_BEAT_SKEW_SECONDS: f64 = 0.030;
/// Upper duration of dynamically discovered tactic continuations.
pub const MAX_TACTIC_CONTINUATION_SECONDS: f64 = 120.;

pub struct StitchRequest<'a> {
    pub from: &'a str,
    pub to: &'a str,
    pub overlap_beats: usize,
    pub context_beats: usize,
    pub outgoing_after_frame: u64,
}

struct Passage {
    start: u64,
    end: u64,
    context: u64,
    seconds: f64,
    beat_phases: Vec<f64>,
    beat_frames: Vec<u64>,
    downbeats: Vec<usize>,
    grid_error: f64,
    bar_beats: usize,
    continued_meter: bool,
}

fn passages(
    track: &Analyzed,
    overlap: usize,
    context: usize,
    outgoing: bool,
    pickup: bool,
    cancel: &Cancellation,
) -> Result<Vec<Passage>> {
    passages_for_clock(track, overlap, context, outgoing, pickup, true, cancel)
}

fn reliable_clock(grid: &Beats, first: usize, last: usize, pulse: f64) -> bool {
    let excerpt = &grid.events[first..=last];
    let rate = f64::from(grid.source_sample_rate_hz);
    excerpt.iter().all(|b| b.score >= 0.5)
        && excerpt.windows(2).all(|w| {
            let interval = (w[1].source_frame - w[0].source_frame) as f64 / rate;
            (0.8 * pulse..=1.2 * pulse).contains(&interval)
        })
        && !grid.unreliable_intervals.iter().any(|r| {
            r.start_frame < excerpt.last().unwrap().source_frame
                && r.end_frame > excerpt[0].source_frame
        })
}

fn passages_for_clock(
    track: &Analyzed,
    overlap: usize,
    context: usize,
    outgoing: bool,
    pickup: bool,
    sync_context: bool,
    cancel: &Cancellation,
) -> Result<Vec<Passage>> {
    let beats = &track.beats.events;
    let downbeats: BTreeSet<_> = track.beats.downbeat_ids.iter().collect();
    let rate = track.track.format.sample_rate_hz as f64;
    let mut passages = vec![];
    for (i, beat) in beats.iter().enumerate() {
        cancel.check()?;
        if (!pickup && !downbeats.contains(&beat.event_id)) || (outgoing && i < context) {
            continue;
        }
        let j = i + overlap;
        let k = if outgoing { i - context } else { j + context };
        if j.max(k) >= beats.len() || (!pickup && !downbeats.contains(&beats[j].event_id)) {
            continue;
        }
        let start = beat.source_frame;
        let end = beats[j].source_frame;
        let seconds = (end - start) as f64 / rate;
        let pulse = seconds / overlap as f64;
        // Solo context need not keep the overlap's rhythm: an incoming beat
        // change can be precisely the musical reason to finish the handoff here.
        let (first, last) = if sync_context {
            (i.min(k), j.max(k))
        } else {
            (i, j)
        };
        if !reliable_clock(&track.beats, first, last, pulse) {
            continue;
        }
        let mut accents: Vec<_> = beats[i..=j]
            .iter()
            .enumerate()
            .filter_map(|(n, b)| downbeats.contains(&b.event_id).then_some(n))
            .collect();
        let continued_meter = outgoing
            && !pickup
            && overlap >= 24
            && i >= 12
            && overlap % 4 == 0
            && (i - 12..=i).all(|n| downbeats.contains(&beats[n].event_id) == ((i - n) % 4 == 0))
            && (0..=overlap).step_by(4).all(|n| accents.contains(&n))
            && !track.beats.meter_segments.iter().any(|meter| {
                meter.start_frame < end && meter.end_frame > start && meter.pulses_per_bar != 4
            })
            && accents.windows(2).any(|pair| pair[1] - pair[0] != 4);
        if continued_meter {
            accents = (0..=overlap).step_by(4).collect();
        }
        let bar_beats = if pickup {
            let mut bars = beats[..=j]
                .iter()
                .enumerate()
                .rev()
                .filter(|(_, b)| downbeats.contains(&b.event_id));
            let (Some((last, _)), Some((previous, _))) = (bars.next(), bars.next()) else {
                continue;
            };
            let meter = last - previous;
            if meter < 2 || accents.windows(2).any(|w| w[1] - w[0] != meter) {
                continue;
            }
            meter
        } else {
            if accents[1] < 2 || accents.windows(2).any(|w| w[1] - w[0] != accents[1]) {
                continue;
            }
            accents[1]
        };
        let beat_phases: Vec<_> = beats[i..=j]
            .iter()
            .map(|b| (b.source_frame - start) as f64 / (end - start) as f64)
            .collect();
        let grid_error = beat_phases
            .iter()
            .enumerate()
            .map(|(n, phase)| (phase - n as f64 / overlap as f64).abs() * seconds)
            .fold(0., f64::max);
        passages.push(Passage {
            start,
            end,
            context: beats[k].source_frame,
            seconds,
            beat_phases,
            beat_frames: beats[i..=j].iter().map(|b| b.source_frame).collect(),
            downbeats: accents,
            grid_error,
            bar_beats,
            continued_meter,
        });
    }
    Ok(passages)
}

#[derive(Clone, Copy)]
enum StitchStyle {
    Basic,
    Bands {
        require_space: bool,
    },
    QuietLinger {
        outgoing: u64,
        incoming: u64,
    },
    PinnedTactic {
        outgoing: u64,
        incoming: u64,
        incoming_overlap: usize,
    },
}

const LINGER_BEATS: usize = 32;
// Validate every bar of the hold and resolution. Stem preparation then chooses
// a dense six-plus-two exit or a spacious vocal-pocket-driven brisk exit.
const MASHUP_VERIFIED_BARS: usize = 8;
const LINGER_BARS: [usize; 9] = [0, 4, 8, 12, 16, 20, 24, 28, 32];

fn linger_outgoing(
    track: &Analyzed,
    context: usize,
    cancel: &Cancellation,
) -> Result<Vec<Passage>> {
    // The grid has to be reliable during the overlap. The unshared lead-in can
    // have subdivisions since the other track has not entered yet.
    let beats = &track.beats.events;
    let mut passages = passages(track, LINGER_BEATS, 0, true, false, cancel)?;
    passages.retain_mut(|p| {
        let at = beats.partition_point(|beat| beat.source_frame < p.start);
        if at < context || p.bar_beats != 4 {
            return false;
        }
        p.context = beats[at - context].source_frame;
        true
    });
    Ok(passages)
}

/// A low-bass break can keep its texture while the new drums take over.
pub fn quiet_linger_stitch(
    store: &Store,
    request: &StitchRequest<'_>,
    policy: &MixingConfig,
    cancel: &Cancellation,
) -> Result<Option<(MixPlan, DrumHandoff)>> {
    Ok(
        quiet_linger_candidates(store, request, policy, 1, 0, cancel)?
            .into_iter()
            .next(),
    )
}

/// Rank distinct outgoing passages so stem analysis can reject a cue
/// without giving up on the transition type.
pub fn quiet_linger_candidates(
    store: &Store,
    request: &StitchRequest<'_>,
    policy: &MixingConfig,
    limit: usize,
    min_incoming_tail_seconds: u64,
    cancel: &Cancellation,
) -> Result<Vec<(MixPlan, DrumHandoff)>> {
    policy.validate()?;
    let from = load_analyzed(store, store.track(request.from)?)?;
    let to = load_analyzed(store, store.track(request.to)?)?;
    let outgoing = ReturnFeatures::read(store, &from, cancel)?;
    let incoming = ReturnFeatures::read(store, &to, cancel)?;
    let typical_bass = outgoing.bass_rms(0, from.track.format.frames);
    if typical_bass < 1e-5 {
        return Ok(vec![]);
    }
    let phrase_positions = phrase::infer_phrase_positions(&from.beats, &outgoing.frames);
    let outgoing_profile = TileProfile::new(&from, &outgoing);
    let incoming_profile = TileProfile::new(&to, &incoming);
    let entries: Vec<_> = passages(
        &to,
        LINGER_BEATS,
        request.context_beats,
        false,
        false,
        cancel,
    )?
    .into_iter()
    .filter_map(|p| {
        let tail = min_incoming_tail_seconds * u64::from(to.track.format.sample_rate_hz);
        if p.bar_beats != 4 || to.track.format.frames - p.end < tail {
            return None;
        }
        let bars = incoming_profile.bar_tiles(&p.beat_frames[..=MASHUP_VERIFIED_BARS * 4], 4);
        if bars[0].relative_level.min(bars[1].relative_level) < 0.65 {
            return None;
        }
        let earlier = p.start.saturating_sub(p.beat_frames[8] - p.start);
        let tile = incoming_profile.tile(p.start, p.beat_frames[8], 2, 8);
        let lift = tile.relative_level - incoming.rms(earlier, p.start) / incoming_profile.rms;
        let ranked = passage_prefix(&p, 24);
        Some((p, ranked, tile, bars, lift.clamp(0., 0.75)))
    })
    .collect();

    let exits = linger_outgoing(&from, request.context_beats, cancel)?;
    let mut candidates = vec![];
    for a in &exits {
        cancel.check()?;
        if a.start < request.outgoing_after_frame {
            continue;
        }
        let frames = &a.beat_frames;
        // Rank the same six-bar opportunity even when its hold is extended.
        let ranked_a = passage_prefix(a, 24);
        let earlier = a.start.saturating_sub(ranked_a.end - a.start);
        let bass_limit = (0.58 * typical_bass).min(0.72 * outgoing.bass_rms(earlier, a.start));
        let bars = outgoing_profile.bar_tiles(&frames[..=MASHUP_VERIFIED_BARS * 4], 4);
        let quiet = |bar: &BarTile| {
            (0.30..=0.78).contains(&bar.relative_level)
                && bar.relative_bass * outgoing_profile.bass <= bass_limit
        };
        let legacy_break = bars[..3].iter().all(quiet);
        let short_break = bars[..2].iter().all(quiet);
        let tile = outgoing_profile.tile(a.start, frames[8], 2, 8);
        let steady = bars[..2]
            .iter()
            .all(|bar| (0.45..=1.18).contains(&bar.relative_level))
            && (bars[0].relative_level - bars[1].relative_level).abs() <= 0.15
            && harmonic_distance(&bars[0], &bars[1]) <= 0.18
            && tile.onset <= 0.005;
        if !short_break && !steady && !mashup_capabilities(&bars).0 {
            continue;
        }

        let mut best = None;
        let mut best_cost = f64::INFINITY;
        for (b, ranked_b, incoming_tile, incoming_bars, lift) in &entries {
            if pair_timing(a, b, policy).is_none() {
                continue;
            }
            let Some((_, stretch, skew)) = pair_timing(&ranked_a, ranked_b, policy) else {
                continue;
            };
            let harmonic = harmonic_distance(&tile, incoming_tile);
            let mashup = mashup_compatibility(&bars, incoming_bars);
            let groove =
                steady && harmonic <= 0.19 && incoming_tile.onset >= (1.45 * tile.onset).max(0.006);
            if !short_break && !groove && mashup.is_none() {
                continue;
            }
            // Keep the established quiet-bed tactic ahead of the more permissive tiles.
            // Favor entries with more room to develop before the next join.
            let extra = if legacy_break {
                0.
            } else {
                if TrackPacing::from_features(&to.beats, &incoming.frames).earliest_exit(b.end)
                    + 16 * u64::from(to.track.format.sample_rate_hz)
                    > to.track.format.frames
                {
                    continue;
                }
                0.25 + 0.07 * harmonic + 0.025 * b.start as f64 / to.track.format.frames as f64
            };
            let ranked_mashup = mashup_compatibility_for_bars(&bars, incoming_bars, 5);
            let cost = skew
                + 0.5 * (ranked_a.grid_error + ranked_b.grid_error)
                + 0.2 * stretch
                + 0.1 * tile.relative_bass
                - 0.05 * lift
                + extra
                - ranked_mashup.map_or(0., |harmonic| 0.40 - 0.10 * harmonic);
            if cost < best_cost {
                best_cost = cost;
                best = Some((b.start, mashup));
            }
        }
        if let Some((incoming, mashup)) = best {
            candidates.push((best_cost, a.start, incoming, mashup));
        }
    }
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut selected = vec![];
    let mut plans = vec![];
    let separation = 8 * u64::from(from.track.format.sample_rate_hz);
    for &(_, original_cue, original_incoming, original_mashup) in &candidates {
        let (outgoing, incoming, mashup) = match phrase_positions.get(&original_cue) {
            Some(position) if !position.boundary => {
                let Some((&boundary, _)) = phrase_positions
                    .range(original_cue..)
                    .find(|(_, p)| p.boundary && p.anchor_frame == position.anchor_frame)
                else {
                    continue;
                };
                let Some(&(_, _, incoming, mashup)) =
                    candidates.iter().find(|(_, cue, _, _)| *cue == boundary)
                else {
                    continue;
                };
                // Moving to the phrase boundary should preserve the incoming
                // groove when the extended pair remains compatible.
                let nearby = original_mashup.and_then(|_| {
                    let outgoing = exits.iter().find(|p| p.start == boundary)?;
                    let bars = outgoing_profile
                        .bar_tiles(&outgoing.beat_frames[..=MASHUP_VERIFIED_BARS * 4], 4);
                    entries
                        .iter()
                        .filter_map(|(entry, _, _, incoming_bars, _)| {
                            pair_timing(outgoing, entry, policy)?;
                            let harmonic = mashup_compatibility(&bars, incoming_bars)?;
                            Some((entry.start, harmonic))
                        })
                        .min_by_key(|(frame, _)| frame.abs_diff(original_incoming))
                });
                nearby.map_or((boundary, incoming, mashup), |(entry, harmonic)| {
                    (boundary, entry, Some(harmonic))
                })
            }
            _ => (original_cue, original_incoming, original_mashup),
        };
        if plans.len() == limit {
            break;
        }
        if selected
            .iter()
            .any(|&frame| outgoing.abs_diff(frame) < separation)
        {
            continue;
        }
        let style = StitchStyle::QuietLinger { outgoing, incoming };
        let (mut plan, handoff) = build_stitch(store, request, policy, None, None, style, cancel)?;
        if let Some(harmonic) = mashup {
            plan.objective = "drum_mashup".into();
            plan.header.extensions.insert(
                "mashup".into(),
                serde_json::json!({
                    "hold_bars": 6, "entry_beats": 1, "exit_bars": 1,
                    "exit_policy": "adaptive",
                    "verified_bars": MASHUP_VERIFIED_BARS,
                    "harmonic_distance": harmonic,
                }),
            );
            plan.diagnostics.retain(|message| {
                !message.contains("hold it under incoming drums for two bars")
                    && !message.contains("accompaniment enters")
            });
            plan.diagnostics.push(format!(
                "six-bar mashup candidate: sustained outgoing texture and incoming groove; mean chroma distance {harmonic:.3}; full-window stem density and vocal evidence choose the exit"
            ));
        }
        if let Some(position) = phrase_positions.get(&outgoing) {
            plan.header.extensions.insert(
                "phrase_entry".into(),
                serde_json::json!({
                    "source_frame": outgoing,
                    "original_cue_frame": original_cue,
                    "position": position,
                }),
            );
            plan.diagnostics.push(format!(
                "drum entry on inferred four-bar phrase boundary at {:.3}s; phase evidence margin {:.3}",
                outgoing as f64 / f64::from(from.track.format.sample_rate_hz), position.confidence,
            ));
        }
        selected.push(outgoing);
        plans.push((plan, handoff.expect("quiet linger has a vocal window")));
    }
    Ok(plans)
}

pub fn stitch(
    store: &Store,
    request: &StitchRequest<'_>,
    policy: &MixingConfig,
    cancel: &Cancellation,
) -> Result<MixPlan> {
    build_stitch(
        store,
        request,
        policy,
        None,
        None,
        StitchStyle::Basic,
        cancel,
    )
    .map(|(plan, _)| plan)
}

/// Enumerate cue and duration alternatives before choosing their arrangement.
/// These are timing-feasible skeletons, not admitted tactics. The measured
/// selector must validate every eventual lane/vocal/effect schedule.
pub fn tactic_stitch_candidates(
    store: &Store,
    request: &StitchRequest<'_>,
    policy: &MixingConfig,
    limit: usize,
    cancel: &Cancellation,
) -> Result<Vec<MixPlan>> {
    require(
        (1..=64).contains(&limit),
        "TACTIC_BUDGET",
        "candidate budget must be 1..64",
    )?;
    let a = load_analyzed(store, store.track(request.from)?)?;
    let b = load_analyzed(store, store.track(request.to)?)?;
    require(
        request.from != request.to && a.track.audio_hash != b.track.audio_hash,
        "DUPLICATE_AUDIO",
        "choose two different recordings",
    )?;
    require(
        !a.overrides.operations.iter().any(
            |o| matches!(o,Correction::ExcludeTransition{to_track_id} if to_track_id==request.to),
        ),
        "STITCH_EXCLUDED",
        "a correction excludes this transition",
    )?;
    let af = ReturnFeatures::read(store, &a, cancel)?;
    let bf = ReturnFeatures::read(store, &b, cancel)?;
    let ap = phrase::infer_phrase_positions(&a.beats, &af.frames);
    let bp = phrase::infer_phrase_positions(&b.beats, &bf.frames);
    let cached = |track: &Analyzed| -> Result<Option<TacticTrackAnalysis>> {
        track
            .analysis
            .artifacts
            .get("tactic_analysis")
            .map(|r| store.read_ref(r))
            .transpose()
    };
    let av = cached(&a)?;
    let bv = cached(&b)?;
    let return_frames = |r: Option<&TacticTrackAnalysis>| -> BTreeSet<u64> {
        r.map(crate::transition_endpoints::arrivals)
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.frame)
            .collect()
    };
    let a_returns = return_frames(av.as_ref());
    let b_returns = return_frames(bv.as_ref());

    let quiet = |record: Option<&TacticTrackAnalysis>| {
        record
            .and_then(|r| {
                r.vocal_evidence.get("track").map(|track| {
                    crate::cached_tactic_evidence::source_quiet_spans(
                        track,
                        r.source_sample_rate_hz,
                    )
                })
            })
            .unwrap_or_default()
    };
    let aq = quiet(av.as_ref());
    let bq = quiet(bv.as_ref());
    let summary = |features: &ReturnFeatures,
                   p: &Passage,
                   record: Option<&TacticTrackAnalysis>,
                   quiet: &[(u64, u64)]| {
        let frames = features.between(p.start, p.end);
        let norm = frames.iter().map(|f| f.rms).sum::<f64>().max(1e-9);
        let chroma: [f64; 12] =
            std::array::from_fn(|c| frames.iter().map(|f| f.chroma[c] * f.rms).sum::<f64>() / norm);
        let guard = (0.17 * features.rate).ceil() as u64;
        let gaps: Vec<_> = p
            .downbeats
            .iter()
            .map(|&beat| {
                let frame = p.beat_frames[beat];
                quiet
                    .iter()
                    .any(|&(a, b)| a <= frame.saturating_sub(guard) && b >= frame + guard)
            })
            .collect();
        (
            chroma,
            features.rms(p.start, p.end),
            crate::tactic_cues::vocal_window_supported(record, p.start, p.end),
            gaps,
        )
    };
    let meters = |track: &Analyzed| {
        let ids: BTreeSet<_> = track.beats.downbeat_ids.iter().collect();
        let indices: Vec<_> = track
            .beats
            .events
            .iter()
            .enumerate()
            .filter(|(_, b)| ids.contains(&b.event_id))
            .map(|(i, _)| i)
            .collect();
        indices
            .windows(2)
            .map(|w| w[1] - w[0])
            .filter(|m| (2..=12).contains(m))
            .collect::<BTreeSet<_>>()
    };
    let am = meters(&a);
    let bm = meters(&b);
    let lengths: BTreeSet<_> = am
        .intersection(&bm)
        .flat_map(|meter| [1usize, 2, 4, 8, 16].map(|bars| bars * meter))
        .collect();
    let per_length = (limit / lengths.len().max(1)).max(1);
    let mut choices = Vec::new();
    let mut return_choices = Vec::new();
    // Include short exits and long holds in each evidenced common meter.
    for overlap in lengths {
        let outgoing = passages_for_clock(
            &a,
            overlap,
            request.context_beats,
            true,
            false,
            false,
            cancel,
        )?;
        for incoming_overlap in [overlap, overlap * 2, overlap / 2] {
            if !(2..=256).contains(&incoming_overlap) {
                continue;
            }
            let incoming = passages_for_clock(
                &b,
                incoming_overlap,
                request.context_beats,
                false,
                false,
                false,
                cancel,
            )?;
            let mut ranked = Vec::new();
            let bs: Vec<_> = incoming
                .iter()
                .map(|p| summary(&bf, p, bv.as_ref(), &bq))
                .collect();
            for pa in outgoing
                .iter()
                .filter(|p| p.start >= request.outgoing_after_frame)
            {
                cancel.check()?;
                let (ac, ar, a_voice, a_gaps) = summary(&af, pa, av.as_ref(), &aq);
                for (pb, (bc, br, b_voice, b_gaps)) in incoming.iter().zip(&bs) {
                    let Some((duration, deviation, skew)) = mapped_pair_timing(pa, pb, policy)
                    else {
                        continue;
                    };
                    if duration > 120. || overlap % pa.bar_beats != 0 {
                        continue;
                    }
                    let harmony = ac.iter().zip(bc).map(|(a, b)| (a - b).abs()).sum::<f64>() / 2.;
                    let level = (ar - br).abs() / ar.max(*br).max(1e-9);
                    let boundary_penalty = 0.4
                        * f64::from(!ap.get(&pa.end).is_some_and(|p| p.boundary))
                        + 0.4 * f64::from(!bp.get(&pb.start).is_some_and(|p| p.boundary))
                        + 0.2 * f64::from(!ap.get(&pa.start).is_some_and(|p| p.boundary));
                    // Independent stem gates can finish A's line and admit B
                    // later. Whole-window transcription is not a prerequisite
                    // for considering that arrangement. Keep structural phrase
                    // quality in the cost; a simultaneous quiet downbeat must
                    // not override it and flood the shortlist with cuts.
                    let a_gate = a_gaps.iter().skip(4).any(|gap| *gap);
                    let b_gate = b_gaps.iter().skip(4).any(|gap| *gap);
                    let vocal_penalty =
                        0.8 * (f64::from(!a_voice && !a_gate) + f64::from(!*b_voice && !b_gate));
                    let evidence_penalty = boundary_penalty + vocal_penalty;
                    let cost = 4. * skew
                        + deviation
                        + 0.3 * harmony
                        + 0.2 * level
                        + evidence_penalty
                        + 0.12 * pb.start as f64 / b.track.format.frames as f64;
                    ranked.push((cost, pa.start, pb.start, pa.bar_beats));
                }
            }
            ranked.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
            // Give distinct parts of both recordings a turn before keeping
            // several nearly interchangeable variants of the same cues.
            // Keep the best measured return alongside ordinary cue ranking.
            // Otherwise a slightly cheaper interior beat can consume the quota.
            let return_choice = ranked
                .iter()
                .find(|(_, a, b, _)| a_returns.contains(a) || b_returns.contains(b))
                .copied();
            let mut regions = BTreeSet::new();
            let (distinct, nearby): (Vec<_>, Vec<_>) =
                ranked.into_iter().partition(|&(_, a_frame, b_frame, _)| {
                    regions.insert((
                        a_frame / (30 * u64::from(a.track.format.sample_rate_hz)),
                        b_frame / (30 * u64::from(b.track.format.sample_rate_hz)),
                    ))
                });
            let mut selected: Vec<(u64, u64)> = Vec::new();
            if let Some((cost, outgoing, incoming, meter)) = return_choice {
                return_choices.push((overlap, cost, outgoing, incoming, meter, incoming_overlap));
            }
            for (cost, outgoing, incoming, meter) in distinct.into_iter().chain(nearby) {
                let separation = u64::from(a.track.format.sample_rate_hz) * 2;
                if selected.iter().any(|&(x, y)| {
                    x.abs_diff(outgoing) < separation && y.abs_diff(incoming) < separation
                }) {
                    continue;
                }
                selected.push((outgoing, incoming));
                choices.push((overlap, cost, outgoing, incoming, meter, incoming_overlap));
                if selected.len() >= per_length {
                    break;
                }
            }
        }
    }
    // Per-duration quotas keep plentiful short windows from hiding long
    // mashups before their selected-layer evidence can be evaluated.
    choices.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)).then(a.2.cmp(&b.2)));
    let (ordinary, mapped): (Vec<_>, Vec<_>) = choices.into_iter().partition(|c| c.0 == c.5);
    let mut ordinary = distribute_tactic_durations(ordinary).into_iter();
    let mut mapped = distribute_tactic_durations(mapped).into_iter();
    let mut choices = Vec::new();
    while choices.len() < limit {
        let next = if choices.len() % 3 == 2 {
            mapped.next().or_else(|| ordinary.next())
        } else {
            ordinary.next().or_else(|| mapped.next())
        };
        if let Some(c) = next {
            choices.push(c)
        } else {
            break;
        }
    }
    // Return probes have their own small quota. They must not evict the
    // ordinary entries from which long dense continuations were discovered.
    let return_keys = append_return_probes(&mut choices, return_choices, (limit / 3).max(1));
    let mut result = Vec::new();
    for (overlap, proposal_cost, outgoing, incoming, meter, incoming_overlap) in choices {
        let pinned = StitchRequest {
            overlap_beats: overlap,
            ..*request
        };
        let (mut plan, _) = match build_stitch_from_tracks(
            store,
            [&a, &b],
            &pinned,
            policy,
            None,
            None,
            StitchStyle::PinnedTactic {
                outgoing,
                incoming,
                incoming_overlap,
            },
            cancel,
        ) {
            Ok(plan) => plan,
            Err(error)
                if matches!(
                    error.code.as_str(),
                    "STITCH_INFEASIBLE" | "TIME_MAP_SPEED" | "TIME_MAP_ACCELERATION"
                ) =>
            {
                continue
            }
            Err(error) => return Err(error),
        };
        plan.header.extensions.insert("tactic_cue_candidate".into(),serde_json::json!({
            "outgoing_source_frame":outgoing,"incoming_source_frame":incoming,"overlap_beats":overlap,
            "incoming_overlap_beats":incoming_overlap,"pulse_ratio":incoming_overlap as f64/overlap as f64,
            "timing_basis":"bars", "bar_beats":meter, "proposal_cost":proposal_cost, "entry_return_probe":return_keys.contains(&(overlap,outgoing,incoming,incoming_overlap))
        }));
        result.push(plan);
    }
    let mut alternative = crate::tactic_cues::structural_candidates(
        store,
        request,
        policy,
        (limit / 4).max(1),
        cancel,
    )?;
    if policy
        .header
        .extensions
        .get("allow_expressive_tactics")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        alternative.extend(independent_pulse_candidates(
            &a,
            &b,
            request,
            policy,
            (limit / 4).max(1),
            cancel,
        )?);
    }
    alternative.truncate(limit);
    let (returns, mut result): (Vec<_>, Vec<_>) = result.into_iter().partition(|p| {
        p.header
            .extensions
            .get("tactic_cue_candidate")
            .and_then(|v| v.get("entry_return_probe"))
            .and_then(serde_json::Value::as_bool)
            == Some(true)
    });
    result.truncate(limit.saturating_sub(alternative.len()));
    result.extend(alternative);
    result.extend(returns);
    Ok(result)
}

type TacticCueChoice = (usize, f64, u64, u64, usize, usize);

/// Keep a promising entry alive and ask where the music gives it an ending.
/// Event endpoints are proposals, never authorization to bypass admission.
pub fn tactic_continuations(
    store: &Store,
    request: &StitchRequest<'_>,
    policy: &MixingConfig,
    seed: &MixPlan,
    limit: usize,
    cancel: &Cancellation,
) -> Result<Vec<MixPlan>> {
    if limit == 0 {
        return Ok(vec![]);
    }
    let a = load_analyzed(store, store.track(request.from)?)?;
    let b = load_analyzed(store, store.track(request.to)?)?;
    let tracks = [&a, &b];
    let transition = &seed.transitions[0];
    let starts: [u64; 2] = std::array::from_fn(|i| {
        seed.placements[i]
            .time_map
            .source_at(transition.start_output_frame as f64)
            .round() as u64
    });
    let index = |track: &Analyzed, frame: u64| {
        track
            .beats
            .events
            .iter()
            .enumerate()
            .filter(|(_, b)| {
                b.source_frame.abs_diff(frame)
                    <= u64::from(track.track.format.sample_rate_hz) * 8 / 100
            })
            .min_by_key(|(_, b)| b.source_frame.abs_diff(frame))
            .map(|(i, _)| i)
    };
    let Some(indices) = index(&a, starts[0]).zip(index(&b, starts[1])) else {
        return Ok(vec![]);
    };
    let indices = [indices.0, indices.1];
    let initial: [usize; 2] = std::array::from_fn(|i| {
        index(
            tracks[i],
            seed.placements[i]
                .time_map
                .source_at(transition.end_output_frame as f64)
                .round() as u64,
        )
        .unwrap_or(indices[i])
        .saturating_sub(indices[i])
    });
    if initial.contains(&0) {
        return Ok(vec![]);
    }
    let mut endpoints = vec![];
    for (side, track) in tracks.iter().enumerate() {
        let Some(reference) = track.analysis.artifacts.get("tactic_analysis") else {
            continue;
        };
        let record: TacticTrackAnalysis = store.read_ref(reference)?;
        for event in crate::transition_endpoints::measured(&record) {
            let Some(at) = index(track, event.frame) else {
                continue;
            };
            let count = at.saturating_sub(indices[side]);
            if count <= initial[side] || count > 256 {
                continue;
            }
            let numerator = count * initial[1 - side];
            if numerator % initial[side] != 0 {
                continue;
            }
            let other = numerator / initial[side];
            if !(2..=256).contains(&other) {
                continue;
            }
            let lengths = if side == 0 {
                [count, other]
            } else {
                [other, count]
            };
            endpoints.push((lengths, side, event));
        }
    }
    endpoints.sort_by_key(|(counts, _, _)| counts[0]);
    endpoints.dedup_by_key(|(counts, _, _)| *counts);
    // Probe both the nearest musical change and the longest feasible horizon.
    let mut pending: std::collections::VecDeque<_> = endpoints.into();
    let mut endpoints = vec![];
    while let Some(first) = pending.pop_front() {
        endpoints.push(first);
        if let Some(last) = pending.pop_back() {
            endpoints.push(last);
        }
    }

    let mut result = vec![];
    for (counts, side, endpoint) in endpoints {
        cancel.check()?;
        let pinned = StitchRequest {
            overlap_beats: counts[0],
            ..*request
        };
        let (mut plan, _) = match build_stitch_from_tracks(
            store,
            [&a, &b],
            &pinned,
            policy,
            None,
            None,
            StitchStyle::PinnedTactic {
                outgoing: starts[0],
                incoming: starts[1],
                incoming_overlap: counts[1],
            },
            cancel,
        ) {
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
        if (plan.transitions[0].end_output_frame - plan.transitions[0].start_output_frame) as f64
            / f64::from(plan.output.sample_rate_hz)
            > MAX_TACTIC_CONTINUATION_SECONDS
        {
            continue;
        }
        plan.header.extensions.insert("tactic_cue_candidate".into(),serde_json::json!({
            "outgoing_source_frame":starts[0],"incoming_source_frame":starts[1],
            "overlap_beats":counts[0],"incoming_overlap_beats":counts[1],"timing_basis":"bars",
            "endpoint":{"placement":side,"source_frame":endpoint.frame,"reason":endpoint.reason},
        }));
        result.push(plan);
        if result.len() >= limit {
            break;
        }
    }
    Ok(result)
}

fn append_return_probes(
    choices: &mut Vec<TacticCueChoice>,
    mut return_choices: Vec<TacticCueChoice>,
    limit: usize,
) -> BTreeSet<(usize, u64, u64, usize)> {
    return_choices.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
    let key = |c: &TacticCueChoice| (c.0, c.2, c.3, c.5);
    let existing: BTreeSet<_> = choices.iter().map(key).collect();
    let returns: Vec<_> = distribute_tactic_durations(return_choices)
        .into_iter()
        .filter(|c| !existing.contains(&key(c)))
        .take(limit)
        .collect();
    let return_keys: BTreeSet<_> = returns.iter().map(key).collect();
    choices.extend(returns);
    return_keys
}

fn distribute_tactic_durations(choices: Vec<TacticCueChoice>) -> Vec<TacticCueChoice> {
    // Preserve ranking within each duration, but give every evidenced duration
    // a turn before taking another short candidate. A global cost truncation
    // otherwise discards the long windows before tactic admission can see them.
    let mut durations =
        std::collections::BTreeMap::<usize, std::collections::VecDeque<TacticCueChoice>>::new();
    for choice in choices {
        durations.entry(choice.0).or_default().push_back(choice);
    }
    let mut result = Vec::new();
    loop {
        let before = result.len();
        for queue in durations.values_mut() {
            if let Some(choice) = queue.pop_front() {
                result.push(choice);
            }
        }
        if result.len() == before {
            return result;
        }
    }
}

fn independent_pulse_candidates(
    a: &Analyzed,
    b: &Analyzed,
    request: &StitchRequest<'_>,
    policy: &MixingConfig,
    limit: usize,
    cancel: &Cancellation,
) -> Result<Vec<MixPlan>> {
    let mut windows = Vec::new();
    let lengths = |track: &Analyzed| {
        let accents: BTreeSet<_> = track.beats.downbeat_ids.iter().collect();
        let indices: Vec<_> = track
            .beats
            .events
            .iter()
            .enumerate()
            .filter(|(_, beat)| accents.contains(&beat.event_id))
            .map(|(i, _)| i)
            .collect();
        indices
            .windows(2)
            .map(|w| w[1] - w[0])
            .filter(|meter| (2..=12).contains(meter))
            .flat_map(|meter| [2usize, 3, 4, 6, 8, 12, 16].map(|bars| bars * meter))
            .filter(|beats| *beats <= 192)
            .collect::<BTreeSet<_>>()
    };
    let al = lengths(a);
    let bl = lengths(b);
    for na in al {
        let aa = passages(a, na, request.context_beats, true, false, cancel)?;
        for &nb in &bl {
            if na == nb {
                continue;
            }
            let bb = passages(b, nb, request.context_beats, false, false, cancel)?;
            for pa in aa
                .iter()
                .filter(|p| p.start >= request.outgoing_after_frame)
            {
                cancel.check()?;
                for pb in &bb {
                    let duration = (pa.seconds + pb.seconds) * 0.5;
                    let deviation = (pa.seconds / duration - 1.).abs();
                    let ratio = nb as f64 / na as f64 * pa.seconds / pb.seconds;
                    if deviation > policy.max_speed_deviation || !(0.25..=4.).contains(&ratio) {
                        continue;
                    }
                    windows.push((deviation, (pa.start, pa.end), (pb.start, pb.end)));
                }
            }
        }
    }
    windows.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut result = Vec::new();
    let mut selected: Vec<(u64, u64)> = Vec::new();
    for (_, aw, bw) in windows {
        if selected.iter().any(|&(x, y)| {
            x.abs_diff(aw.0) < a.track.format.sample_rate_hz as u64 * 2
                && y.abs_diff(bw.0) < b.track.format.sample_rate_hz as u64 * 2
        }) {
            continue;
        }
        if let Some(plan) = crate::tactic_cues::native_pair(
            [a, b],
            [aw, bw],
            request.context_beats,
            policy,
            "independent_pulses",
        )? {
            result.push(plan);
            selected.push((aw.0, bw.0));
        }
        if result.len() >= limit {
            break;
        }
    }
    Ok(result)
}

#[derive(Debug)]
pub struct DrumHandoff {
    pub output_frame: u64,
    pub source_frame: u64,
    pub vocal_bars: Option<Vec<u64>>,
}

/// Search for the longest feasible band exchange. Never substitute a broadband fade.
pub fn band_stitch(
    store: &Store,
    request: &StitchRequest<'_>,
    policy: &MixingConfig,
    cancel: &Cancellation,
) -> Result<MixPlan> {
    find_band_stitch(store, request, policy, &[16, 12, 8], false, cancel)?.ok_or_else(|| {
        Error::new(
            "STITCH_INFEASIBLE",
            "no aligned eight-beat or longer band exchange",
        )
    })
}

/// Prefer a band exchange only when there is room to hear the individual moves.
pub fn preferred_band_stitch(
    store: &Store,
    request: &StitchRequest<'_>,
    policy: &MixingConfig,
    cancel: &Cancellation,
) -> Result<Option<MixPlan>> {
    find_band_stitch(store, request, policy, &[16, 12], true, cancel)
}

fn find_band_stitch(
    store: &Store,
    request: &StitchRequest<'_>,
    policy: &MixingConfig,
    lengths: &[usize],
    require_space: bool,
    cancel: &Cancellation,
) -> Result<Option<MixPlan>> {
    for &overlap_beats in lengths {
        let candidate = StitchRequest {
            overlap_beats,
            ..*request
        };
        match build_stitch(
            store,
            &candidate,
            policy,
            None,
            None,
            StitchStyle::Bands { require_space },
            cancel,
        ) {
            Ok((plan, _)) => return Ok(Some(plan)),
            Err(error) if error.code == "STITCH_INFEASIBLE" => {}
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

fn apply_band_exchange(state: &mut State, tracks: [&Analyzed; 2], output_beats: &[u64]) {
    let transition = &state.transitions[0];
    let start = transition.start_output_frame;
    let end = transition.end_output_frame;
    let beat_count = output_beats.len() - 1;
    let output_at = |beat: usize| output_beats[beat];
    let boundaries = [
        (0, [0., 0., 0.]),
        (beat_count / 4, [0., 0., 1.]),
        (beat_count / 2 - 1, [0., 0., 1.]),
        (beat_count / 2 + 1, [1., 0., 1.]),
        (3 * beat_count / 4, [1., 0., 1.]),
        (beat_count, [1., 1., 1.]),
    ];
    for (index, placement) in state.placements.iter_mut().enumerate() {
        placement
            .automation
            .retain(|point| point.output_frame < start || point.output_frame > end);
        let mut points = BTreeMap::new();
        for (beat, incoming) in boundaries {
            let bands = if index == 0 {
                incoming.map(|value| 1. - value)
            } else {
                incoming
            };
            points.insert(
                output_at(beat),
                AutomationPoint {
                    output_frame: output_at(beat),
                    gain_linear: gain(tracks[index]),
                    bands_linear: bands,
                    mute: false,
                },
            );
        }
        placement.automation.extend(points.into_values());
        placement
            .automation
            .sort_unstable_by_key(|point| point.output_frame);
    }
}

pub fn drum_stitch(
    store: &Store,
    request: &StitchRequest<'_>,
    policy: &MixingConfig,
    lead_bars: usize,
    cancel: &Cancellation,
) -> Result<(MixPlan, DrumHandoff)> {
    let (plan, handoff) = build_stitch(
        store,
        request,
        policy,
        Some(lead_bars),
        None,
        StitchStyle::Basic,
        cancel,
    )?;
    Ok((plan, handoff.expect("drum stitch must select a handoff")))
}

pub struct BeatReturnCue {
    pub outgoing_near: f64,
    pub incoming_cut_near: Option<f64>,
    pub vocal_tail_until: Option<f64>,
    pub auto_vocal_handoff: bool,
}

pub fn beat_return_stitch(
    store: &Store,
    request: &StitchRequest<'_>,
    policy: &MixingConfig,
    cue: BeatReturnCue,
    cancel: &Cancellation,
) -> Result<(MixPlan, DrumHandoff)> {
    require(
        !cue.auto_vocal_handoff || cue.vocal_tail_until.is_none(),
        "STITCH_VOCAL_MODE",
        "choose either automatic vocal handoff or a manual vocal tail",
    )?;
    require(
        std::iter::once(cue.outgoing_near)
            .chain(cue.incoming_cut_near)
            .chain(cue.vocal_tail_until)
            .all(|time| time.is_finite() && time >= 0.),
        "STITCH_RETURN_CUE",
        "use a nonnegative cue time",
    )?;
    let (plan, handoff) = build_stitch(
        store,
        request,
        policy,
        None,
        Some(cue),
        StitchStyle::Basic,
        cancel,
    )?;
    Ok((
        plan,
        handoff.expect("beat return stitch must select a handoff"),
    ))
}

#[derive(Clone, Copy)]
struct BeatReturnSpan {
    source_start: u64,
    overlap: usize,
    fade_beat: Option<usize>,
    vocal_bars: Option<[usize; 7]>,
}

struct ReturnFeatures {
    frames: Vec<FeatureFrame>,
    rate: f64,
}

impl ReturnFeatures {
    fn read(store: &Store, track: &Analyzed, cancel: &Cancellation) -> Result<Self> {
        let harmony: Harmony = store.read_ref(&track.analysis.artifacts["harmony"])?;
        let mut frames = Vec::new();
        for reference in &harmony.feature_chunks {
            cancel.check()?;
            let chunk: FeatureChunk = store.read_ref(reference)?;
            frames.extend(chunk.frames);
        }
        frames.sort_unstable_by_key(|f| f.source_frame);
        Ok(Self {
            frames,
            rate: f64::from(track.track.format.sample_rate_hz),
        })
    }

    fn between(&self, start: u64, end: u64) -> &[FeatureFrame] {
        let lo = self.frames.partition_point(|f| f.source_frame < start);
        let hi = self.frames.partition_point(|f| f.source_frame < end);
        &self.frames[lo..hi]
    }

    fn is_return(&self, beats: &[Beat]) -> bool {
        let energy = |beat: &Beat| {
            let start = beat.source_frame.saturating_sub((0.04 * self.rate) as u64);
            let end = beat.source_frame + (0.22 * self.rate) as u64;
            self.between(start, end)
                .iter()
                .map(|f| f.band_energy[0])
                .fold(0., f64::max)
        };
        let before = energy(&beats[0]).max(energy(&beats[1]));
        let after = energy(&beats[2]).min(energy(&beats[3]));
        after >= (4. * before).max(0.00002)
    }

    fn bass_rms(&self, start: u64, end: u64) -> f64 {
        let frames = self.between(start, end);
        (frames.iter().map(|f| f.band_energy[0]).sum::<f64>() / frames.len().max(1) as f64).sqrt()
    }

    fn rms(&self, start: u64, end: u64) -> f64 {
        let frames = self.between(start, end);
        (frames.iter().map(|f| f.rms * f.rms).sum::<f64>() / frames.len().max(1) as f64).sqrt()
    }

    fn low_band_share(&self, start: u64, end: u64) -> Option<f64> {
        let frames = self.between(start, end);
        let low = frames.iter().map(|f| f.band_energy[0]).sum::<f64>();
        let total = frames.iter().flat_map(|f| f.band_energy).sum::<f64>();
        (total > 1e-12).then(|| low / total)
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum SegmentRole {
    Rest,
    DrumLeadBed,
    SustainedTexture,
    DrumReturn,
    DrumGroove,
    Mixed,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct BarTile {
    pub start_frame: u64,
    pub end_frame: u64,
    pub bars: usize,
    pub bpm: f64,
    pub relative_level: f64,
    pub relative_bass: f64,
    pub onset: f64,
    pub chroma: [f64; 12],
    pub relative_onset: f64,
    pub level_change: f64,
    pub mashup_bed: bool,
    pub mashup_drums: bool,
    role: SegmentRole,
    phrase: Option<phrase::PhrasePosition>,
}

struct TileProfile<'a> {
    features: &'a ReturnFeatures,
    rms: f64,
    bass: f64,
    onset: f64,
}

impl<'a> TileProfile<'a> {
    fn new(track: &Analyzed, features: &'a ReturnFeatures) -> Self {
        Self {
            features,
            rms: track.analysis.rms.max(1e-6),
            bass: features.bass_rms(0, track.track.format.frames).max(1e-6),
            onset: mean_onset(&features.frames).max(1e-6),
        }
    }

    fn bar_tiles(&self, beat_frames: &[u64], meter: usize) -> Vec<BarTile> {
        beat_frames
            .iter()
            .step_by(meter)
            .collect::<Vec<_>>()
            .windows(2)
            .map(|frames| self.tile(*frames[0], *frames[1], 1, meter))
            .collect()
    }

    fn tile(&self, start_frame: u64, end_frame: u64, bars: usize, beats: usize) -> BarTile {
        let features = self.features.between(start_frame, end_frame);
        let mut chroma = [0.; 12];
        for frame in features {
            for (sum, value) in chroma.iter_mut().zip(frame.chroma) {
                *sum += value * frame.rms;
            }
        }
        let total: f64 = chroma.iter().sum();
        if total > 0. {
            chroma.iter_mut().for_each(|value| *value /= total);
        }
        let onset = mean_onset(features);
        let relative_onset = onset / self.onset;
        let relative_level = self.features.rms(start_frame, end_frame) / self.rms;
        let relative_bass = self.features.bass_rms(start_frame, end_frame) / self.bass;
        let previous = start_frame.saturating_sub((end_frame - start_frame) / bars as u64);
        let level_change = if previous < start_frame {
            relative_level - self.features.rms(previous, start_frame) / self.rms
        } else {
            0.
        };
        let role = if relative_level < 0.30 {
            SegmentRole::Rest
        } else if relative_bass < 0.58 && relative_onset <= 0.80 {
            SegmentRole::DrumLeadBed
        } else if relative_level >= 0.65 && relative_bass >= 0.55 && relative_onset >= 0.85 {
            if level_change >= 0.28 {
                SegmentRole::DrumReturn
            } else {
                SegmentRole::DrumGroove
            }
        } else if onset <= 0.005 {
            SegmentRole::SustainedTexture
        } else {
            SegmentRole::Mixed
        };
        BarTile {
            start_frame,
            end_frame,
            bars,
            bpm: 60. * beats as f64 * self.features.rate / (end_frame - start_frame) as f64,
            relative_level,
            relative_bass,
            onset,
            chroma,
            relative_onset,
            level_change,
            mashup_bed: (0.30..=1.20).contains(&relative_level)
                && onset <= 0.005
                && relative_onset <= 1.05,
            mashup_drums: relative_level >= 0.65
                && relative_bass >= 0.55
                && onset >= 0.006
                && relative_onset >= 0.85,
            role,
            phrase: None,
        }
    }
}

fn mean_onset(frames: &[FeatureFrame]) -> f64 {
    frames.iter().map(|f| f.onset).sum::<f64>() / frames.len().max(1) as f64
}

fn harmonic_distance(a: &BarTile, b: &BarTile) -> f64 {
    a.chroma
        .iter()
        .zip(b.chroma)
        .map(|(a, b)| (a - b).abs())
        .sum::<f64>()
        * 0.5
}

// A window must remain usable in every bar; an average must not hide a
// returning drum hit, a break in the incoming groove, or a harmonic change.
fn mashup_capabilities(bars: &[BarTile]) -> (bool, bool) {
    let stable = bars.windows(2).all(|pair| {
        (pair[0].relative_level - pair[1].relative_level).abs() <= 0.30
            && harmonic_distance(&pair[0], &pair[1]) <= 0.20
    });
    (
        stable && bars.iter().all(|bar| bar.mashup_bed),
        stable && bars.iter().all(|bar| bar.mashup_drums),
    )
}

fn mashup_compatibility(outgoing: &[BarTile], incoming: &[BarTile]) -> Option<f64> {
    mashup_compatibility_for_bars(outgoing, incoming, MASHUP_VERIFIED_BARS)
}

fn mashup_compatibility_for_bars(
    outgoing: &[BarTile],
    incoming: &[BarTile],
    bar_count: usize,
) -> Option<f64> {
    if outgoing.len() < bar_count
        || incoming.len() < bar_count
        || !mashup_capabilities(&outgoing[..bar_count]).0
        || !mashup_capabilities(&incoming[..bar_count]).1
    {
        return None;
    }
    let mut harmonic = 0.;
    for (a, b) in outgoing[..bar_count].iter().zip(&incoming[..bar_count]) {
        let distance = harmonic_distance(a, b);
        if distance > 0.28 || b.onset < 1.4 * a.onset {
            return None;
        }
        harmonic += distance / bar_count as f64;
    }
    (harmonic <= 0.23).then_some(harmonic)
}

pub fn describe_bar_tiles(
    store: &Store,
    track: Track,
    cancel: &Cancellation,
) -> Result<Vec<BarTile>> {
    let track = load_analyzed(store, track)?;
    let features = ReturnFeatures::read(store, &track, cancel)?;
    let profile = TileProfile::new(&track, &features);
    let phrase_positions = phrase::infer_phrase_positions(&track.beats, &features.frames);
    let beats = &track.beats.events;
    let accents: BTreeSet<_> = track.beats.downbeat_ids.iter().collect();
    let bars: Vec<_> = beats
        .iter()
        .enumerate()
        .filter_map(|(i, beat)| accents.contains(&beat.event_id).then_some(i))
        .collect();
    let mut tiles = vec![];
    for (i, pair) in bars.windows(2).enumerate() {
        cancel.check()?;
        let meter = pair[1] - pair[0];
        if !(2..=12).contains(&meter) {
            continue;
        }
        for count in 1..=4 {
            let Some(&end) = bars.get(i + count) else {
                break;
            };
            if end - pair[0] != count * meter
                || bars[i..=i + count].windows(2).any(|w| w[1] - w[0] != meter)
            {
                break;
            }
            let (start_frame, end_frame) = (beats[pair[0]].source_frame, beats[end].source_frame);
            if beats[pair[0]..=end].iter().any(|beat| beat.score < 0.5)
                || track
                    .beats
                    .unreliable_intervals
                    .iter()
                    .any(|r| r.start_frame < end_frame && r.end_frame > start_frame)
            {
                break;
            }
            let mut tile = profile.tile(start_frame, end_frame, count, count * meter);
            let bar_tiles: Vec<_> = bars[i..=i + count]
                .windows(2)
                .map(|pair| {
                    profile.tile(
                        beats[pair[0]].source_frame,
                        beats[pair[1]].source_frame,
                        1,
                        meter,
                    )
                })
                .collect();
            (tile.mashup_bed, tile.mashup_drums) = mashup_capabilities(&bar_tiles);
            tile.phrase = phrase_positions.get(&start_frame).copied();
            tiles.push(tile);
        }
    }
    Ok(tiles)
}

fn vocal_window(beats: &[Beat], downbeats: &BTreeSet<&String>, start: usize) -> Option<[usize; 7]> {
    let mut after = beats[start + 1..]
        .iter()
        .enumerate()
        .filter_map(|(n, beat)| downbeats.contains(&beat.event_id).then_some(n + 1));
    let mut bars = [0; 7];
    for bar in &mut bars {
        *bar = after.next()?;
    }
    (bars[6] <= 64).then_some(bars)
}

fn beat_return(
    store: &Store,
    track: &Analyzed,
    near: f64,
    cue: &BeatReturnCue,
    cancel: &Cancellation,
) -> Result<BeatReturnSpan> {
    let features = ReturnFeatures::read(store, track, cancel)?;
    let beats = &track.beats.events;
    let rate = f64::from(track.track.format.sample_rate_hz);
    let downbeats: BTreeSet<_> = track.beats.downbeat_ids.iter().collect();
    for (i, window) in beats.windows(4).enumerate() {
        let beat = &window[2];
        if (beat.source_frame as f64 / rate - near).abs() > 0.8 {
            continue;
        }
        if !features.is_return(window) {
            continue;
        }
        if cue.auto_vocal_handoff {
            let bars = vocal_window(beats, &downbeats, i + 2).ok_or_else(|| {
                Error::new(
                    "STITCH_VOCAL_WINDOW",
                    "the beat grid needs six complete bars after the drum swap",
                )
            })?;
            return Ok(BeatReturnSpan {
                source_start: beat.source_frame,
                overlap: bars[6],
                fade_beat: None,
                vocal_bars: Some(bars),
            });
        }
        if let Some(until) = cue.vocal_tail_until {
            let Some(fade_beat) = beats[i + 3..]
                .iter()
                .position(|b| b.source_frame as f64 / rate >= until)
                .map(|n| n + 1)
            else {
                break;
            };
            require(
                until > beat.source_frame as f64 / rate
                    && fade_beat < 64
                    && i + 3 + fade_beat < beats.len(),
                "STITCH_VOCAL_TAIL",
                "vocal tail must end after the drum swap and leave one beat to fade",
            )?;
            return Ok(BeatReturnSpan {
                source_start: beat.source_frame,
                overlap: fade_beat + 1,
                fade_beat: Some(fade_beat),
                vocal_bars: None,
            });
        }
        if let Some(next_bar) = beats[i + 3..]
            .iter()
            .position(|b| downbeats.contains(&b.event_id))
        {
            return Ok(BeatReturnSpan {
                source_start: beat.source_frame,
                overlap: next_bar + 1,
                fade_beat: None,
                vocal_bars: None,
            });
        }
    }
    Err(Error::new(
        "STITCH_BEAT_RETURN",
        "no sustained return of low-band energy near the cue",
    ))
}

// Invert the monotone map within the two anchors bracketing a source beat.
fn output_at_beat(map: &TimeMap, frame: u64) -> f64 {
    let at = map.anchors.partition_point(|a| a.source_frame < frame);
    if map.anchors[at].source_frame == frame {
        return map.anchors[at].output_frame as f64;
    }
    let mut lo = map.anchors[at - 1].output_frame as f64;
    let mut hi = map.anchors[at].output_frame as f64;
    while hi - lo > 0.5 {
        let mid = (lo + hi) * 0.5;
        if map.source_at(mid) < frame as f64 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (lo + hi) * 0.5
}

fn passage_prefix(passage: &Passage, beats: usize) -> Passage {
    let start = passage.start;
    let end = passage.beat_frames[beats];
    let seconds = passage.seconds * (end - start) as f64 / (passage.end - start) as f64;
    let beat_frames = passage.beat_frames[..=beats].to_vec();
    let beat_phases: Vec<_> = beat_frames
        .iter()
        .map(|frame| (frame - start) as f64 / (end - start) as f64)
        .collect();
    let grid_error = beat_phases
        .iter()
        .enumerate()
        .map(|(i, phase)| (phase - i as f64 / beats as f64).abs() * seconds)
        .fold(0., f64::max);
    Passage {
        start,
        end,
        seconds,
        context: passage.context,
        beat_phases,
        beat_frames,
        downbeats: passage
            .downbeats
            .iter()
            .copied()
            .filter(|&beat| beat <= beats)
            .collect(),
        grid_error,
        bar_beats: passage.bar_beats,
        continued_meter: passage.continued_meter,
    }
}

fn pair_timing(a: &Passage, b: &Passage, policy: &MixingConfig) -> Option<(f64, f64, f64)> {
    let duration = (a.seconds + b.seconds) * 0.5;
    let deviation = (a.seconds / duration - 1.).abs();
    if a.downbeats != b.downbeats
        || a.bar_beats != b.bar_beats
        || deviation > policy.max_speed_deviation
    {
        return None;
    }
    let skew = a
        .beat_phases
        .iter()
        .zip(&b.beat_phases)
        .map(|(a, b)| (a - b).abs() * duration)
        .fold(0., f64::max);
    (skew <= MAX_BEAT_SKEW_SECONDS).then_some((duration, deviation, skew))
}

/// Timing feasibility only: alternative beat correspondence never widens the
/// playback speed limits. Admission still requires measured groove hierarchy.
fn mapped_pair_timing(a: &Passage, b: &Passage, policy: &MixingConfig) -> Option<(f64, f64, f64)> {
    let na = a.beat_phases.len().checked_sub(1)?;
    let nb = b.beat_phases.len().checked_sub(1)?;
    if na == nb {
        return pair_timing(a, b, policy);
    }
    if na == 0 || nb == 0 || !(na == 2 * nb || nb == 2 * na) || a.bar_beats != b.bar_beats {
        return None;
    }
    let duration = (a.seconds + b.seconds) * 0.5;
    let deviation = (a.seconds / duration - 1.)
        .abs()
        .max((b.seconds / duration - 1.).abs());
    if deviation > policy.max_speed_deviation {
        return None;
    }
    let count = na.min(nb);
    let skew = (0..=count)
        .map(|i| (a.beat_phases[i * na / count] - b.beat_phases[i * nb / count]).abs() * duration)
        .fold(0., f64::max);
    (skew <= MAX_BEAT_SKEW_SECONDS).then_some((duration, deviation, skew))
}

/// Ordered cue candidates. The separator still has to confirm a quiet vocal pocket.
pub fn automatic_cues(
    store: &Store,
    request: &StitchRequest<'_>,
    policy: &MixingConfig,
    cancel: &Cancellation,
) -> Result<Vec<BeatReturnCue>> {
    policy.validate()?;
    let from = load_analyzed(store, store.track(request.from)?)?;
    let to = load_analyzed(store, store.track(request.to)?)?;
    let features = ReturnFeatures::read(store, &from, cancel)?;
    let beats = &from.beats.events;
    let accents: BTreeSet<_> = from.beats.downbeat_ids.iter().collect();
    let track_rms = from.analysis.rms.max(1e-6);
    let mut returns = vec![];
    for (i, window) in beats.windows(4).enumerate() {
        cancel.check()?;
        let start = i + 2;
        if start < request.context_beats.max(12)
            || start + 4 >= beats.len()
            || !features.is_return(window)
        {
            continue;
        }
        let frame = beats[start].source_frame;
        if frame < request.outgoing_after_frame {
            continue;
        }
        let before = features.rms(beats[start - 4].source_frame, frame) / track_rms;
        let after = features.rms(frame, beats[start + 4].source_frame) / track_rms;
        if before > 0.65 || after < 0.8 {
            continue;
        }
        let early = features.rms(
            beats[start - 12].source_frame,
            beats[start - 4].source_frame,
        ) / track_rms;
        if let Some(bars) = vocal_window(beats, &accents, start) {
            // A return that builds through the break is a natural point to change drums.
            returns.push((after - before + 2. * (before - early), frame, bars));
        }
    }
    returns.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));

    let incoming_beats = &to.beats.events;
    let incoming_accents: BTreeSet<_> = to.beats.downbeat_ids.iter().collect();
    let downbeats: Vec<_> = incoming_beats
        .iter()
        .enumerate()
        .filter_map(|(i, beat)| incoming_accents.contains(&beat.event_id).then_some(i))
        .collect();
    let incoming_rate = f64::from(to.track.format.sample_rate_hz);
    let intro_limit = (to.track.format.frames as f64 * 0.25).min(incoming_rate * 45.);
    let mut candidates = vec![];
    for (_, frame, bars) in returns.into_iter().take(8) {
        let overlap = bars[6];
        let Some(a) = passages(&from, overlap, request.context_beats, true, true, cancel)?
            .into_iter()
            .find(|p| p.start == frame)
        else {
            continue;
        };
        // Prefer the earliest stable intro with two complete bars confirming its meter.
        for b in passages(&to, overlap, request.context_beats, false, true, cancel)? {
            let entry = b.beat_frames[bars[0]];
            if entry as f64 > intro_limit {
                break;
            }
            let beat = incoming_beats.partition_point(|beat| beat.source_frame < entry);
            let Ok(bar) = downbeats.binary_search(&beat) else {
                continue;
            };
            if bar < 2
                || downbeats[bar] - downbeats[bar - 1] != a.bar_beats
                || downbeats[bar - 1] - downbeats[bar - 2] != a.bar_beats
                || pair_timing(&a, &b, policy).is_none()
            {
                continue;
            }
            candidates.push(BeatReturnCue {
                outgoing_near: frame as f64 / features.rate,
                incoming_cut_near: Some(entry as f64 / incoming_rate),
                vocal_tail_until: None,
                auto_vocal_handoff: true,
            });
            if candidates.len() == 3 {
                return Ok(candidates);
            }
        }
    }
    Ok(candidates)
}

#[derive(Debug)]
pub struct ChainPick {
    pub analyzed_tracks: usize,
    pub orders: Vec<Vec<String>>,
    /// Basic-analysis shortlist only; stem/vocal admission still required.
    pub preparation_candidates: Vec<serde_json::Value>,
}

struct PoolWindow {
    passage: Passage,
    tile: BarTile,
    unsupported_vocals: bool,
    lead: Option<[SegmentRole; 2]>,
    mashup_bars: Vec<BarTile>,
}

fn pool_windows(
    track: &Analyzed,
    features: &ReturnFeatures,
    tactic_analysis: Option<&TacticTrackAnalysis>,
    overlap: usize,
    context: usize,
    outgoing: bool,
    cancel: &Cancellation,
) -> Result<Vec<PoolWindow>> {
    let profile = TileProfile::new(track, features);
    let phrases = phrase::infer_phrase_positions(&track.beats, &features.frames);
    Ok(passages(track, overlap, context, outgoing, false, cancel)?
        .into_iter()
        .filter(|p| {
            !outgoing
                || phrases
                    .get(&p.start)
                    .is_none_or(|position| position.boundary)
        })
        .map(|passage| {
            let tile = profile.tile(
                passage.start,
                passage.end,
                overlap / passage.bar_beats,
                overlap,
            );
            let lead = (passage.downbeats.len() >= 3).then(|| {
                [0, 1].map(|bar| {
                    let start = passage.beat_frames[bar * passage.bar_beats];
                    let end = passage.beat_frames[(bar + 1) * passage.bar_beats];
                    profile.tile(start, end, 1, passage.bar_beats).role
                })
            });
            let mashup_bars = if passage.bar_beats == 4 && overlap >= LINGER_BEATS {
                profile.bar_tiles(&passage.beat_frames[..=MASHUP_VERIFIED_BARS * 4], 4)
            } else {
                vec![]
            };
            PoolWindow {
                unsupported_vocals: tactic_analysis.is_some_and(|record| {
                    !crate::tactic_cues::vocal_window_supported(
                        Some(record),
                        passage.start,
                        passage.end,
                    )
                }),
                passage,
                tile,
                lead,
                mashup_bars,
            }
        })
        .collect())
}

// Compare the two bars that would be heard under the incoming drums.
fn drum_lead_preference(a: &PoolWindow, b: &PoolWindow, harmonic: f64) -> f64 {
    use SegmentRole::*;
    let (Some(a), Some(b)) = (a.lead, b.lead) else {
        return 0.;
    };
    if !b.iter().all(|role| matches!(role, DrumReturn | DrumGroove)) {
        return 0.;
    }
    match a {
        [DrumLeadBed, DrumLeadBed] => 0.035,
        [DrumLeadBed | SustainedTexture, DrumLeadBed | SustainedTexture] if harmonic <= 0.19 => {
            0.020
        }
        _ => 0.,
    }
}

#[derive(Clone, Copy)]
struct PoolEdge {
    exit: u64,
    entry_end: u64,
    score: f64,
}

/// Search the analyzed pool for ordered chains with compatible joins.
/// The actual stitch selector can use a richer transition at any of those joins.
fn preparation_bridge_cost(
    incoming: &PoolEdge,
    outgoing: &PoolEdge,
    pacing: &TrackPacing,
) -> Option<f64> {
    let decision = pacing.assess(incoming.entry_end, outgoing.exit)?;
    Some(
        incoming.score
            + outgoing.score
            + 0.75 * incoming.score.max(outgoing.score)
            + 0.005 * decision.penalty_quality_seconds,
    )
}

pub fn pick_four_tracks(
    store: &Store,
    policy: &MixingConfig,
    context: usize,
    cancel: &Cancellation,
) -> Result<ChainPick> {
    pick_tracks(store, policy, context, 4, cancel)
}

pub fn pick_tracks(
    store: &Store,
    policy: &MixingConfig,
    context: usize,
    track_count: usize,
    cancel: &Cancellation,
) -> Result<ChainPick> {
    pick_tracks_in_pool(store, policy, context, track_count, false, false, cancel)
}

/// Restrict a route search to recordings with full tactic evidence when the
/// caller has no analyzer configured to prepare missing evidence.
pub fn pick_prepared_tracks(
    store: &Store,
    policy: &MixingConfig,
    context: usize,
    track_count: usize,
    cancel: &Cancellation,
) -> Result<ChainPick> {
    pick_tracks_in_pool(store, policy, context, track_count, true, false, cancel)
}

/// Preparation needs only prepared↔unprepared connections, not a speculative
/// full route through every unanalyzed candidate in the library.
pub fn shortlist_preparation(
    store: &Store,
    policy: &MixingConfig,
    context: usize,
    count: usize,
    cancel: &Cancellation,
) -> Result<ChainPick> {
    pick_tracks_in_pool(store, policy, context, count, false, true, cancel)
}

#[allow(clippy::too_many_arguments)]
fn pick_tracks_in_pool(
    store: &Store,
    policy: &MixingConfig,
    context: usize,
    track_count: usize,
    prepared_only: bool,
    preparation_only: bool,
    cancel: &Cancellation,
) -> Result<ChainPick> {
    require(
        (3..=8).contains(&track_count),
        "CHAIN_TRACKS",
        "choose three to eight tracks",
    )?;
    policy.validate()?;
    require(
        (1..=64).contains(&context),
        "STITCH_BEATS",
        "choose 1–64 context beats",
    )?;
    let tracks = store
        .tracks()?
        .into_iter()
        .filter(|t| matches!(t.lifecycle, Lifecycle::Available) && t.active_analysis.is_some())
        .collect();
    let mut pool = Vec::new();
    for track in crate::unique_song_pool(store, tracks)? {
        cancel.check()?;
        let analyzed = load_analyzed(store, track)?;
        if !prepared_only || analyzed.analysis.artifacts.contains_key("tactic_analysis") {
            pool.push(analyzed);
        }
    }
    require(
        pool.len() >= track_count,
        "CHAIN_POOL",
        format!("analyze at least {track_count} distinct available tracks for the requested pool"),
    )?;
    require(
        pool.len() <= 128,
        "CHAIN_POOL_LIMIT",
        "automatic chain search supports up to 128 analyzed recordings",
    )?;
    let features = pool
        .iter()
        .map(|track| ReturnFeatures::read(store, track, cancel))
        .collect::<Result<Vec<_>>>()?;
    let pacing: Vec<_> = pool
        .iter()
        .zip(&features)
        .map(|(t, f)| TrackPacing::from_features(&t.beats, &f.frames))
        .collect();
    let count = pool.len();
    let evidence = pool
        .iter()
        .map(|track| {
            track
                .analysis
                .artifacts
                .get("tactic_analysis")
                .map(|r| store.read_ref::<TacticTrackAnalysis>(r))
                .transpose()
        })
        .collect::<Result<Vec<_>>>()?;
    let mut edges = vec![vec![Vec::<PoolEdge>::new(); count]; count];
    for overlap in [LINGER_BEATS, 16, 12, 8, 4] {
        let exits = pool
            .iter()
            .zip(&features)
            .zip(&evidence)
            .map(|((track, f), record)| {
                pool_windows(track, f, record.as_ref(), overlap, context, true, cancel)
            })
            .collect::<Result<Vec<_>>>()?;
        let entries = pool
            .iter()
            .zip(&features)
            .zip(&evidence)
            .map(|((track, f), record)| {
                pool_windows(track, f, record.as_ref(), overlap, context, false, cancel)
            })
            .collect::<Result<Vec<_>>>()?;
        for (from, track) in pool.iter().enumerate() {
            for (to, incoming) in pool.iter().enumerate() {
                cancel.check()?;
                if preparation_only && evidence[from].is_some() == evidence[to].is_some() {
                    continue;
                }
                if from == to || track.overrides.operations.iter().any(|op| matches!(
                    op, Correction::ExcludeTransition { to_track_id } if to_track_id == &incoming.track.track_id
                )) { continue; }
                for a in &exits[from] {
                    let best = entries[to]
                        .iter()
                        .filter_map(|b| {
                            let (duration, stretch, skew) =
                                pair_timing(&a.passage, &b.passage, policy)?;
                            let harmonic = harmonic_distance(&a.tile, &b.tile);
                            let mashup = mashup_compatibility(&a.mashup_bars, &b.mashup_bars);
                            let breathing_room = mashup.map_or_else(
                                || drum_lead_preference(a, b, harmonic),
                                |distance| 0.065 - 0.025 * distance,
                            );
                            let drum_lead = overlap == LINGER_BEATS;
                            if drum_lead {
                                if a.passage.bar_beats != 4 || breathing_room == 0. {
                                    return None;
                                }
                            } else if duration > policy.overlap_seconds {
                                return None;
                            }
                            let level = (a.tile.relative_level - b.tile.relative_level)
                                .abs()
                                .min(2.);
                            let level_weight = if drum_lead { 0.010 } else { 0.025 };
                            let entry_fraction =
                                b.passage.start as f64 / incoming.track.format.frames as f64;
                            let score = skew
                                + 0.2 * stretch
                                + 0.05 * harmonic
                                + level_weight * level
                                + 0.04 * entry_fraction
                                + 0.08
                                    * (f64::from(a.unsupported_vocals)
                                        + f64::from(b.unsupported_vocals))
                                + 16_usize.saturating_sub(overlap) as f64 * 0.003
                                - if drum_lead { breathing_room } else { 0. };
                            Some(PoolEdge {
                                exit: a.passage.start,
                                entry_end: b.passage.end,
                                score,
                            })
                        })
                        .min_by(|a, b| a.score.total_cmp(&b.score));
                    if let Some(edge) = best {
                        edges[from][to].push(edge);
                    }
                }
            }
        }
    }

    // Rank new material by a feasible entrance AND later exit to different
    // prepared tracks. The same solo runway as the measured search applies.
    // These inexpensive connections nominate analysis, never admit a tactic.
    let mut preparation = Vec::new();
    for target in 0..count {
        if evidence[target].is_some() {
            continue;
        }
        let mut best: Option<(f64, usize, usize)> = None;
        for from in 0..count {
            if evidence[from].is_none() {
                continue;
            }
            for (to, prepared) in evidence.iter().enumerate() {
                if from == to || prepared.is_none() {
                    continue;
                }
                for incoming in &edges[from][target] {
                    for outgoing in &edges[target][to] {
                        let Some(cost) =
                            preparation_bridge_cost(incoming, outgoing, &pacing[target])
                        else {
                            continue;
                        };
                        if best.is_none_or(|old| cost < old.0) {
                            best = Some((cost, from, to));
                        }
                    }
                }
            }
        }
        if let Some((cost, from, to)) = best {
            preparation.push((cost, target, from, to));
        }
    }
    preparation.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let preparation_candidates = preparation.into_iter().map(|(cost, target, from, to)| {
        serde_json::json!({"track_id":pool[target].track.track_id,
            "basic_connection_cost":cost,"incoming_from":pool[from].track.track_id,
            "outgoing_to":pool[to].track.track_id,"pacing":"phrase-pacing/1",
            "basis":"basic timing, harmony, texture and source runway; pending stem/vocal admission"})
    }).collect();

    if preparation_only {
        return Ok(ChainPick {
            analyzed_tracks: count,
            orders: vec![],
            preparation_candidates,
        });
    }

    struct Route {
        tracks: Vec<usize>,
        entry_end: u64,
        score: f64,
        worst_join: f64,
    }
    let mut routes: Vec<_> = (0..count)
        .map(|i| Route {
            tracks: vec![i],
            entry_end: 0,
            score: 0.,
            worst_join: f64::NEG_INFINITY,
        })
        .collect();
    for _ in 1..track_count {
        let mut next = vec![];
        for route in &routes {
            let from = *route.tracks.last().unwrap();
            let earliest = if route.tracks.len() == 1 {
                0
            } else {
                pacing[from].earliest_exit(route.entry_end)
            };
            for to in 0..count {
                cancel.check()?;
                if route.tracks.contains(&to) {
                    continue;
                }
                if let Some(edge) = edges[from][to]
                    .iter()
                    .filter(|edge| {
                        edge.exit >= earliest
                            && pacing[from].assess(route.entry_end, edge.exit).is_some()
                    })
                    .min_by(|a, b| a.score.total_cmp(&b.score))
                {
                    let mut tracks = route.tracks.clone();
                    tracks.push(to);
                    next.push(Route {
                        tracks,
                        entry_end: edge.entry_end,
                        score: route.score + edge.score,
                        worst_join: route.worst_join.max(edge.score),
                    });
                }
            }
        }
        next.sort_by(|a, b| {
            (a.score + 0.75 * a.worst_join)
                .total_cmp(&(b.score + 0.75 * b.worst_join))
                .then(a.tracks.cmp(&b.tracks))
        });
        next.truncate(policy.beam_width);
        routes = next;
    }
    require(
        !routes.is_empty(),
        "CHAIN_POOL_INFEASIBLE",
        format!("no {track_count}-track route with compatible joins"),
    )?;
    let orders = routes
        .into_iter()
        .take(8)
        .map(|route| {
            route
                .tracks
                .into_iter()
                .map(|index| pool[index].track.track_id.clone())
                .collect()
        })
        .collect();
    Ok(ChainPick {
        analyzed_tracks: count,
        orders,
        preparation_candidates,
    })
}

#[derive(Clone, Copy, Debug)]
pub struct ChainClosingWindow {
    pub earliest: u64,
    pub latest: u64,
}

pub struct ChainBookends {
    pub start: u64,
    pub end: u64,
    pub outro_fade_frames: u64,
    pub ending: &'static str,
}

#[derive(Clone, Debug, Default)]
pub struct ChainOpening {
    attack: u64,
    accent: Option<u64>,
    lead: u64,
}
impl ChainOpening {
    pub fn before(&self, frame: u64) -> u64 {
        self.accent
            .filter(|accent| *accent <= frame)
            .map_or(self.attack, |accent| accent.saturating_sub(self.lead))
            .min(frame)
    }
}
pub fn read_chain_opening(
    store: &Store,
    track_id: &str,
    before_frame: u64,
    cancel: &Cancellation,
) -> Result<u64> {
    Ok(read_chain_opening_cue(store, track_id, cancel)?.before(before_frame))
}

/// Shared opening selection for both short chains and duration-aware sets.
pub fn read_chain_opening_cue(
    store: &Store,
    track_id: &str,
    cancel: &Cancellation,
) -> Result<ChainOpening> {
    let first_track = load_analyzed(store, store.track(track_id)?)?;
    let opening = ReturnFeatures::read(store, &first_track, cancel)?;
    let threshold = first_track.analysis.rms.max(1e-6) * 0.10;
    let first_audible = opening
        .frames
        .windows(4)
        .find(|w| w.iter().all(|f| f.rms >= threshold))
        .map_or(0, |w| w[0].source_frame);
    let opening_rate = u64::from(first_track.track.format.sample_rate_hz);
    let attack = first_audible.saturating_sub(opening_rate / 4);
    let accents: BTreeSet<_> = first_track.beats.downbeat_ids.iter().collect();
    let entry = first_track
        .beats
        .events
        .iter()
        .filter(|beat| {
            accents.contains(&beat.event_id)
                && beat.score >= 0.5
                && beat.source_frame >= first_audible
                && beat.source_frame <= first_audible + 2 * opening_rate
        })
        .find(|beat| {
            let at = beat.source_frame;
            let before = opening.rms(at.saturating_sub(opening_rate / 3), at);
            let after = opening.rms(at, at + opening_rate / 2);
            after >= threshold && before <= after * 0.30
        })
        .map(|beat| beat.source_frame);
    Ok(ChainOpening {
        attack,
        accent: entry,
        lead: opening_rate / 20,
    })
}

pub fn choose_chain_bookends(
    store: &Store,
    first: &Placement,
    last: &Placement,
    cancel: &Cancellation,
) -> Result<ChainBookends> {
    choose_chain_bookends_in_window(store, first, last, None, cancel)
}

/// Duration-targeted endings must be musical within the supplied source window;
/// an arbitrary source-tail fade is not a substitute for a feasible ending.
pub fn choose_chain_bookends_in_window(
    store: &Store,
    first: &Placement,
    last: &Placement,
    window: Option<ChainClosingWindow>,
    cancel: &Cancellation,
) -> Result<ChainBookends> {
    let start = read_chain_opening(store, &first.track_id, first.source_start_frame, cancel)?;
    let last_track = load_analyzed(store, store.track(&last.track_id)?)?;
    let closing = ReturnFeatures::read(store, &last_track, cancel)?;
    let rate = u64::from(last_track.track.format.sample_rate_hz);
    let source_end = last_track.track.format.frames;
    let solo_start = last.time_map.anchors[last.time_map.anchors.len() - 2].source_frame;
    let earliest = window.map_or_else(
        || {
            (solo_start + 16 * rate)
                .min(source_end)
                .max(last.source_end_frame)
        },
        |w| w.earliest.max(solo_start + 1),
    );
    let latest = window.map_or((earliest + 48 * rate).min(source_end), |w| {
        w.latest.min(source_end)
    });
    require(
        earliest <= latest,
        "CHAIN_BOOKEND",
        "no closing runway within the set duration window",
    )?;
    let tactic = if window.is_some() {
        crate::load_tactic_track_analysis(store, last)?
    } else {
        None
    };
    let protected = |end: u64, fade: u64| {
        window.is_some()
            && tactic
                .as_ref()
                .is_none_or(|record| !closing_is_clear(record, end.saturating_sub(fade), end))
    };
    let rms = last_track.analysis.rms.max(1e-6);
    let tail = closing.rms(source_end.saturating_sub(rate), source_end);
    let before_tail = closing.rms(
        source_end.saturating_sub(9 * rate),
        source_end.saturating_sub(rate),
    );
    if tail < 0.22 * rms && tail < 0.45 * before_tail {
        let last_audible = closing
            .frames
            .iter()
            .rfind(|f| f.rms >= 0.06 * rms)
            .map_or(source_end, |f| f.source_frame);
        let end = (last_audible + rate / 2).min(source_end);
        let end = if window.is_none() {
            end.max(earliest)
        } else {
            end
        };
        if end >= earliest && (window.is_none() || end <= latest) && !protected(end, rate / 2) {
            return Ok(ChainBookends {
                start,
                end,
                outro_fade_frames: rate / 2,
                ending: "natural_tail",
            });
        }
    }

    let accents: BTreeSet<_> = last_track.beats.downbeat_ids.iter().collect();
    let bars: Vec<_> = last_track
        .beats
        .events
        .iter()
        .filter(|beat| accents.contains(&beat.event_id) && beat.score >= 0.5)
        .map(|beat| beat.source_frame)
        .collect();
    let mut best = None;
    let mut cost = f64::INFINITY;
    for bar in bars.windows(2) {
        let (previous, end) = (bar[0], bar[1]);
        if end < earliest || end > latest || previous < solo_start {
            continue;
        }
        let before = closing.rms(previous.saturating_sub(2 * rate), previous) / rms;
        let last_bar = closing.rms(previous, end) / rms;
        let fade = (end - previous).clamp(rate / 2, 3 * rate);
        if protected(end, fade) || (window.is_some() && last_bar > 0.70 && last_bar > 0.70 * before)
        {
            continue;
        }
        let score = last_bar - 0.35 * (before - last_bar).max(0.)
            + 0.12 * (end - earliest) as f64
                / if window.is_some() {
                    latest.saturating_sub(earliest).max(1)
                } else {
                    48 * rate
                } as f64;
        if score < cost {
            cost = score;
            best = Some((end, fade));
        }
    }
    require(
        window.is_none() || best.is_some(),
        "CHAIN_BOOKEND",
        "no clear musical ending within the set duration window",
    )?;
    let (end, outro_fade_frames, ending) = best
        .map_or((source_end, 2 * rate, "source_tail_fade"), |(end, fade)| {
            (end, fade, "phrase_fade")
        });
    Ok(ChainBookends {
        start,
        end,
        outro_fade_frames,
        ending,
    })
}

fn closing_is_clear(record: &TacticTrackAnalysis, start: u64, end: u64) -> bool {
    let vocals_clear = record.vocal_evidence.get("track").is_some_and(|track| {
        crate::cached_tactic_evidence::verified_absence(
            track,
            start,
            end,
            record.source_sample_rate_hz,
        )
    });
    // Acoustic silence inside a continuing recognized line is not a line ending.
    let phrase_overlap = record
        .vocal_evidence
        .pointer("/track/phrases")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .any(|phrase| {
            let Some(a) = phrase
                .get("start_seconds")
                .and_then(serde_json::Value::as_f64)
            else {
                return true;
            };
            let Some(b) = phrase
                .get("end_seconds")
                .and_then(serde_json::Value::as_f64)
            else {
                return true;
            };
            !a.is_finite()
                || !b.is_finite()
                || a < 0.
                || b <= a
                || (a * f64::from(record.source_sample_rate_hz) < end as f64
                    && b * f64::from(record.source_sample_rate_hz) > start as f64)
        });
    let build_overlap = record
        .signal_evidence
        .get("events")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .any(|event| {
            if event.get("kind").and_then(serde_json::Value::as_str) != Some("build") {
                return false;
            }
            let Some(a) = event.get("frame").and_then(serde_json::Value::as_u64) else {
                return true;
            };
            let Some(b) = event.get("end_frame").and_then(serde_json::Value::as_u64) else {
                return true;
            };
            a < end && crate::transition_endpoints::build_release(record, a, b) > start
        });
    vocals_clear && !phrase_overlap && !build_overlap
}

fn build_stitch(
    store: &Store,
    request: &StitchRequest<'_>,
    policy: &MixingConfig,
    lead_bars: Option<usize>,
    cue: Option<BeatReturnCue>,
    style: StitchStyle,
    cancel: &Cancellation,
) -> Result<(MixPlan, Option<DrumHandoff>)> {
    let a = load_analyzed(store, store.track(request.from)?)?;
    let b = load_analyzed(store, store.track(request.to)?)?;
    build_stitch_from_tracks(
        store,
        [&a, &b],
        request,
        policy,
        lead_bars,
        cue,
        style,
        cancel,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_stitch_from_tracks(
    store: &Store,
    tracks: [&Analyzed; 2],
    request: &StitchRequest<'_>,
    policy: &MixingConfig,
    lead_bars: Option<usize>,
    cue: Option<BeatReturnCue>,
    style: StitchStyle,
    cancel: &Cancellation,
) -> Result<(MixPlan, Option<DrumHandoff>)> {
    policy.validate()?;
    require(
        (2..=if matches!(style, StitchStyle::PinnedTactic { .. }) {
            256
        } else {
            64
        })
            .contains(&request.overlap_beats)
            && (1..=64).contains(&request.context_beats),
        "STITCH_BEATS",
        "choose 2–64 overlap beats and 1–64 context beats",
    )?;
    require(
        request.from != request.to,
        "DUPLICATE_AUDIO",
        "choose two different tracks",
    )?;
    require(
        tracks[0].track.audio_hash != tracks[1].track.audio_hash,
        "DUPLICATE_AUDIO",
        "choose two different recordings",
    )?;
    require(
        !tracks[0].overrides.operations.iter().any(|o| {
            matches!(o, Correction::ExcludeTransition { to_track_id } if to_track_id == request.to)
        }),
        "STITCH_EXCLUDED",
        "a correction excludes this transition",
    )?;
    let bands = matches!(style, StitchStyle::Bands { .. });
    let require_band_space = matches!(
        style,
        StitchStyle::Bands {
            require_space: true
        }
    );
    let linger = matches!(style, StitchStyle::QuietLinger { .. });
    let beat_return = if let StitchStyle::QuietLinger { outgoing, .. } = style {
        Some(BeatReturnSpan {
            source_start: outgoing,
            overlap: LINGER_BEATS,
            fade_beat: None,
            vocal_bars: None,
        })
    } else {
        cue.as_ref()
            .map(|cue| beat_return(store, tracks[0], cue.outgoing_near, cue, cancel))
            .transpose()?
    };
    let overlap_beats = beat_return.map_or(request.overlap_beats, |span| span.overlap);
    let vocal_fade_beat = beat_return.and_then(|span| span.fade_beat);
    let vocal_bars = if linger {
        Some(LINGER_BARS.to_vec())
    } else {
        beat_return
            .and_then(|span| span.vocal_bars)
            .map(|bars| bars.to_vec())
    };
    let mut outgoing = if linger {
        linger_outgoing(tracks[0], request.context_beats, cancel)?
    } else {
        passages_for_clock(
            tracks[0],
            overlap_beats,
            request.context_beats,
            true,
            beat_return.is_some(),
            !matches!(style, StitchStyle::PinnedTactic { .. }),
            cancel,
        )?
    };
    outgoing.retain(|p| p.start >= request.outgoing_after_frame);
    if let StitchStyle::PinnedTactic {
        outgoing: frame, ..
    } = style
    {
        outgoing.retain(|p| p.start == frame);
    }
    if let Some(span) = beat_return {
        outgoing.retain(|p| p.start == span.source_start);
    }
    let incoming_overlap = match style {
        StitchStyle::PinnedTactic {
            incoming_overlap, ..
        } => incoming_overlap,
        _ => overlap_beats,
    };
    let mut incoming = passages_for_clock(
        tracks[1],
        incoming_overlap,
        request.context_beats,
        false,
        beat_return.is_some() && !linger,
        !matches!(style, StitchStyle::PinnedTactic { .. }),
        cancel,
    )?;

    if let StitchStyle::QuietLinger {
        incoming: frame, ..
    }
    | StitchStyle::PinnedTactic {
        incoming: frame, ..
    } = style
    {
        incoming.retain(|p| p.start == frame);
    }

    if let Some(near) = cue.and_then(|cue| cue.incoming_cut_near) {
        let rate = f64::from(tracks[1].track.format.sample_rate_hz);
        let target = vocal_bars
            .as_ref()
            .map(|bars| bars[0])
            .or(vocal_fade_beat)
            .unwrap_or(overlap_beats);
        let tolerance = if vocal_fade_beat.is_some() { 0.2 } else { 0.8 };
        incoming.retain(|p| (p.beat_frames[target] as f64 / rate - near).abs() <= tolerance);
        require(
            !incoming.is_empty(),
            "STITCH_INCOMING_CUE",
            "no eligible incoming beat near the entry cue",
        )?;
    }

    let (outgoing_low, incoming_profiles): (BTreeMap<_, _>, BTreeMap<_, _>) = if bands {
        let outgoing_features = ReturnFeatures::read(store, tracks[0], cancel)?;
        let incoming_features = ReturnFeatures::read(store, tracks[1], cancel)?;
        let rms = tracks[1].analysis.rms.max(1e-6);
        // Leave space for the outgoing track during the first bar.
        let incoming_profiles = incoming
            .iter()
            .map(|p| {
                (
                    p.start,
                    (
                        incoming_features.rms(p.start, p.beat_frames[p.bar_beats]) / rms,
                        incoming_features.low_band_share(p.start, p.end),
                    ),
                )
            })
            .collect();
        let outgoing_low = outgoing
            .iter()
            .map(|p| (p.start, outgoing_features.low_band_share(p.start, p.end)))
            .collect();
        (outgoing_low, incoming_profiles)
    } else {
        (BTreeMap::new(), BTreeMap::new())
    };

    let mut levels = Vec::new();
    if let Some(bars) = lead_bars {
        require(
            (1..=8).contains(&bars),
            "STITCH_DRUM_BARS",
            "choose 1–8 drum lead bars",
        )?;
        let harmony: Harmony = store.read_ref(&tracks[0].analysis.artifacts["harmony"])?;
        for reference in &harmony.feature_chunks {
            cancel.check()?;
            let chunk: FeatureChunk = store.read_ref(reference)?;
            levels.extend(chunk.frames.into_iter().map(|f| (f.source_frame, f.rms)));
        }
        levels.sort_unstable_by_key(|&(frame, _)| frame);
    }

    // The mean duration gives the two sources equal and opposite speed changes.
    // Compare every paired beat after stretching; matching BPM alone can hide drift.
    let mut best = None;
    let mut best_cost = f64::INFINITY;
    for a in &outgoing {
        cancel.check()?;
        let mut lead_beat = 0;
        let mut quiet_ratio = 0.;
        if let Some(bars) = lead_bars {
            let Some(&beat) = a
                .downbeats
                .get(bars)
                .filter(|&&n| n < request.overlap_beats)
            else {
                continue;
            };
            lead_beat = beat;
            let first = levels.partition_point(|&(frame, _)| frame < a.start);
            let last = levels.partition_point(|&(frame, _)| frame < a.beat_frames[lead_beat]);
            if first == last {
                continue;
            }
            let rms = (levels[first..last]
                .iter()
                .map(|&(_, rms)| rms * rms)
                .sum::<f64>()
                / (last - first) as f64)
                .sqrt();
            quiet_ratio = rms / tracks[0].analysis.rms.max(1e-6);
            // Require the lead to sit at least 6 dB below the track's overall level.
            if quiet_ratio > 0.5 {
                continue;
            }
        }
        for b in &incoming {
            if require_band_space
                && outgoing_low
                    .get(&a.start)
                    .copied()
                    .flatten()
                    .zip(incoming_profiles.get(&b.start).and_then(|(_, low)| *low))
                    .is_some_and(|(a, b)| crate::tactics::shared_low_band_dominance(a, b))
            {
                continue;
            }
            let timing = if matches!(style, StitchStyle::PinnedTactic { .. }) {
                mapped_pair_timing(a, b, policy)
            } else {
                pair_timing(a, b, policy)
            };
            let Some((duration, deviation, skew)) = timing else {
                continue;
            };
            if vocal_bars.is_none()
                && !matches!(style, StitchStyle::PinnedTactic { .. })
                && duration * (1. - a.beat_phases[vocal_fade_beat.unwrap_or(lead_beat)])
                    > policy.overlap_seconds
            {
                continue;
            }
            let cost = skew
                + 0.5 * (a.grid_error + b.grid_error)
                + 0.2 * deviation
                + 0.2 * quiet_ratio
                + incoming_profiles
                    .get(&b.start)
                    .map_or(0., |(level, _)| 0.075 * (level - 0.55).abs());
            if cost < best_cost {
                best_cost = cost;
                best = Some((a, b, duration, skew, lead_beat, quiet_ratio));
            }
        }
    }
    let (a, b, duration, skew, lead_beat, quiet_ratio) = best.ok_or_else(|| Error::new(
        "STITCH_INFEASIBLE",
        if lead_bars.is_some() {
            "no beat-matched pair with a quiet outgoing lead and at least one bar left to crossfade"
        } else {
            "no reliable passages with matching downbeats, at most 30 ms beat skew, and speed changes within the configured limits"
        },
    ))?;

    let rate = policy.output_sample_rate_hz;
    let overlap = (duration * rate as f64).round() as u64;
    let scaled = |frames, passage: &Passage| {
        (frames as f64 * overlap as f64 / (passage.end - passage.start) as f64).round() as u64
    };
    let start = scaled(a.start - a.context, a);
    let end = start + overlap;
    let total = end + scaled(b.context - b.end, b);
    let map = |i: usize, points: [(u64, u64); 3]| TimeMap {
        header: Header::default(),
        map_id: format!("stitch-{}", tracks[i].track.track_id),
        source_sample_rate_hz: tracks[i].track.format.sample_rate_hz,
        output_sample_rate_hz: rate,
        interpolation: "monotone_cubic".into(),
        anchors: points
            .into_iter()
            .map(|(source_frame, output_frame)| Anchor {
                source_frame,
                output_frame,
            })
            .collect(),
        speed_ratio_limits: [
            1. - policy.max_speed_deviation,
            1. + policy.max_speed_deviation,
        ],
        max_speed_change_per_second: policy.max_speed_change_per_second,
        preserve_pitch: true,
    };
    let mut maps = [
        map(0, [(a.context, 0), (a.start, start), (a.end, end)]),
        map(1, [(b.start, start), (b.end, end), (b.context, total)]),
    ];
    let mut bar_bpms = vec![];
    if (bands
        || linger
        || matches!(style, StitchStyle::PinnedTactic { .. }) && incoming_overlap == overlap_beats)
        && overlap_beats >= 2 * a.bar_beats
    {
        let bar_count = overlap_beats / a.bar_beats;
        // Keep the overlap endpoints fixed and move gradually toward the
        // incoming natural tempo. If quantized beat markers force the cubic
        // map to accelerate too fast, allow small deviations from those
        // markers as long as the tracks still agree on beat timing.
        let drift = 0.70 * (b.seconds - a.seconds) / duration;
        let output_bar = |bar: usize| {
            let t = bar as f64 / bar_count as f64;
            start + ((t + 0.5 * drift * (t * t - t)) * overlap as f64).round() as u64
        };
        let tolerance = MAX_BEAT_SKEW_SECONDS * f64::from(rate);
        for grid_weight in [1., 0.5, 0.] {
            let mut ramped = maps.clone();
            for (index, passage) in [a, b].into_iter().enumerate() {
                let at = if index == 0 { 2 } else { 1 };
                ramped[index].anchors.splice(
                    at..at,
                    (1..bar_count).map(|bar| {
                        let even = passage.start as f64
                            + (passage.end - passage.start) as f64 * bar as f64 / bar_count as f64;
                        let measured = passage.beat_frames[bar * a.bar_beats] as f64;
                        Anchor {
                            source_frame: (even + grid_weight * (measured - even)).round() as u64,
                            output_frame: output_bar(bar),
                        }
                    }),
                );
            }
            if ramped.iter().any(|map| map.validate().is_err()) {
                continue;
            }
            let aligned = a.beat_frames.iter().zip(&b.beat_frames).enumerate().all(
                |(beat, (&a_frame, &b_frame))| {
                    let out_a = output_at_beat(&ramped[0], a_frame);
                    let out_b = output_at_beat(&ramped[1], b_frame);
                    (out_a - out_b).abs() <= tolerance
                        && (beat % a.bar_beats != 0
                            || [out_a, out_b].iter().all(|out| {
                                (out - output_bar(beat / a.bar_beats) as f64).abs() <= tolerance
                            }))
                },
            );
            if aligned {
                maps = ramped;
                bar_bpms = (0..bar_count)
                    .map(|bar| {
                        60. * a.bar_beats as f64 * rate as f64
                            / (output_bar(bar + 1) - output_bar(bar)) as f64
                    })
                    .collect();
                break;
            }
        }
    }
    let beat_output: Vec<_> = b
        .beat_frames
        .iter()
        .map(|&frame| output_at_beat(&maps[1], frame).round() as u64)
        .collect();
    let [outgoing_map, incoming_map] = maps;
    let mut state = State {
        used: vec![0, 1],
        placements: vec![
            placement(tracks[0], outgoing_map),
            placement(tracks[1], incoming_map),
        ],
        transitions: vec![Transition {
            transition_id: id(),
            from_track_id: request.from.into(),
            to_track_id: request.to.into(),
            outgoing_region_id: format!("beats-{}-{}", a.start, a.end),
            incoming_region_id: format!("beats-{}-{}", b.start, b.end),
            start_output_frame: start,
            end_output_frame: end,
            template: if bands {
                Template::ThreeBandExchange
            } else {
                Template::ShortCrossfade
            },
            costs: BTreeMap::new(),
            fallback_reason: None,
        }],
        cost: best_cost,
        tie: 0,
    };
    for (i, track) in tracks.iter().enumerate() {
        state.placements[i].automation = automation_for(i, &state, track, rate);
    }
    if bands {
        apply_band_exchange(&mut state, tracks, &beat_output);
    }
    let handoff = if beat_return.is_some() {
        // A short de-click on the departing layer leaves the cut at the downbeat.
        let fade = state.placements[0]
            .automation
            .iter_mut()
            .find(|p| p.output_frame == start)
            .unwrap();
        let source_frame = b.beat_frames[vocal_fade_beat.unwrap_or(overlap_beats)];
        let output_frame = beat_output[vocal_fade_beat.unwrap_or(overlap_beats)];
        fade.output_frame = if vocal_fade_beat.is_some() {
            output_frame
        } else {
            end - (u64::from(rate) / 200).min(overlap / 2)
        };
        state.placements[1].automation[0].mute = false;
        state.placements[1].automation[0].gain_linear = gain(tracks[1]);
        Some(DrumHandoff {
            output_frame,
            source_frame,
            vocal_bars: vocal_bars
                .as_ref()
                .map(|bars| bars.iter().map(|&beat| beat_output[beat]).collect()),
        })
    } else {
        lead_bars.map(|_| {
            let output_frame = start + scaled(b.beat_frames[lead_beat] - b.start, b);
            let outgoing = &mut state.placements[0].automation;
            let fade = outgoing
                .iter_mut()
                .find(|p| p.output_frame == start)
                .unwrap();
            fade.output_frame = output_frame;
            let incoming = &mut state.placements[1].automation;
            let mut drum_gain = incoming
                .iter()
                .find(|p| p.output_frame == end)
                .unwrap()
                .clone();
            drum_gain.output_frame = start + rate as u64 / 50;
            incoming.insert(1, drum_gain);
            DrumHandoff {
                output_frame,
                source_frame: b.beat_frames[lead_beat],
                vocal_bars: None,
            }
        })
    };
    let mut policy = policy.clone();
    policy.allow_fallback = false;
    policy.template = if bands {
        Template::ThreeBandExchange
    } else {
        Template::ShortCrossfade
    };
    let mut plan = MixPlan {
        header: Header::default(),
        plan_id: id(),
        planner_version: "beat-stitch/1".into(),
        renderer_version: RENDERER.into(),
        policy_hash: canonical_hash(&policy)?,
        policy,
        seed: 42,
        objective: if bands { "band_stitch" } else { "beat_stitch" }.into(),
        requested_duration_seconds: None,
        complete: true,
        output: OutputFormat {
            sample_rate_hz: rate,
            channels: 2,
            encoding: "float32".into(),
            dither: "seeded_tpdf_1_lsb".into(),
        },
        drum_intro: None,
        prepared_patches: vec![],
        placements: state.placements,
        transitions: state.transitions,
        total_output_frames: total,
        crossover_hz: [200., 2500.],
        crossover_design: "complementary_one_pole_v1".into(),
        automation_interpolation: "smoothstep_linear_amplitude".into(),
        diagnostics: vec![
            format!(
                "{}-beat overlap at {:.2} BPM, {} context beats per side",
                overlap_beats,
                overlap_beats as f64 * 60. / duration,
                request.context_beats
            ),
            format!(
                "source speed multipliers {:.5} and {:.5}; predicted maximum beat skew {:.1} ms",
                a.seconds / duration,
                b.seconds / duration,
                skew * 1000.
            ),
        ],
    };
    if a.continued_meter || b.continued_meter {
        plan.diagnostics.push(
            "continued the established four-beat meter through surplus downbeat labels; all expected downbeats and reliable pulses are present".into(),
        );
    }
    if !bar_bpms.is_empty() {
        plan.header
            .extensions
            .insert("bar_bpm".into(), serde_json::json!(bar_bpms));
        plan.diagnostics.push(format!(
            "shared tempo moves from {:.2} to {:.2} BPM across the overlap",
            bar_bpms[0],
            bar_bpms[bar_bpms.len() - 1]
        ));
    }
    if bands {
        plan.diagnostics.push("three-band exchange: highs enter first, bass switches around the midpoint, and mids crossfade last".into());
    }
    if let Some(bars) = lead_bars {
        plan.objective = "drum_stitch".into();
        plan.diagnostics.push(format!(
            "{bars} drum lead bars; outgoing lead RMS {:.1} dB relative to the track",
            20. * quiet_ratio.max(1e-9).log10()
        ));
    }
    if let Some(span) = beat_return {
        let frame = span.source_start;
        plan.objective = "beat_return_stitch".into();
        let source_rate = f64::from(tracks[0].track.format.sample_rate_hz);
        if linger {
            plan.objective = "quiet_drum_linger".into();
            plan.diagnostics.push(format!(
                "sustained outgoing texture at {:.3}s; hold it under incoming drums for two bars",
                frame as f64 / source_rate
            ));
        } else if vocal_bars.is_some() {
            plan.objective = "auto_vocal_handoff".into();
            plan.diagnostics.push(format!(
                "swap drums at {:.3}s and search for a vocal gap after four complete bars",
                frame as f64 / source_rate
            ));
        } else if let Some(fade_beat) = vocal_fade_beat {
            plan.diagnostics.push(format!(
                "swap drums at {:.3}s; retain outgoing non-drum layers until {:.3}s, then fade them out over one beat",
                frame as f64 / source_rate, a.beat_frames[fade_beat] as f64 / source_rate
            ));
        } else {
            plan.diagnostics.push(format!(
                "outgoing drums return at {:.3}s; swap there and cut at the next bar at {:.3}s",
                frame as f64 / source_rate,
                a.end as f64 / source_rate
            ));
        }
        let incoming_rate = f64::from(tracks[1].track.format.sample_rate_hz);
        plan.diagnostics.push(format!(
            "incoming drums start at {:.3}s; accompaniment enters at {:.3}s in the incoming source",
            b.start as f64 / incoming_rate,
            b.beat_frames[vocal_bars
                .as_ref()
                .map(|bars| bars[if linger { 2 } else { 0 }])
                .or(vocal_fade_beat)
                .unwrap_or(overlap_beats)] as f64
                / incoming_rate
        ));
    }
    plan.validate()?;
    Ok((plan, handoff))
}

#[cfg(test)]
mod mashup_tests {
    use super::*;

    #[test]
    fn preparation_requires_a_later_exit_after_the_same_solo_runway() {
        let incoming = PoolEdge {
            exit: 0,
            entry_end: 136,
            score: 0.01,
        };
        let early = PoolEdge {
            exit: 159,
            entry_end: 0,
            score: 0.001,
        };
        let later = PoolEdge {
            exit: 182,
            entry_end: 0,
            score: 0.05,
        };
        assert!(preparation_bridge_cost(&incoming, &early, &TrackPacing::unknown(1)).is_none());
        assert!(preparation_bridge_cost(&incoming, &later, &TrackPacing::unknown(1)).is_some());
    }

    #[test]
    fn return_probe_quota_never_evicts_existing_dense_entries() {
        let original = vec![(32, 0.1, 100, 200, 4, 32), (64, 0.2, 100, 200, 4, 64)];
        let mut choices = original.clone();
        let keys = append_return_probes(
            &mut choices,
            vec![
                original[0],
                (16, 0.3, 300, 400, 4, 16),
                (32, 0.4, 300, 400, 4, 32),
            ],
            1,
        );
        assert_eq!(&choices[..2], original.as_slice());
        assert_eq!(choices.len(), 3);
        assert_eq!(keys.len(), 1);
        assert!(keys.contains(&(16, 300, 400, 16)));
    }

    #[test]
    fn beat_change_after_handoff_does_not_invalidate_synchronized_overlap() {
        let mut grid = Beats {
            header: Header::default(),
            beats_id: "test".into(),
            source_sample_rate_hz: 1000,
            source_frames: 6000,
            events: (0..=12)
                .map(|n| Beat {
                    event_id: n.to_string(),
                    source_frame: n * 500,
                    score: 1.,
                    local_bpm: Some(120.),
                })
                .collect(),
            downbeat_ids: vec![],
            meter_segments: vec![],
            unreliable_intervals: vec![],
            score_semantics: "synthetic test".into(),
        };
        grid.events[9].source_frame += 180;
        assert!(!reliable_clock(&grid, 0, 12, 0.5));
        assert!(
            reliable_clock(&grid, 0, 8, 0.5),
            "solo beat changes are allowed after the mix ends"
        );
        grid.events[7].source_frame += 180;
        assert!(
            !reliable_clock(&grid, 0, 8, 0.5),
            "overlapping beats must still be reliable"
        );
    }

    #[test]
    fn cue_budget_preserves_long_windows_before_tactic_admission() {
        let ranked = vec![
            (4, 0.0, 1, 1, 4, 4),
            (4, 0.1, 2, 2, 4, 4),
            (4, 0.2, 3, 3, 4, 4),
            (8, 0.3, 4, 4, 4, 8),
            (8, 0.4, 5, 5, 4, 8),
            (32, 0.9, 6, 6, 4, 32),
            (64, 1.0, 7, 7, 4, 64),
        ];
        let balanced = distribute_tactic_durations(ranked);
        assert_eq!(
            balanced.iter().take(4).map(|c| c.0).collect::<Vec<_>>(),
            vec![4, 8, 32, 64]
        );
        assert_eq!(
            balanced
                .iter()
                .filter(|c| c.0 == 4)
                .map(|c| c.2)
                .collect::<Vec<_>>(),
            vec![1, 2, 3],
            "ranking within each duration remains intact"
        );
    }

    fn bar(drums: bool) -> BarTile {
        BarTile {
            start_frame: 0,
            end_frame: 88200,
            bars: 1,
            bpm: 120.,
            relative_level: 0.8,
            relative_bass: 0.8,
            onset: if drums { 0.01 } else { 0.003 },
            chroma: [1. / 12.; 12],
            relative_onset: 1.,
            level_change: 0.,
            mashup_bed: !drums,
            mashup_drums: drums,
            phrase: None,
            role: SegmentRole::Mixed,
        }
    }

    fn pulse_passage(pulses: usize, seconds: f64) -> Passage {
        Passage {
            start: 0,
            end: (seconds * 48000.) as u64,
            context: 0,
            seconds,
            beat_phases: (0..=pulses).map(|i| i as f64 / pulses as f64).collect(),
            beat_frames: (0..=pulses)
                .map(|i| (i as f64 * seconds * 48000. / pulses as f64).round() as u64)
                .collect(),
            downbeats: (0..=pulses).step_by(4).collect(),
            grid_error: 0.,
            bar_beats: 4,
            continued_meter: false,
        }
    }

    #[test]
    fn pulse_reinterpretation_changes_correspondence_not_playback_speed() {
        let a = pulse_passage(16, 8.);
        let b = pulse_passage(32, 8.08);
        let policy = MixingConfig::default();
        assert!(pair_timing(&a, &b, &policy).is_none());
        let (_, deviation, skew) = mapped_pair_timing(&a, &b, &policy).unwrap();
        assert!(deviation < 0.01 && skew < 1e-9);
        assert!(mapped_pair_timing(&b, &a, &policy).is_some());
        assert!(
            mapped_pair_timing(&a, &pulse_passage(32, 16.), &policy).is_none(),
            "2x playback is forbidden"
        );
        let mut drift = b;
        drift.beat_phases[8] += 0.04;
        assert!(
            mapped_pair_timing(&a, &drift, &policy).is_none(),
            "every mapped beat must stay aligned"
        );
    }

    #[test]
    fn mashup_requires_sustained_bars_through_the_latest_exit() {
        let outgoing = vec![bar(false); MASHUP_VERIFIED_BARS];
        let mut incoming = vec![bar(true); MASHUP_VERIFIED_BARS];
        assert_eq!(mashup_compatibility(&outgoing, &incoming), Some(0.));
        assert!(mashup_compatibility(&outgoing[..MASHUP_VERIFIED_BARS - 1], &incoming).is_none());

        incoming[MASHUP_VERIFIED_BARS - 1].mashup_drums = false;
        assert!(mashup_compatibility(&outgoing, &incoming).is_none());
        assert!(!mashup_capabilities(&incoming).1);

        incoming[MASHUP_VERIFIED_BARS - 1] = bar(true);
        incoming[2].chroma = [0.; 12];
        incoming[2].chroma[0] = 1.;
        assert!(mashup_compatibility(&outgoing, &incoming).is_none());

        incoming[2] = bar(true);
        incoming[2].relative_level = 0.3;
        assert!(mashup_compatibility(&outgoing, &incoming).is_none());
    }
}

#[cfg(test)]
mod opening_cue_tests {
    use super::*;
    #[test]
    fn composed_ending_cannot_fade_a_vocal_line_or_unreleased_build() {
        let plan: MixPlan =
            serde_json::from_str(include_str!("../../../examples/mix-plan.json")).unwrap();
        let p = &plan.placements[0];
        let mut record = TacticTrackAnalysis {
            header: Header::default(),
            tactic_analysis_id: "ending-fixture".into(),
            track_id: p.track_id.clone(),
            audio_hash: p.audio_hash.clone(),
            audio_revision: 1,
            source_sample_rate_hz: 100,
            source_frames: 1000,
            base_analysis: p.analysis.clone(),
            beat_artifact: p.analysis.clone(),
            override_revision: 0,
            method: TACTIC_ANALYSIS_METHOD.into(),
            implementation_hash: p.audio_hash.clone(),
            component_hashes: BTreeMap::new(),
            cache_key: p.audio_hash.clone(),
            models: BTreeMap::new(),
            stems: BTreeMap::new(),
            vocal_evidence: serde_json::json!({"track":{"verified_absence_spans":[{
                "start_seconds":2.,"end_seconds":9.,
                "verification_method":"separated_vocal_rms_and_peak_negligible/1","reference":"fixture"}]}}),
            signal_evidence: serde_json::json!({"events":[{"kind":"build","frame":300,"end_frame":600}]}),
            limitations: vec![],
            created_at: "fixture".into(),
        };
        assert!(
            !closing_is_clear(&record, 500, 700),
            "a clear vocal gap cannot authorize fading the crescendo"
        );
        assert!(closing_is_clear(&record, 600, 700));
        record.vocal_evidence["track"]["phrases"] =
            serde_json::json!([{"start_seconds":3.,"end_seconds":8.}]);
        assert!(
            !closing_is_clear(&record, 600, 700),
            "a quiet breath inside a continuing line cannot end the song"
        );
        record.vocal_evidence["track"]["phrases"] = serde_json::json!([]);
        assert!(
            !closing_is_clear(&record, 800, 950),
            "the complete fade must lie within verified vocal absence"
        );
        record.vocal_evidence = serde_json::json!({"track":{"words":[],"instrumental":true}});
        assert!(
            !closing_is_clear(&record, 600, 700),
            "no transcript is not acoustic absence evidence"
        );
    }
    #[test]
    fn cached_opening_keeps_the_before_frame_constraint() {
        let cue = ChainOpening {
            attack: 75,
            accent: Some(150),
            lead: 5,
        };
        assert_eq!(cue.before(50), 50);
        assert_eq!(cue.before(100), 75);
        assert_eq!(cue.before(149), 75);
        assert_eq!(cue.before(150), 145);
        assert_eq!(cue.before(500), 145);
    }
}
