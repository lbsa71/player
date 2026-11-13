//! Pair evidence is measured for each proposed processing schedule. A favorable
//! drum-only hold must never authorize unrelated full-spectrum or vocal overlap.
use crate::tactic_evidence::output_at;
use crate::tactics::*;
use mixlib_types::*;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const METHOD: &str = "mapped_selected_lane_pair_evidence/3";
const LANES: [StemLane; 4] = [
    StemLane::Drums,
    StemLane::Bass,
    StemLane::Other,
    StemLane::Vocals,
];
fn name(l: StemLane) -> &'static str {
    match l {
        StemLane::Stereo => "source",
        StemLane::Drums => "drums",
        StemLane::Bass => "bass",
        StemLane::Other => "other",
        StemLane::Vocals => "vocals",
    }
}
fn number(v: &Value, key: &str) -> Option<f64> {
    v.get(key)?.as_f64().filter(|x| x.is_finite())
}
fn frame(v: &Value, key: &str) -> Option<u64> {
    v.get(key)?.as_u64().filter(|x| *x <= MAX_FRAME)
}
fn array<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}
fn db(v: f64) -> f64 {
    20. * v.max(1e-12).log10()
}
fn interval_distance(a: &[f64; 12], b: &[f64; 12]) -> Option<f64> {
    let sums = [a.iter().sum::<f64>(), b.iter().sum::<f64>()];
    if sums[0] * sums[1] <= 1e-18 {
        return None;
    }
    let a = a.map(|x| x / sums[0]);
    let b = b.map(|x| x / sums[1]);
    let mass = |a: &[f64; 12], b: &[f64; 12]| {
        a.iter()
            .enumerate()
            .flat_map(|(i, a)| {
                b.iter().enumerate().map(move |(j, b)| {
                    if [0, 3, 4, 5, 7, 8, 9].contains(&((j + 12 - i) % 12)) {
                        0.
                    } else {
                        a * b
                    }
                })
            })
            .sum::<f64>()
    };
    // Bar chroma aggregates notes over time. Internal intervals in a passage
    // are not new clashes introduced by the other deck. Without this baseline
    // even an identical seven-note major scale exceeds the admission limit.
    // Retain spectral disagreement in proportion to that baseline so unrelated
    // broad pitch distributions cannot erase their mismatch by being dense.
    let baseline = (mass(&a, &a) + mass(&b, &b)) / 2.;
    let variation = a.iter().zip(b).map(|(a, b)| (a - b).abs()).sum::<f64>() / 2.;
    Some(
        ((mass(&a, &b) - baseline).max(0.) / (1. - baseline).max(1e-9) + baseline * variation)
            .clamp(0., 1.),
    )
}

fn layer_stability(levels: &[f64], held_bars: usize) -> Vec<f64> {
    let held = &levels[..held_bars.max(1).min(levels.len())];
    let mean = held.iter().sum::<f64>() / held.len().max(1) as f64;
    levels
        .iter()
        .map(|level| {
            if mean > 1e-6 {
                (1. - (level - mean).abs() / mean).clamp(0., 1.)
            } else {
                0.
            }
        })
        .collect()
}
#[derive(Clone, Debug)]
struct Note {
    start: u64,
    end: u64,
    midi: f64,
    score: f64,
}
fn notes(values: &[Value]) -> Vec<Note> {
    let mut result: Vec<_> = values
        .iter()
        .filter_map(|v| {
            let n = Note {
                start: frame(v, "start_frame")?,
                end: frame(v, "end_frame")?,
                midi: number(v, "midi_note")?,
                score: number(v, "score")?,
            };
            (n.start < n.end && n.score >= 0.78 && (0.0..=127.).contains(&n.midi)).then_some(n)
        })
        .collect();
    result.sort_by_key(|n| (n.start, n.end));
    result
}
#[derive(Clone, Debug)]
struct Stats {
    rms: f64,
    peak: f64,
    activity: f64,
    crest: f64,
    tonal: f64,
    lead: f64,
    pitched: f64,
    bands: [f64; 3],
    chroma: [f64; 12],
    notes: Vec<Note>,
}
impl Stats {
    fn read(v: &Value) -> Option<Self> {
        let c = v.get("chroma")?.as_array()?;
        if c.len() != 12 {
            return None;
        }
        let mut chroma = [0.; 12];
        for (i, v) in c.iter().enumerate() {
            chroma[i] = v.as_f64().filter(|x| x.is_finite() && *x >= 0.)?;
        }
        Some(Self {
            rms: number(v, "rms")?,
            peak: number(v, "peak")?,
            activity: number(v, "activity_fraction")?,
            crest: number(v, "crest_db")?,
            tonal: number(v, "tonalness")?,
            lead: number(v, "foreground_lead_score")?,
            pitched: number(v, "pitched_fraction")?,
            bands: [
                number(v, "low_fraction")?,
                number(v, "mid_fraction")?,
                number(v, "high_fraction")?,
            ],
            chroma,
            notes: notes(array(v, "pitch_notes")),
        })
    }
}
#[derive(Clone, Debug)]
struct Window {
    source: Stats,
    lanes: BTreeMap<StemLane, Stats>,
}

/// Aggregate only fully covered source spans. Peaks are upper bounds from all
/// intersecting measured bars; RMS/spectral summaries are duration weighted.
fn window(record: &TacticTrackAnalysis, start: u64, end: u64) -> Option<Window> {
    if start >= end {
        return None;
    }
    let mut rows: Vec<_> = array(&record.signal_evidence, "bars")
        .iter()
        .chain(array(&record.signal_evidence, "unmetered_windows"))
        .filter_map(|v| {
            let a = frame(v, "start_frame")?;
            let b = frame(v, "end_frame")?;
            (a < end && b > start && a < b).then_some((a, b, v))
        })
        .collect();
    rows.sort_by_key(|(a, b, _)| (*a, *b));
    let mut covered = start;
    for (a, b, _) in &rows {
        if *a > covered {
            return None;
        }
        covered = covered.max(*b);
    }
    if covered < end {
        return None;
    }
    let aggregate = |lane: StemLane| -> Option<Stats> {
        let mut out: Option<Stats> = None;
        let mut total = 0.;
        let mut distinct = BTreeMap::new();
        for (a, b, v) in &rows {
            let value = if lane == StemLane::Stereo {
                v.get("source")?
            } else {
                v.get("stems")?.get(name(lane))?
            };
            let s = Stats::read(value)?;
            let weight = (end.min(*b) - start.max(*a)) as f64;
            if s.rms < 0. || s.peak < 0. || !(0.0..=1.).contains(&s.activity) {
                return None;
            }
            let o = out.get_or_insert_with(|| Stats {
                rms: 0.,
                peak: 0.,
                activity: 0.,
                crest: 0.,
                tonal: 0.,
                lead: 0.,
                pitched: 0.,
                bands: [0.; 3],
                chroma: [0.; 12],
                notes: vec![],
            });
            o.rms += weight * s.rms * s.rms;
            o.peak = o.peak.max(s.peak);
            o.activity += weight * s.activity;
            o.crest += weight * s.crest;
            o.tonal += weight * s.tonal;
            o.lead = o.lead.max(s.lead);
            o.pitched += weight * s.pitched;
            for i in 0..3 {
                o.bands[i] += weight * s.bands[i];
            }
            for i in 0..12 {
                o.chroma[i] += weight * s.rms * s.rms * s.chroma[i];
            }
            for note in s
                .notes
                .into_iter()
                .filter(|n| n.start < end && n.end > start)
            {
                distinct.insert((note.start, note.end, note.midi as i32), note);
            }
            total += weight;
        }
        let mut o = out?;
        o.rms = (o.rms / total).sqrt();
        o.activity /= total;
        o.crest /= total;
        o.tonal /= total;
        o.pitched /= total;
        for v in &mut o.bands {
            *v /= total;
        }
        let norm = o.chroma.iter().sum::<f64>();
        if norm > 1e-12 {
            for v in &mut o.chroma {
                *v /= norm;
            }
        }
        o.notes = distinct.into_values().collect();
        Some(o)
    };
    let mut lanes = BTreeMap::new();
    for l in LANES {
        lanes.insert(l, aggregate(l)?);
    }
    Some(Window {
        source: aggregate(StemLane::Stereo)?,
        lanes,
    })
}
fn value(points: &[ScalarPoint], at: u64) -> f64 {
    if points.is_empty() {
        return 1.;
    }
    if at <= points[0].output_frame {
        return points[0].value;
    }
    for w in points.windows(2) {
        if at <= w[1].output_frame {
            let x =
                (at - w[0].output_frame) as f64 / (w[1].output_frame - w[0].output_frame) as f64;
            let s = x * x * (3. - 2. * x);
            return w[0].value + (w[1].value - w[0].value) * s;
        }
    }
    points.last().unwrap().value
}
fn eq(points: &[BandGainPoint], at: u64) -> [f64; 3] {
    std::array::from_fn(|i| {
        value(
            &points
                .iter()
                .map(|p| ScalarPoint {
                    output_frame: p.output_frame,
                    value: p.gains[i],
                })
                .collect::<Vec<_>>(),
            at,
        )
    })
}
fn transpose(chroma: [f64; 12], semitones: f64) -> [f64; 12] {
    let shift = semitones.round() as i32;
    let mut out = [0.; 12];
    for (i, x) in chroma.into_iter().enumerate() {
        out[(i as i32 + shift).rem_euclid(12) as usize] += x;
    }
    out
}

#[derive(Clone)]
struct Audible {
    side: usize,
    lane: StemLane,
    gain: f64,
    stats: Stats,
    source_frame: f64,
    transposed: f64,
}
fn lead(s: &Stats) -> f64 {
    // A failed monophonic transcription does not prove a lead is absent.
    s.lead.max(0.5 * s.tonal * s.activity).clamp(0., 1.)
}
fn density(layers: &[Audible], reference: f64) -> f64 {
    let active = layers
        .iter()
        .filter(|l| l.gain >= 0.10 && l.stats.activity >= 0.6 && l.stats.rms * l.gain >= 0.002)
        .count();
    let energy = layers
        .iter()
        .map(|l| (l.stats.rms * l.gain).powi(2))
        .sum::<f64>()
        .sqrt();
    let weight = layers
        .iter()
        .map(|l| (l.stats.rms * l.gain).powi(2))
        .sum::<f64>();
    let crest = if weight > 1e-12 {
        layers
            .iter()
            .map(|l| l.stats.crest * (l.stats.rms * l.gain).powi(2))
            .sum::<f64>()
            / weight
    } else {
        18.
    };
    let raw = 0.40 * (active as f64 / 4.).min(1.)
        + 0.30 * (energy / reference.max(1e-9)).min(1.)
        + 0.30 * (1. - crest / 18.).clamp(0., 1.);
    if active < 3 || energy < 0.015 {
        raw.min(0.61)
    } else {
        raw
    }
}

fn profile(
    t: &TacticPlan,
    records: [&TacticTrackAnalysis; 2],
    placements: [&Placement; 2],
    facts: &CandidateFacts,
) -> Option<PairProfile> {
    profile_with_hold(t, records, placements, facts, None)
}
fn profile_with_hold(
    t: &TacticPlan,
    records: [&TacticTrackAnalysis; 2],
    placements: [&Placement; 2],
    facts: &CandidateFacts,
    precise_hold: Option<&[Window; 2]>,
) -> Option<PairProfile> {
    let level = placements.map(|p| {
        p.automation
            .iter()
            .filter(|a| !a.mute)
            .map(|a| a.gain_linear)
            .fold(0., f64::max)
    });
    let mut harmonic: f64 = 0.;
    let mut conflict: f64 = 0.;
    let mut collision: f64 = 0.;
    let mut peak: f64 = 0.;
    let mut densities = vec![];
    let mut beds = vec![];
    let measure_stability = matches!(
        t.tactic_id,
        TacticId::M01 | TacticId::M02 | TacticId::S05 | TacticId::S06
    );
    let mut held_levels = [vec![], vec![]];
    let mut cache: BTreeMap<(usize, u64, u64), Window> = BTreeMap::new();
    for bounds in facts.bar_frames.windows(2) {
        let mut per_density = vec![];
        let mut per_bed = vec![];
        let base = [0usize, 1].map(|side| {
            let m = &placements[side].time_map;
            window(
                records[side],
                m.source_at(bounds[0] as f64).floor() as u64,
                m.source_at(bounds[1] as f64).ceil() as u64,
            )
        });
        let base = [base[0].as_ref()?, base[1].as_ref()?];
        let held = precise_hold
            .filter(|_| bounds[0] < t.hold_end_frame && t.hold_end_frame < bounds[1])
            .map_or(base, |w| [&w[0], &w[1]]);
        if measure_stability {
            for side in 0..2 {
                let placement = if side == 0 {
                    facts.a.placement
                } else {
                    facts.b.placement
                };
                let energy = t
                    .lanes
                    .iter()
                    .filter(|l| l.placement == placement)
                    .map(|l| {
                        let held_gain = value(&l.gain, t.start_frame);
                        let rms = held[side].lanes.get(&l.lane).map(|s| s.rms)?;
                        Some((rms * held_gain * level[side]).powi(2))
                    })
                    .collect::<Option<Vec<_>>>()?
                    .into_iter()
                    .sum::<f64>()
                    .sqrt();
                held_levels[side].push(energy);
            }
        }
        let mut phases: BTreeSet<u64> = (0..8)
            .map(|i| bounds[0] + (bounds[1] - bounds[0]) * i / 8)
            .collect();
        for l in &t.lanes {
            phases.extend(
                l.gain
                    .iter()
                    .filter(|p| p.output_frame >= bounds[0] && p.output_frame < bounds[1])
                    .map(|p| p.output_frame),
            );
        }
        if t.phrase_exit.is_some() {
            let points: Vec<_> = phases.iter().copied().collect();
            phases.extend(points.windows(2).map(|w| w[0] + (w[1] - w[0]) / 2));
        }
        for at in phases {
            let mut audible = vec![];
            let mut bound = 0.;
            for l in &t.lanes {
                let side = if l.placement == facts.a.placement {
                    0
                } else {
                    1
                };
                let mut gain = value(&l.gain, at) * level[side];
                let mut bands = eq(&l.eq, at);
                let mut transposed = 0.;
                let edit = t.source_edits.iter().find(|e| {
                    e.placement == l.placement
                        && e.lane == l.lane
                        && e.output_start_frame <= at
                        && at < e.output_end_frame
                });
                let mut capture = edit.map(|e| (e.source_start_frame, e.source_end_frame));
                if let Some(edit) = edit {
                    if let SourceEditKind::PitchSequence { notes } = &edit.kind {
                        if let Some(n) = notes
                            .iter()
                            .find(|n| n.start_frame <= at && at < n.end_frame)
                        {
                            gain *= n.gain;
                            transposed = n.semitones;
                        } else {
                            gain = 0.;
                        }
                    }
                }
                let mut fx_bound = 1.;
                let mut added_noise = 0.;
                for fx in t.effects.iter().filter(|e| {
                    e.placement == l.placement
                        && e.lane == l.lane
                        && e.start_frame <= at
                        && at < e.end_frame
                }) {
                    // A wet tail contains captured history, not the source bar
                    // currently underneath the deck's continuing transport.
                    // Include the full capture even while feeding: its spectral
                    // and peak summaries are a conservative bound, not a dry-bar
                    // claim about an echo or frozen chord.
                    if matches!(
                        fx.kind,
                        EffectKind::Delay { .. }
                            | EffectKind::Reverb { .. }
                            | EffectKind::Spiral { .. }
                    ) {
                        let map = &placements[side].time_map;
                        let start = map.source_at(fx.start_frame as f64).floor() as u64;
                        let end = map.source_at(fx.input_end_frame as f64).ceil() as u64;
                        capture =
                            Some(capture.map_or((start, end), |(a, b)| (a.min(start), b.max(end))));
                        gain = gain.max(
                            l.gain
                                .iter()
                                .filter(|p| {
                                    p.output_frame >= fx.start_frame
                                        && p.output_frame <= fx.input_end_frame
                                })
                                .map(|p| p.value * level[side])
                                .fold(0., f64::max),
                        );
                        for point in l.eq.iter().filter(|p| {
                            p.output_frame >= fx.start_frame && p.output_frame <= fx.input_end_frame
                        }) {
                            for (band, captured) in bands.iter_mut().zip(point.gains) {
                                *band = band.max(captured);
                            }
                        }
                    }
                    let wet = value(&fx.wet, at);
                    fx_bound *= match &fx.kind {
                        EffectKind::Delay { feedback, .. }
                        | EffectKind::Spiral { feedback, .. } => {
                            (if at < fx.input_end_frame {
                                1. - wet
                            } else {
                                0.
                            }) + wet / (1. - feedback)
                        }
                        EffectKind::Reverb { .. } => {
                            (if at < fx.input_end_frame {
                                1. - wet
                            } else {
                                0.
                            }) + wet * 2.
                        }
                        EffectKind::Filter { resonance, .. } => {
                            (1. - wet) + wet * (resonance * 2f64.sqrt()).max(1.)
                        }
                        EffectKind::Noise { gain, .. } => {
                            added_noise += gain * wet;
                            1.
                        }
                        EffectKind::Gate {
                            period_frames,
                            phase_frames,
                            duty_cycle,
                            ..
                        } => {
                            let phase = (at - fx.start_frame + phase_frames) % period_frames;
                            if (phase as f64) < *period_frames as f64 * duty_cycle {
                                1.
                            } else {
                                1. - wet
                            }
                        }
                    };
                }
                let selected = if let Some((start, end)) = capture {
                    let key = (side, start, end);
                    if let std::collections::btree_map::Entry::Vacant(entry) = cache.entry(key) {
                        entry.insert(window(records[side], start, end)?);
                    }
                    &cache[&key]
                } else if at < t.hold_end_frame {
                    held[side]
                } else {
                    base[side]
                };
                let stats = if l.lane == StemLane::Stereo {
                    &selected.source
                } else {
                    selected.lanes.get(&l.lane)?
                };
                bound += gain
                    * (stats.peak * bands.into_iter().fold(0., f64::max) * fx_bound + added_noise);
                let selected_lanes: Vec<_> = if l.lane == StemLane::Stereo {
                    LANES.to_vec()
                } else {
                    vec![l.lane]
                };
                for lane in selected_lanes {
                    let stats = selected.lanes.get(&lane)?.clone();
                    let spectral_gain = (stats
                        .bands
                        .iter()
                        .zip(bands)
                        .map(|(weight, g)| weight * g * g)
                        .sum::<f64>())
                    .sqrt()
                    .min(1.);
                    let actual_gain = gain * spectral_gain * fx_bound;
                    if actual_gain > 1e-5 && stats.rms > 0.0005 {
                        audible.push(Audible {
                            side,
                            lane,
                            gain: actual_gain,
                            stats,
                            source_frame: placements[side].time_map.source_at(at as f64),
                            transposed,
                        });
                    }
                }
            }
            peak = peak.max(bound);
            let mut chromas = [[0.; 12]; 2];
            let mut tonal_active = [false; 2];
            let mut leads = [0f64; 2];
            let mut bass = [0f64; 2];
            let mut drums = [0f64; 2];
            for l in &audible {
                if l.lane == StemLane::Bass {
                    bass[l.side] = bass[l.side].max(l.gain * l.stats.activity);
                }
                if l.lane == StemLane::Drums {
                    drums[l.side] = drums[l.side].max(l.gain * l.stats.activity);
                }
                if matches!(l.lane, StemLane::Other | StemLane::Vocals) {
                    leads[l.side] = leads[l.side].max(l.gain * lead(&l.stats));
                }
                if l.stats.tonal >= 0.4 && (l.lane != StemLane::Drums || l.stats.pitched >= 0.5) {
                    tonal_active[l.side] = true;
                    let c = transpose(l.stats.chroma, l.transposed);
                    for (i, c) in c.into_iter().enumerate() {
                        chromas[l.side][i] += c * (l.stats.rms * l.gain).powi(2);
                    }
                }
            }
            let tonal = if tonal_active.iter().all(|active| *active) {
                interval_distance(&chromas[0], &chromas[1])?
            } else {
                0.
            };
            harmonic = harmonic.max(tonal);
            let mut note_conflict: f64 = 0.;
            for a in audible
                .iter()
                .filter(|a| a.side == 0 && a.stats.pitched >= 0.5)
            {
                for b in audible
                    .iter()
                    .filter(|b| b.side == 1 && b.stats.pitched >= 0.5)
                {
                    let an = a.stats.notes.iter().find(|n| {
                        n.start as f64 <= a.source_frame
                            && a.source_frame < n.end as f64
                            && n.score >= 0.85
                    });
                    let bn = b.stats.notes.iter().find(|n| {
                        n.start as f64 <= b.source_frame
                            && b.source_frame < n.end as f64
                            && n.score >= 0.85
                    });
                    if let (Some(an), Some(bn)) = (an, bn) {
                        let interval = ((an.midi + a.transposed - bn.midi - b.transposed).round()
                            as i32)
                            .rem_euclid(12);
                        if ![0, 3, 4, 5, 7, 8, 9].contains(&interval) {
                            note_conflict = note_conflict.max(a.gain.min(b.gain));
                        }
                    }
                }
            }
            conflict =
                conflict.max((leads[0].min(leads[1]) * (0.5 + 0.5 * tonal)).max(note_conflict));
            collision = collision
                .max(bass[0].min(bass[1]).powi(2))
                .max(drums[0].min(drums[1]).powi(2))
                .max(conflict);
            let reference_window = if at < t.hold_end_frame { held } else { base };
            let reference = (reference_window[0].source.rms * level[0])
                .max(reference_window[1].source.rms * level[1]);
            if bounds[0] >= t.hold_end_frame || at < t.hold_end_frame {
                per_density.push(density(&audible, reference));
            }
            let bed: Vec<_> = audible
                .into_iter()
                .filter(|l| l.side == 0 && l.lane != StemLane::Drums)
                .collect();
            per_bed.push(density(&bed, base[0].source.rms * level[0]));
        }
        densities.push(per_density.into_iter().fold(1., f64::min));
        beds.push(per_bed.into_iter().fold(0., f64::max));
    }
    let held_bars = t
        .bar_frames
        .iter()
        .take_while(|&&at| at < t.hold_end_frame)
        .count();
    Some(PairProfile{method:METHOD.into(),evidence:vec![format!("source analysis {} / {}; actual {:?} lane curves, edit captures and bounded FX; harmony is added incompatible pitch-class interval mass over within-passage baselines plus baseline-weighted spectral disagreement; stability uses separate sustained-lane source RMS before intentional fades; summaries are measured heuristics, summed peak is an upper bound, RMS combination assumes uncorrelated layers",records[0].tactic_analysis_id,records[1].tactic_analysis_id,t.tactic_id)],preflight_rejection:None,harmonic_distance:Some(harmonic),lead_conflict:Some(conflict.clamp(0.,1.)),complementarity:Some((1.-collision).clamp(0.,1.)),predicted_peak_dbfs:Some(db(peak)),selected_composite_density:Some(densities),retained_bed_density:Some(beds),outgoing_layer_stability:measure_stability.then(||layer_stability(&held_levels[0], held_bars)),incoming_layer_stability:measure_stability.then(||layer_stability(&held_levels[1], held_bars))})
}

#[derive(Clone, Debug)]
struct Word {
    start: u64,
    end: u64,
    text: String,
    score: f64,
}
fn normalized_word(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
fn words(record: &TacticTrackAnalysis) -> Vec<Word> {
    let normalized = record
        .signal_evidence
        .get("vocal_evidence")
        .unwrap_or(&Value::Null);
    let mut result: Vec<_> = array(normalized, "words")
        .iter()
        .filter_map(|v| {
            let start = frame(v, "start_frame")?;
            let end = frame(v, "end_frame")?;
            let score = number(v, "score")?;
            let text = normalized_word(v.get("text")?.as_str()?);
            (start < end && score >= 0.85 && !text.is_empty()).then_some(Word {
                start,
                end,
                text,
                score,
            })
        })
        .collect();
    result.sort_by_key(|w| w.start);
    result
}
fn mapped_words(
    record: &TacticTrackAnalysis,
    p: &Placement,
    phrases: &[VocalPhrase],
) -> Vec<Vec<Word>> {
    let mapped: Vec<_> = words(record)
        .into_iter()
        .filter_map(|mut w| {
            w.start = output_at(&p.time_map, w.start as f64)?.round() as u64;
            w.end = output_at(&p.time_map, w.end as f64)?.round() as u64;
            Some(w)
        })
        .collect();
    phrases
        .iter()
        .filter(|p| p.entry_supported && p.exit_supported && p.confidence_margin >= 0.25)
        .map(|p| {
            mapped
                .iter()
                .filter(|w| w.start >= p.start_frame && w.end <= p.end_frame)
                .cloned()
                .collect()
        })
        .collect()
}
fn semantic_relation(
    records: [&TacticTrackAnalysis; 2],
    placements: [&Placement; 2],
    facts: &CandidateFacts,
) -> Option<SemanticRelation> {
    let a = mapped_words(records[0], placements[0], facts.a.vocal_phrases.as_ref()?);
    let b = mapped_words(records[1], placements[1], facts.b.vocal_phrases.as_ref()?);
    let guard = (facts.output_sample_rate_hz as f64 * 0.300).ceil() as u64;
    let stop = [
        "the", "and", "that", "this", "with", "from", "your", "you", "have", "will", "just",
        "what", "when", "then", "they", "them", "love", "baby", "yeah", "oh", "ah", "och", "att",
        "jag", "det",
    ];
    for left in &a {
        for right in &b {
            let (Some(last), Some(first)) = (left.last(), right.first()) else {
                continue;
            };
            if last.end.saturating_add(guard) > first.start {
                continue;
            }
            let bigram = left.len() >= 2
                && right.len() >= 2
                && left[left.len() - 2].text == right[0].text
                && last.text == right[1].text;
            let single = last.text == first.text
                && last.text.chars().count() >= 4
                && !stop.contains(&last.text.as_str());
            if bigram || single {
                let phrase = if bigram {
                    format!("{} {}", left[left.len() - 2].text, last.text)
                } else {
                    last.text.clone()
                };
                let score = if bigram {
                    left[left.len() - 2]
                        .score
                        .min(last.score)
                        .min(first.score)
                        .min(right[1].score)
                } else {
                    last.score.min(first.score)
                };
                return Some(SemanticRelation{relation:format!("recognized_lyrical_handoff:{phrase}"),confidence_margin:score-0.5,evidence:vec![format!("{} / {}; complete aligned phrases share an ending/entry word sequence; literal wordplay only, no inferred question/answer meaning",records[0].tactic_analysis_id,records[1].tactic_analysis_id)]});
            }
        }
    }
    None
}
fn motif_notes(record: &TacticTrackAnalysis, motif: &Value) -> Vec<Note> {
    let inline = array(motif, "notes");
    if !inline.is_empty() {
        return notes(inline);
    }
    let lane = motif.get("lane").and_then(Value::as_str).unwrap_or("");
    let source = record
        .signal_evidence
        .get("pitch_analysis")
        .and_then(|v| v.get(lane))
        .unwrap_or(&Value::Null);
    let source = array(source, "notes");
    let selected: Vec<_> = array(motif, "note_indices")
        .iter()
        .filter_map(|i| source.get(i.as_u64()? as usize).cloned())
        .collect();
    notes(&selected)
}
fn notes_for_motif(record: &TacticTrackAnalysis, motif: &MotifEvidence) -> Vec<Note> {
    let source = record
        .signal_evidence
        .get("pitch_analysis")
        .and_then(|v| v.get(name(motif.lane)))
        .unwrap_or(&Value::Null);
    notes(array(source, "notes"))
        .into_iter()
        .filter(|n| n.start >= motif.source_start_frame && n.end <= motif.source_end_frame)
        .collect()
}
fn shared_motif(
    records: [&TacticTrackAnalysis; 2],
    placements: [&Placement; 2],
    facts: &mut CandidateFacts,
) {
    let (Some(am), Some(bm)) = (facts.a.motif.as_ref(), facts.b.motif.as_ref()) else {
        return;
    };
    let an = notes_for_motif(records[0], am);
    let bn = notes_for_motif(records[1], bm);
    if an.len() < 3 || an.len() != bn.len() || an.iter().chain(&bn).any(|n| n.score < 0.85) {
        return;
    }
    let ad = (an.last().unwrap().end - an[0].start) as f64;
    let bd = (bn.last().unwrap().end - bn[0].start) as f64;
    if an.iter().zip(&bn).any(|(a, b)| {
        ((a.midi - an[0].midi) - (b.midi - bn[0].midi)).abs() > 0.5
            || ((a.start - an[0].start) as f64 / ad - (b.start - bn[0].start) as f64 / bd).abs()
                > 0.04
    }) {
        return;
    }
    // A single supported fundamental supplies the sampler; the complete motif
    // remains the evidence for melodic overlay and recognizable B teasing.
    let Some(sample) = an
        .iter()
        .filter(|n| n.score >= 0.90)
        .max_by_key(|n| n.end - n.start)
    else {
        return;
    };
    let sample = MotifSample {
        source_start_frame: sample.start,
        source_end_frame: sample.end,
        midi_note: sample.midi,
        single_fundamental_score: sample.score,
    };
    let mut sequence = vec![];
    for note in &bn {
        let (Some(start), Some(end)) = (
            output_at(&placements[1].time_map, note.start as f64),
            output_at(&placements[1].time_map, note.end as f64),
        ) else {
            return;
        };
        if start < facts.bar_frames[0] as f64
            || end > *facts.bar_frames.last().unwrap() as f64
            || end <= start
        {
            return;
        }
        let semitones = note.midi - sample.midi_note;
        if semitones.abs() > 12. {
            return;
        }
        sequence.push(PitchNote {
            start_frame: start.round() as u64,
            end_frame: end.round() as u64,
            semitones,
            gain: 0.65,
        });
    }
    facts.a.motif.as_mut().unwrap().sample = Some(sample);
    facts.pair.shared_motif = Some(sequence);
    let evidence=format!("{} / {}: matched {} supported pitch intervals with normalized onset error<=0.04, independent of filenames",records[0].tactic_analysis_id,records[1].tactic_analysis_id,an.len());
    facts.a.evidence.push(evidence.clone());
    facts.b.evidence.push(evidence);
}

fn version_identity(records: [&TacticTrackAnalysis; 2], facts: &mut CandidateFacts) {
    let direct = records[0].audio_hash == records[1].audio_hash;
    let am: Vec<_> = array(&records[0].signal_evidence, "motifs")
        .iter()
        .filter(|m| {
            motif_notes(records[0], m).len() >= 6 && number(m, "score").is_some_and(|s| s >= 0.90)
        })
        .collect();
    let bm: Vec<_> = array(&records[1].signal_evidence, "motifs")
        .iter()
        .filter(|m| {
            motif_notes(records[1], m).len() >= 6 && number(m, "score").is_some_and(|s| s >= 0.90)
        })
        .collect();
    let mut matches: Vec<(u64, u64, u64, u64, String)> = vec![];
    for a in am {
        for b in &bm {
            let (Some(af), Some(bf)) = (
                a.get("fingerprint").and_then(Value::as_str),
                b.get("fingerprint").and_then(Value::as_str),
            ) else {
                continue;
            };
            if af != bf {
                continue;
            }
            let (Some(a0), Some(a1), Some(b0), Some(b1)) = (
                frame(a, "start_frame"),
                frame(a, "end_frame"),
                frame(b, "start_frame"),
                frame(b, "end_frame"),
            ) else {
                continue;
            };
            if matches
                .iter()
                .all(|(x0, x1, y0, y1, _)| (a1 <= *x0 || a0 >= *x1) && (b1 <= *y0 || b0 >= *y1))
            {
                matches.push((a0, a1, b0, b1, af.into()));
            }
        }
    }
    let aw = words(records[0]);
    let bw = words(records[1]);
    let long_lyric = aw.windows(12).any(|a| {
        bw.windows(12)
            .any(|b| a.iter().zip(b).all(|(a, b)| a.text == b.text))
    });
    if direct || matches.len() >= 2 && long_lyric {
        let key = if direct {
            format!("audio:{}", records[0].audio_hash.digest)
        } else {
            let mut fingerprints: Vec<_> = matches.into_iter().map(|m| m.4).collect();
            fingerprints.sort();
            format!(
                "shared_arrangement:{}",
                hash_bytes(fingerprints.join("/").as_bytes()).digest
            )
        };
        facts.a.version_identity = Some(key.clone());
        facts.b.version_identity = Some(key);
        let proof = if direct {
            "identical canonical source audio"
        } else {
            "two disjoint supported pitch/rhythm motifs plus twelve identical recognized lyric words"
        };
        facts.a.evidence.push(format!(
            "version relation inferred from {proof}; titles and filenames unused"
        ));
    }
}

#[derive(Clone, Debug)]
struct Accent {
    frame: f64,
    low: f64,
    high: f64,
}
fn accents(
    record: &TacticTrackAnalysis,
    p: &Placement,
    start: u64,
    end: u64,
) -> Option<Vec<Accent>> {
    let mut events = BTreeMap::new();
    for bar in array(&record.signal_evidence, "bars") {
        let drums = bar.get("stems")?.get("drums")?;
        for onset in array(drums, "onsets") {
            let at = frame(onset, "frame")?;
            let Some(mapped) = output_at(&p.time_map, at as f64) else {
                continue;
            };
            if mapped < start as f64 || mapped >= end as f64 {
                continue;
            }
            let low = number(onset, "low_band_strength")?;
            let high = number(onset, "high_band_strength")?;
            if low < 0. || high < 0. {
                return None;
            }
            events.insert(
                at,
                Accent {
                    frame: mapped,
                    low,
                    high,
                },
            );
        }
    }
    Some(events.into_values().collect())
}
fn mapped_beats(record: &TacticTrackAnalysis, p: &Placement, start: u64, end: u64) -> Vec<f64> {
    array(&record.signal_evidence, "beat_frames")
        .iter()
        .filter_map(|n| output_at(&p.time_map, n.as_u64()? as f64))
        .filter(|n| *n >= start as f64 - 1. && *n <= end as f64 + 1.)
        .collect()
}
fn pulse_relation(
    records: [&TacticTrackAnalysis; 2],
    placements: [&Placement; 2],
    facts: &mut CandidateFacts,
) {
    let start = facts.bar_frames[0];
    let end = *facts.bar_frames.last().unwrap();
    let a = mapped_beats(records[0], placements[0], start, end);
    let b = mapped_beats(records[1], placements[1], start, end);
    if a.len() < 9 || b.len() < 9 {
        return;
    }
    let pulse = |v: &[f64]| {
        let mut d: Vec<_> = v.windows(2).map(|w| w[1] - w[0]).collect();
        d.sort_by(f64::total_cmp);
        d[d.len() / 2]
    };
    let (pa, pb) = (pulse(&a), pulse(&b));
    let ratio = pa / pb;
    let interpreted = if (ratio - 2.).abs() < 0.02 {
        2.
    } else if (ratio - 0.5).abs() < 0.01 {
        0.5
    } else {
        return;
    };
    let (slow, fast) = if interpreted == 2. {
        (&a, &b)
    } else {
        (&b, &a)
    };
    let tolerance = facts.output_sample_rate_hz as f64 * 0.030;
    let skew = slow
        .iter()
        .map(|s| {
            fast.iter()
                .map(|f| (s - f).abs())
                .fold(f64::INFINITY, f64::min)
        })
        .fold(0., f64::max);
    if skew > tolerance {
        return;
    }
    let (Some(aa), Some(bb)) = (
        accents(records[0], placements[0], start, end),
        accents(records[1], placements[1], start, end),
    ) else {
        return;
    };
    let slow_pulse = pa.max(pb);
    let mut low_a = vec![];
    let mut low_b = vec![];
    let mut high_a = vec![];
    let mut high_b = vec![];
    for &beat in slow.iter().take(slow.len() - 1) {
        let energy = |events: &[Accent], high: bool| {
            events
                .iter()
                .filter(|e| (e.frame - beat).abs() <= slow_pulse * 0.16)
                .map(|e| if high { e.high } else { e.low })
                .fold(0., f64::max)
        };
        low_a.push(energy(&aa, false));
        low_b.push(energy(&bb, false));
        high_a.push(energy(&aa, true));
        high_b.push(energy(&bb, true));
    }
    let similarity = |a: &[f64], b: &[f64]| -> Option<f64> {
        let na = a.iter().map(|x| x * x).sum::<f64>().sqrt();
        let nb = b.iter().map(|x| x * x).sum::<f64>().sqrt();
        if na < 0.003
            || nb < 0.003
            || a.iter().filter(|x| **x > 0.001).count() < 2
            || b.iter().filter(|x| **x > 0.001).count() < 2
        {
            return None;
        }
        Some((a.iter().zip(b).map(|(a, b)| a * b).sum::<f64>() / (na * nb)).clamp(0., 1.))
    };
    let (Some(low), Some(high)) = (similarity(&low_a, &low_b), similarity(&high_a, &high_b)) else {
        return;
    };
    // A flat impulse train cannot establish accent hierarchy. Each band needs
    // contrast and the dominant low/high positions must not all coincide.
    let contrast = |v: &[f64]| {
        let max = v.iter().copied().fold(0., f64::max);
        let min = v.iter().copied().fold(f64::INFINITY, f64::min);
        max > min * 1.8 + 0.001
    };
    if ![&low_a, &low_b, &high_a, &high_b]
        .iter()
        .all(|v| contrast(v))
    {
        return;
    }
    if similarity(&low_a, &high_a).is_none_or(|v| v > 0.9)
        || similarity(&low_b, &high_b).is_none_or(|v| v > 0.9)
    {
        return;
    }
    let pattern = low.min(high);
    if pattern < 0.85 {
        return;
    }
    facts.pair.pulse_ratio = Some(interpreted);
    facts.pair.groove_similarity = Some(pattern);
    facts.pair.beat_skew_ms = Some(skew / facts.output_sample_rate_hz as f64 * 1000.);
    if facts.timing_basis == TacticTimingBasis::Bars {
        facts.pair.tempo_maps_validated = true;
    }
    facts.a.evidence.push(format!("measured pulse mapping {interpreted}: slow-grid skew {:.3}ms, low/high attack-pattern cosine {:.3}/{:.3}; no categorical drum-name inference",skew/facts.output_sample_rate_hz as f64*1000.,low,high));
}

/// Replace only time-local energy/activity descriptors for a partial held bar.
/// Full-bar spectral/lead evidence and peak bounds remain conservative. The
/// unheld portion keeps its original descriptors and is assessed separately.
fn precise_hold_window(
    store: &mixlib_catalog::Store,
    record: &TacticTrackAnalysis,
    p: &Placement,
    first: u64,
    last: u64,
    cancel: &Cancellation,
) -> Result<Option<Window>> {
    use mixlib_audio::WavReader;
    let start = p.time_map.source_at(first as f64).ceil() as u64;
    let end = p.time_map.source_at(last as f64).floor() as u64;
    let Some(mut w) = window(record, start, end) else {
        return Ok(None);
    };
    let source = store.source(&store.track(&p.track_id)?)?;
    let measure = |path: &std::path::Path| -> Result<(f64, f64, Vec<f64>)> {
        let mut wav = WavReader::open(path)?;
        require(
            wav.format.sample_rate_hz == record.source_sample_rate_hz
                && wav.format.frames == record.source_frames
                && wav.format.channels == 2,
            "TACTIC_STEM_FORMAT",
            "partial-hold PCM timebase changed",
        )?;
        wav.seek_frame(start)?;
        let block = u64::from((record.source_sample_rate_hz / 50).max(1));
        let mut at = start;
        let (mut energy, mut peak) = (0., 0f64);
        let mut envelope = vec![];
        while at < end {
            cancel.check()?;
            let count = block.min(end - at) as usize;
            let pcm = wav.read_frames(count)?;
            require(
                pcm.len() == count,
                "TACTIC_STEM_FORMAT",
                "partial-hold PCM ended early",
            )?;
            let power = pcm
                .iter()
                .flatten()
                .map(|v| f64::from(*v).powi(2))
                .sum::<f64>();
            energy += power;
            peak = pcm
                .iter()
                .flatten()
                .map(|v| f64::from(*v).abs())
                .fold(peak, f64::max);
            envelope.push((power / (2 * count) as f64).sqrt());
            at += count as u64;
        }
        let rms = (energy / (2 * (end - start)) as f64).sqrt();
        require(
            rms.is_finite() && peak.is_finite(),
            "TACTIC_STEM_FORMAT",
            "nonfinite partial-hold PCM",
        )?;
        Ok((rms, peak, envelope))
    };
    let source_levels = measure(&source)?;
    let threshold = (source_levels.0 * 0.04).max(0.0008);
    let update = |s: &mut Stats, (rms, peak, envelope): (f64, f64, Vec<f64>)| {
        s.rms = rms;
        s.peak = s.peak.max(peak);
        s.crest = if rms > 0. { db(peak / rms) } else { 0. };
        s.activity = envelope.iter().filter(|v| **v >= threshold).count() as f64
            / envelope.len().max(1) as f64;
    };
    update(&mut w.source, source_levels);
    for lane in LANES {
        let Some(asset) = record.stems.get(&lane) else {
            return Ok(None);
        };
        let levels = measure(&store.path(&asset.path)?)?;
        update(w.lanes.get_mut(&lane).unwrap(), levels);
    }
    Ok(Some(w))
}

pub(crate) fn refine_rhythmic_hold(
    store: &mixlib_catalog::Store,
    records: [&TacticTrackAnalysis; 2],
    placements: [&Placement; 2],
    facts: &mut CandidateFacts,
    cancel: &Cancellation,
) -> Result<()> {
    let Some(exit) = facts.pair.rhythmic_exit.clone() else {
        return Ok(());
    };
    let mut active = facts.clone();
    active.phrase_exit = Some(exit.clone());
    let policy = pair_policy(placements);
    let Ok(t) = candidate_tactic_schedule(TacticId::M01, &active, &policy) else {
        return Ok(());
    };
    let first = facts.bar_frames[facts.bar_frames.len() - 2];
    let Some(a) = precise_hold_window(
        store,
        records[0],
        placements[0],
        first,
        exit.start_frame,
        cancel,
    )?
    else {
        return Ok(());
    };
    let Some(b) = precise_hold_window(
        store,
        records[1],
        placements[1],
        first,
        exit.start_frame,
        cancel,
    )?
    else {
        return Ok(());
    };
    if let Some(mut measured) = profile_with_hold(&t, records, placements, &active, Some(&[a, b])) {
        measured.evidence.push("partial held bar RMS, crest and 20ms activity remeasured from source/stem PCM on the actual time maps; later energy change is excluded from the hold; full-bar pitch/lead and peak bounds retained".into());
        facts.pair.rhythmic_profiles.insert(TacticId::M01, measured);
    }
    Ok(())
}

fn pair_policy(placements: [&Placement; 2]) -> TacticPolicy {
    TacticPolicy {
        normal_speed_deviation: placements
            .iter()
            .map(|p| {
                (1. - p.time_map.speed_ratio_limits[0]).max(p.time_map.speed_ratio_limits[1] - 1.)
            })
            .fold(0.06, f64::max)
            .min(0.2),
        ..TacticPolicy::default()
    }
}

/// Enrich only from identity-checked persisted source evidence. This function
/// performs no model inference, file access, user communication or metadata writes.
pub(crate) fn enrich_pair_evidence(
    a: &TacticTrackAnalysis,
    b: &TacticTrackAnalysis,
    placements: [&Placement; 2],
    facts: &mut CandidateFacts,
) -> Result<()> {
    let records = [a, b];
    for side in 0..2 {
        require(
            records[side].audio_hash == placements[side].audio_hash
                && records[side].source_sample_rate_hz
                    == placements[side].time_map.source_sample_rate_hz,
            "TACTIC_PAIR_IDENTITY",
            "pair measurements belong to different source audio or timebase",
        )?;
        require(
            records[side]
                .signal_evidence
                .get("method")
                .and_then(Value::as_str)
                == Some("tactic_signal_evidence/1"),
            "TACTIC_PAIR_METHOD",
            "unsupported signal evidence method",
        )?;
    }
    pulse_relation(records, placements, facts);
    facts.validate()?;
    facts.pair.semantic_relation = semantic_relation(records, placements, facts);
    shared_motif(records, placements, facts);
    version_identity(records, facts);
    let policy = pair_policy(placements);
    for id in TacticId::ALL
        .into_iter()
        .filter(|id| !id.is_modifier() && id.permission() != TacticPermission::EmergencyOnly)
    {
        if let Some(rejection) = geometry_profile_rejection(id, facts) {
            facts.pair.profiles.insert(id, rejection);
            continue;
        }
        let (measured, reason) = match crate::tactics::candidate_tactic_schedule(id, facts, &policy)
        {
            Ok(t) => (
                profile(&t, records, placements, facts),
                "required source coverage is unknown".into(),
            ),
            Err(e) => (None, format!("{}: {}", e.code, e.message)),
        };
        facts.pair.profiles.insert(
            id,
            measured.unwrap_or_else(|| PairProfile {
                method: METHOD.into(),
                evidence: vec![format!("{id:?}: {reason}; no compatibility inferred")],
                ..PairProfile::default()
            }),
        );
    }
    if facts.a.phrase_exit.is_some() {
        let mut swift = facts.clone();
        swift.phrase_exit = facts.a.phrase_exit.clone();
        facts.pair.swift_profiles = swift_profiles(records, placements, &swift, &policy);
    }
    enrich_rhythmic_exit(records, placements, facts);
    Ok(())
}

fn geometry_profile_rejection(id: TacticId, facts: &CandidateFacts) -> Option<PairProfile> {
    let decision = window_geometry_rejection(id, facts)?;
    Some(PairProfile {
        method: METHOD.into(),
        preflight_rejection: Some(decision),
        ..PairProfile::default()
    })
}

fn swift_profiles(
    records: [&TacticTrackAnalysis; 2],
    placements: [&Placement; 2],
    swift: &CandidateFacts,
    policy: &TacticPolicy,
) -> BTreeMap<TacticId, PairProfile> {
    TacticId::ALL
        .into_iter()
        .filter(|id| sustained_tactic(*id))
        .map(|id| {
            if let Some(rejection) = geometry_profile_rejection(id, swift) {
                return (id, rejection);
            }
            let measured = candidate_tactic_schedule(id, swift, policy).and_then(|plan| {
                profile(&plan, records, placements, swift).ok_or_else(|| {
                    Error::new("TACTIC_PAIR", "source coverage unknown for swift exit")
                })
            });
            (
                id,
                measured.unwrap_or_else(|error| PairProfile {
                    method: METHOD.into(),
                    evidence: vec![format!("swift exit: {}: {}", error.code, error.message)],
                    ..PairProfile::default()
                }),
            )
        })
        .collect()
}

pub(crate) fn enrich_later_swift_exits(
    records: [&TacticTrackAnalysis; 2],
    placements: [&Placement; 2],
    facts: &mut CandidateFacts,
    cancel: &Cancellation,
) -> Result<()> {
    if facts.a.later_phrase_exits.is_empty() {
        return Ok(());
    }
    let policy = pair_policy(placements);
    let mut active = facts.clone();
    active.a.later_phrase_exits.clear();
    for exit in &facts.a.later_phrase_exits {
        cancel.check()?;
        active.a.phrase_exit = Some(exit.clone());
        active.phrase_exit = Some(exit.clone());
        facts.pair.later_swift_profiles.insert(
            exit.start_frame,
            swift_profiles(records, placements, &active, &policy),
        );
    }
    Ok(())
}

pub(crate) fn enrich_rhythmic_exit(
    records: [&TacticTrackAnalysis; 2],
    placements: [&Placement; 2],
    facts: &mut CandidateFacts,
) {
    if facts.pair.rhythmic_exit.is_some() {
        let policy = pair_policy(placements);
        let mut rhythmic = facts.clone();
        rhythmic.phrase_exit = facts.pair.rhythmic_exit.clone();
        let measured = candidate_tactic_schedule(TacticId::M01, &rhythmic, &policy).and_then(|t| {
            profile(&t, records, placements, &rhythmic).ok_or_else(|| {
                Error::new(
                    "TACTIC_PAIR",
                    "source coverage unknown for rhythmic handoff",
                )
            })
        });
        facts.pair.rhythmic_profiles.insert(
            TacticId::M01,
            measured.unwrap_or_else(|e| PairProfile {
                method: METHOD.into(),
                evidence: vec![format!("swift exit: {}: {}", e.code, e.message)],
                ..PairProfile::default()
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const BAR: u64 = 96_000;

    fn fixture(side: usize, facts: &CandidateFacts) -> (TacticTrackAnalysis, Placement) {
        let end = facts.a.source.end_frame;
        let file = FileRef {
            path: "fixture.json".into(),
            hash: hash_bytes(b"fixture"),
        };
        let hash = hash_bytes(&[side as u8]);
        let placement = Placement {
            track_id: format!("track-{side}"),
            audio_hash: hash.clone(),
            audio_revision: 1,
            analysis: file.clone(),
            override_revision: 0,
            source_start_frame: 0,
            source_end_frame: end,
            time_map: TimeMap {
                header: Header::default(),
                map_id: format!("map-{side}"),
                source_sample_rate_hz: 48_000,
                output_sample_rate_hz: 48_000,
                interpolation: "monotone_cubic".into(),
                anchors: vec![
                    Anchor {
                        source_frame: 0,
                        output_frame: 0,
                    },
                    Anchor {
                        source_frame: end,
                        output_frame: end,
                    },
                ],
                speed_ratio_limits: [0.94, 1.06],
                max_speed_change_per_second: 0.02,
                preserve_pitch: true,
            },
            automation: vec![AutomationPoint {
                output_frame: 0,
                gain_linear: 1.,
                bands_linear: [1.; 3],
                mute: false,
            }],
        };
        let descriptor = json!({"rms":0.04,"peak":0.08,"activity_fraction":0.9,"crest_db":6.,
            "tonalness":0.0,"foreground_lead_score":0.,"pitched_fraction":0.,
            "low_fraction":0.3,"mid_fraction":0.4,"high_fraction":0.3,
            "chroma":[1.,0.,0.,0.,0.,0.,0.,0.,0.,0.,0.,0.],"pitch_notes":[],"onsets":[]});
        let bars: Vec<_> = (0..end/BAR).map(|i| json!({"start_frame":i*BAR,"end_frame":(i+1)*BAR,
            "source":descriptor,"stems":{"drums":descriptor,"bass":descriptor,"other":descriptor,"vocals":descriptor}})).collect();
        let record = TacticTrackAnalysis {
            header: Header::default(),
            tactic_analysis_id: format!("analysis-{side}"),
            track_id: placement.track_id.clone(),
            audio_hash: hash,
            audio_revision: 1,
            source_sample_rate_hz: 48_000,
            source_frames: end,
            base_analysis: file.clone(),
            beat_artifact: file,
            override_revision: 0,
            method: TACTIC_ANALYSIS_METHOD.into(),
            implementation_hash: hash_bytes(b"implementation"),
            component_hashes: BTreeMap::new(),
            cache_key: hash_bytes(b"cache"),
            models: BTreeMap::new(),
            stems: BTreeMap::new(),
            vocal_evidence: json!({}),
            signal_evidence: json!({"method":"tactic_signal_evidence/1","bars":bars,"beat_frames":[],"vocal_evidence":{"words":[]}}),
            limitations: vec![],
            created_at: "fixture".into(),
        };
        (record, placement)
    }

    #[test]
    fn partial_hold_uses_retained_energy_without_erasing_later_peak_bounds() {
        let mut f = crate::tactics::tests::facts(8);
        let exit = PhraseExit {
            start_frame: 7 * BAR + BAR / 2,
            end_frame: 7 * BAR + BAR / 2 + 3840,
            kind: PhraseExitKind::AccompanimentPause,
            evidence: "partial-window fixture".into(),
        };
        f.a.phrase_exit = Some(exit.clone());
        f.phrase_exit = Some(exit);
        let (mut a, pa) = fixture(0, &f);
        let (mut b, pb) = fixture(1, &f);
        let held = [
            window(&a, 6 * BAR, 7 * BAR).unwrap(),
            window(&b, 6 * BAR, 7 * BAR).unwrap(),
        ];
        for lane in ["drums", "bass", "other", "vocals"] {
            a.signal_evidence["bars"][7]["stems"][lane]["rms"] = json!(0.001);
        }
        b.signal_evidence["bars"][7]["stems"]["other"]["peak"] = json!(0.9);
        let t = candidate_tactic_schedule(TacticId::M01, &f, &TacticPolicy::default()).unwrap();
        let coarse = profile(&t, [&a, &b], [&pa, &pb], &f).unwrap();
        let exact = profile_with_hold(&t, [&a, &b], [&pa, &pb], &f, Some(&held)).unwrap();
        assert!(
            exact.selected_composite_density.as_ref().unwrap()[7]
                > coarse.selected_composite_density.as_ref().unwrap()[7]
        );
        assert!(
            exact.outgoing_layer_stability.as_ref().unwrap()[7]
                > coarse.outgoing_layer_stability.as_ref().unwrap()[7]
        );
        assert!(exact.predicted_peak_dbfs.unwrap() >= coarse.predicted_peak_dbfs.unwrap());
    }

    #[test]
    fn structural_coverage_uses_explicit_unmetered_windows_and_rejects_gaps() {
        let f = crate::tactics::tests::facts(8);
        let (mut record, _) = fixture(0, &f);
        let first = record.signal_evidence["bars"]
            .as_array_mut()
            .unwrap()
            .remove(0);
        assert!(window(&record, 0, BAR * 2).is_none());
        record.signal_evidence["unmetered_windows"] = json!([first]);
        let measured = window(&record, 0, BAR * 2).unwrap();
        assert!((measured.source.rms - 0.04).abs() < 1e-9);
        record.signal_evidence["unmetered_windows"][0]["start_frame"] = json!(1);
        assert!(window(&record, 0, BAR * 2).is_none());
    }

    #[test]
    fn captured_echo_uses_earlier_spectrum_even_after_its_input_lane_closes() {
        let f = crate::tactics::tests::facts(8);
        let (mut a, pa) = fixture(0, &f);
        let (mut b, pb) = fixture(1, &f);
        for row in a.signal_evidence["bars"].as_array_mut().unwrap() {
            let early = row["start_frame"].as_u64().unwrap() < BAR;
            row["stems"]["other"]["tonalness"] = json!(if early { 1. } else { 0. });
        }
        for row in b.signal_evidence["bars"].as_array_mut().unwrap() {
            row["stems"]["other"]["tonalness"] = json!(1.);
            row["stems"]["other"]["chroma"] =
                json!([0., 1., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0.]);
        }
        let mut t = candidate_tactic_schedule(TacticId::S10, &f, &TacticPolicy::default()).unwrap();
        // Input gain closes after capture, as a renderer may do for echo-out.
        t.lanes[0].gain = vec![
            ScalarPoint {
                output_frame: 0,
                value: 1.,
            },
            ScalarPoint {
                output_frame: BAR,
                value: 0.,
            },
            ScalarPoint {
                output_frame: 8 * BAR,
                value: 0.,
            },
        ];
        let evidence = profile(&t, [&a, &b], [&pa, &pb], &f).unwrap();
        assert!(evidence.harmonic_distance.unwrap() > 0.9);
        assert!(evidence.predicted_peak_dbfs.unwrap() > db(0.08));
    }

    #[test]
    fn selected_lane_profiles_are_distinct_and_missing_measurements_never_fall_back() {
        let mut f = crate::tactics::tests::facts(8);
        let (mut a, pa) = fixture(0, &f);
        let (mut b, pb) = fixture(1, &f);
        for record in [&mut a, &mut b] {
            for row in record.signal_evidence["bars"].as_array_mut().unwrap() {
                for lane in ["bass", "other", "vocals"] {
                    row["stems"][lane]["rms"] = json!(0.);
                    row["stems"][lane]["activity_fraction"] = json!(0.);
                }
            }
        }
        enrich_pair_evidence(&a, &b, [&pa, &pb], &mut f).unwrap();
        let drum_handoff = &f.pair.profiles[&TacticId::M01];
        let stereo_fade = &f.pair.profiles[&TacticId::S13];
        assert!(drum_handoff.complementarity.unwrap() > stereo_fade.complementarity.unwrap());
        assert_eq!(
            drum_handoff
                .selected_composite_density
                .as_ref()
                .unwrap()
                .len(),
            8
        );
        f.pair.profiles.insert(
            TacticId::S13,
            PairProfile {
                method: METHOD.into(),
                ..Default::default()
            },
        );
        let decision = select_tactics(&f, &TacticPolicy::default()).unwrap();
        assert!(decision
            .rejected
            .iter()
            .any(|r| r.tactic_id == TacticId::S13
                && r.reasons.iter().any(|r| r.contains("headroom"))));
        b.audio_hash = a.audio_hash.clone();
        assert_eq!(
            enrich_pair_evidence(&a, &b, [&pa, &pb], &mut f)
                .unwrap_err()
                .code,
            "TACTIC_PAIR_IDENTITY"
        );
    }

    #[test]
    fn preflight_preserves_viable_profiles_and_skips_impossible_schedules() {
        use RealizationDecision::*;
        use TacticId::*;
        let mut f = crate::tactics::tests::facts(8);
        let (a, pa) = fixture(0, &f);
        let (b, pb) = fixture(1, &f);
        let placements = [&pa, &pb];
        let records = [&a, &b];
        let policy = pair_policy(placements);
        let measured_before = [M01, S13].map(|id| {
            let plan = candidate_tactic_schedule(id, &f, &policy).unwrap();
            serde_json::to_value(profile(&plan, records, placements, &f).unwrap()).unwrap()
        });
        // The profiling shape for a two-bar fade can be built at eight bars.
        // The selector must never admit it there, even with complete evidence.
        let plan = candidate_tactic_schedule(S02, &f, &policy).unwrap();
        assert!(profile(&plan, records, placements, &f).is_some());
        enrich_pair_evidence(&a, &b, placements, &mut f).unwrap();
        let skipped = &f.pair.profiles[&S02];
        assert_eq!(skipped.preflight_rejection, Some(IncompatibleBarCount));
        assert!(skipped.predicted_peak_dbfs.is_none());
        for (id, before) in [M01, S13].into_iter().zip(measured_before) {
            assert_eq!(serde_json::to_value(&f.pair.profiles[&id]).unwrap(), before);
        }

        f.timing_basis = TacticTimingBasis::Structural;
        f.bar_frames = vec![0, 8 * BAR];
        f.a.bars.clear();
        f.b.bars.clear();
        enrich_pair_evidence(&a, &b, placements, &mut f).unwrap();
        assert_eq!(
            f.pair.profiles[&S13].preflight_rejection,
            Some(IncompatibleTimingBasis)
        );
        assert_eq!(f.pair.profiles[&S07].preflight_rejection, None);
    }

    #[test]
    fn wordplay_requires_recognized_complete_aligned_phrases_and_guard() {
        let mut f = crate::tactics::tests::facts(8);
        let (mut a, pa) = fixture(0, &f);
        let (mut b, pb) = fixture(1, &f);
        let phrase = |start, end| VocalPhrase {
            start_frame: start,
            end_frame: end,
            tail_end_frame: end,
            entry_supported: true,
            exit_supported: true,
            confidence_margin: 0.4,
        };
        f.a.vocal_phrases = Some(vec![phrase(0, 24_000)]);
        f.b.vocal_phrases = Some(vec![phrase(48_000, 72_000)]);
        a.signal_evidence["vocal_evidence"]["words"] =
            json!([{"text":"Thunder!","start_frame":0,"end_frame":24_000,"score":0.95}]);
        b.signal_evidence["vocal_evidence"]["words"] =
            json!([{"text":"thunder","start_frame":48_000,"end_frame":72_000,"score":0.95}]);
        assert!(semantic_relation([&a, &b], [&pa, &pb], &f).is_some());
        f.b.vocal_phrases.as_mut().unwrap()[0].entry_supported = false;
        assert!(semantic_relation([&a, &b], [&pa, &pb], &f).is_none());
        f.b.vocal_phrases.as_mut().unwrap()[0].entry_supported = true;
        b.signal_evidence["vocal_evidence"]["words"][0]["score"] = json!(0.5);
        assert!(semantic_relation([&a, &b], [&pa, &pb], &f).is_none());
    }

    #[test]
    fn motif_sampler_captures_one_supported_note_and_rejects_different_intervals() {
        let mut f = crate::tactics::tests::facts(8);
        let (mut a, pa) = fixture(0, &f);
        let (mut b, pb) = fixture(1, &f);
        for (record, root) in [(&mut a, 60.), (&mut b, 62.)] {
            record.signal_evidence["pitch_analysis"] = json!({"other":{"notes":[
                {"start_frame":0,"end_frame":12_000,"midi_note":root,"score":0.95},
                {"start_frame":24_000,"end_frame":36_000,"midi_note":root+4.,"score":0.95},
                {"start_frame":48_000,"end_frame":60_000,"midi_note":root+7.,"score":0.95}]}});
        }
        f.pair.shared_motif = None;
        shared_motif([&a, &b], [&pa, &pb], &mut f);
        assert_eq!(f.pair.shared_motif.as_ref().unwrap().len(), 3);
        let capture = f.a.motif.as_ref().unwrap().sample.as_ref().unwrap();
        assert_eq!(
            capture.source_end_frame - capture.source_start_frame,
            12_000
        );
        assert!(capture.source_end_frame < f.a.motif.as_ref().unwrap().source_end_frame);
        b.signal_evidence["pitch_analysis"]["other"]["notes"][2]["midi_note"] = json!(70.);
        f.pair.shared_motif = None;
        shared_motif([&a, &b], [&pa, &pb], &mut f);
        assert!(f.pair.shared_motif.is_none());
    }

    #[test]
    fn double_time_needs_measured_alternating_accent_hierarchy() {
        let mut f = crate::tactics::tests::facts(8);
        let (mut a, pa) = fixture(0, &f);
        let (mut b, pb) = fixture(1, &f);
        for (record, period) in [(&mut a, 48_000u64), (&mut b, 24_000)] {
            record.signal_evidence["beat_frames"] = json!((0..=8 * BAR / period)
                .map(|i| i * period)
                .collect::<Vec<_>>());
            for row in record.signal_evidence["bars"].as_array_mut().unwrap() {
                let start = row["start_frame"].as_u64().unwrap();
                let events:Vec<_>=(start/48_000..(start+BAR)/48_000).map(|i|json!({"frame":i*48_000,"low_band_strength":if i%2==0{0.1}else{0.001},"high_band_strength":if i%2==1{0.1}else{0.001}})).collect();
                row["stems"]["drums"]["onsets"] = json!(events);
            }
        }
        f.pair.pulse_ratio = None;
        f.pair.tempo_maps_validated = false;
        pulse_relation([&a, &b], [&pa, &pb], &mut f);
        assert_eq!(f.pair.pulse_ratio, Some(2.));
        assert!(f.pair.tempo_maps_validated);
        for row in b.signal_evidence["bars"].as_array_mut().unwrap() {
            for event in row["stems"]["drums"]["onsets"].as_array_mut().unwrap() {
                event["low_band_strength"] = json!(0.1);
                event["high_band_strength"] = json!(0.1);
            }
        }
        f.pair.pulse_ratio = None;
        f.pair.tempo_maps_validated = false;
        pulse_relation([&a, &b], [&pa, &pb], &mut f);
        assert_eq!(f.pair.pulse_ratio, None);
        assert!(!f.pair.tempo_maps_validated);
    }

    #[test]
    fn unknown_optional_evidence_is_ineligible_without_panicking_for_any_tactic() {
        let mut f = crate::tactics::tests::facts(8);
        f.pair = PairEvidence::default();
        for passage in [&mut f.a, &mut f.b] {
            passage.vocal_phrases = None;
            passage.stems = None;
            passage.motif = None;
            passage.loop_candidates.clear();
            passage.version_identity = None;
            passage.roles.clear();
        }
        let (a, pa) = fixture(0, &f);
        let (b, pb) = fixture(1, &f);
        enrich_pair_evidence(&a, &b, [&pa, &pb], &mut f).unwrap();
        assert_eq!(f.pair.profiles.len(), 33);
        assert!(f
            .pair
            .profiles
            .values()
            .all(|p| p.predicted_peak_dbfs.is_none()));
        for id in TacticId::ALL {
            assert!(
                candidate_tactic_schedule(id, &f, &TacticPolicy::default()).is_err(),
                "{id:?}"
            );
        }
        f.a.vocal_phrases = Some(vec![]);
        f.b.vocal_phrases = Some(vec![]);
        for id in TacticId::ALL {
            let _ = candidate_tactic_schedule(id, &f, &TacticPolicy::default());
        }
        f.a.bars[0].beats_per_bar = Some(0);
        assert!(candidate_tactic_schedule(TacticId::S02, &f, &TacticPolicy::default()).is_err());
    }

    #[test]
    fn repeated_scale_is_not_a_new_clash_and_shifted_scales_still_differ() {
        let major = |root: usize| {
            std::array::from_fn(|pc| {
                if [0, 2, 4, 5, 7, 9, 11].contains(&((pc + 12 - root) % 12)) {
                    1.
                } else {
                    0.
                }
            })
        };
        for root in 0..12 {
            assert_eq!(interval_distance(&major(root), &major(root)), Some(0.));
            assert!(interval_distance(&major(root), &major((root + 1) % 12)).unwrap() > 0.30);
            assert!(interval_distance(&major(root), &major((root + 7) % 12)).unwrap() < 0.30);
        }
    }

    #[test]
    fn harmonic_fit_accepts_complementary_thirds_and_fifths_without_hiding_dissonance() {
        let pitch = |pc: usize| {
            let mut c = [0.; 12];
            c[pc] = 1.;
            c
        };
        for root in 0..12 {
            for interval in [3, 4, 7] {
                assert_eq!(
                    interval_distance(&pitch(root), &pitch((root + interval) % 12)),
                    Some(0.)
                );
            }
            for interval in [1, 2, 6, 10, 11] {
                assert_eq!(
                    interval_distance(&pitch(root), &pitch((root + interval) % 12)),
                    Some(1.)
                );
            }
        }
        let c_major = [1., 0., 0., 0., 1., 0., 0., 1., 0., 0., 0., 0.];
        let e_major = [0., 0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 1.];
        assert_eq!(interval_distance(&c_major, &c_major), Some(0.));
        // A wholesale major-third transposition does not automatically make
        // two complete triads compatible: their G/G# and C/B still conflict.
        assert!(interval_distance(&c_major, &e_major).unwrap() > 0.);
        assert_eq!(interval_distance(&[0.; 12], &c_major), None);
    }

    #[test]
    fn consonant_foreground_leads_still_compete_and_missing_chroma_stays_unknown() {
        let f = crate::tactics::tests::facts(8);
        let (mut a, pa) = fixture(0, &f);
        let (mut b, pb) = fixture(1, &f);
        for (record, pc) in [(&mut a, 0), (&mut b, 4)] {
            for row in record.signal_evidence["bars"].as_array_mut().unwrap() {
                row["stems"]["other"]["tonalness"] = json!(1.);
                row["stems"]["other"]["foreground_lead_score"] = json!(1.);
                let mut chroma = [0.; 12];
                chroma[pc] = 1.;
                row["stems"]["other"]["chroma"] = json!(chroma);
            }
        }
        let mut t = candidate_tactic_schedule(TacticId::M01, &f, &TacticPolicy::default()).unwrap();
        for lane in &mut t.lanes {
            if lane.lane == StemLane::Other {
                for point in &mut lane.gain {
                    point.value = 1.;
                }
            }
        }
        let p = profile(&t, [&a, &b], [&pa, &pb], &f).unwrap();
        assert_eq!(p.harmonic_distance, Some(0.));
        assert!(p.lead_conflict.unwrap() > TacticPolicy::default().max_lead_conflict);
        for row in b.signal_evidence["bars"].as_array_mut().unwrap() {
            row["stems"]["other"]["chroma"] = json!(vec![0.; 12]);
        }
        assert!(profile(&t, [&a, &b], [&pa, &pb], &f).is_none());
    }

    #[test]
    fn sustained_stability_ignores_muted_sources_and_intentional_fades_but_detects_drum_loss() {
        let f = crate::tactics::tests::facts(8);
        let (mut a, pa) = fixture(0, &f);
        let (mut b, pb) = fixture(1, &f);
        for row in a.signal_evidence["bars"].as_array_mut().unwrap() {
            row["stems"]["drums"]["rms"] =
                json!(if row["start_frame"].as_u64().unwrap() < 4 * BAR {
                    0.5
                } else {
                    0.
                });
        }
        for row in b.signal_evidence["bars"].as_array_mut().unwrap() {
            row["stems"]["other"]["rms"] =
                json!(if row["start_frame"].as_u64().unwrap() < 4 * BAR {
                    0.01
                } else {
                    0.8
                });
        }
        let t = candidate_tactic_schedule(TacticId::M01, &f, &TacticPolicy::default()).unwrap();
        let p = profile(&t, [&a, &b], [&pa, &pb], &f).unwrap();
        assert!(p
            .outgoing_layer_stability
            .unwrap()
            .iter()
            .all(|s| (*s - 1.).abs() < 1e-9));
        assert!(p
            .incoming_layer_stability
            .unwrap()
            .iter()
            .all(|s| (*s - 1.).abs() < 1e-9));
        for row in b.signal_evidence["bars"].as_array_mut().unwrap() {
            if row["start_frame"].as_u64().unwrap() >= 4 * BAR {
                row["stems"]["drums"]["rms"] = json!(0.);
            }
        }
        let p = profile(&t, [&a, &b], [&pa, &pb], &f).unwrap();
        assert_eq!(p.incoming_layer_stability.unwrap()[7], 0.);
        assert!(p
            .outgoing_layer_stability
            .unwrap()
            .iter()
            .all(|s| (*s - 1.).abs() < 1e-9));
    }
}
