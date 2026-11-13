//! Detect short exits in the retained accompaniment, never in muted drums.
//! These are candidate refinements: phrase, vocal, bleed and pair checks still
//! run on the actual schedule, and the ordinary resolution remains available.
use crate::{tactic_evidence::output_at, tactics::PassageFacts};
use mixlib_audio::WavReader;
use mixlib_catalog::Store;
use mixlib_types::*;

#[derive(Clone, Debug)]
struct Bin {
    start: u64,
    end: u64,
    rms: [f64; 2],
    peak: [f64; 2],
}

#[derive(Default)]
pub(crate) struct ExitScan {
    pub exits: Vec<PhraseExit>,
    pub complete: bool,
}

fn detect(bins: &[Bin], last_bar: u64, end: u64, rate: u32, earliest: u64) -> ExitScan {
    let reference = &bins[..bins.partition_point(|b| b.end <= last_bar)];
    if reference.len() < 10 {
        return ExitScan::default();
    }
    let baseline = [0, 1].map(|lane| {
        (reference.iter().map(|b| b.rms[lane].powi(2)).sum::<f64>() / reference.len() as f64).sqrt()
    });
    // A permanently empty bed is not a phrase-ending pause.
    if baseline.iter().all(|v| *v < 0.003) {
        return ExitScan {
            exits: vec![],
            complete: true,
        };
    }
    let half = last_bar + (end - last_bar) / 2;
    let fade = u64::from(rate) * 80 / 1000;
    let guard = u64::from(rate) * 20 / 1000;
    let energy = |b: &Bin| b.rms.iter().map(|v| v.powi(2)).sum::<f64>().sqrt();
    let mut exits = Vec::new();
    for (i, first) in bins.iter().enumerate() {
        if first.start < half || first.start + guard < earliest || first.start + guard + fade > end
        {
            continue;
        }
        let finish = first.start + guard + fade;
        let tail = &bins[i..i + bins[i..].partition_point(|b| b.start < finish)];
        if tail.last().is_none_or(|bin| bin.end < finish) {
            continue;
        }
        let pause = tail.iter().all(|b| {
            (0..2).all(|lane| {
                b.rms[lane] <= (baseline[lane] * 0.20).max(0.0005)
                    && b.peak[lane] <= (baseline[lane] * 0.8).max(0.003)
            })
        });
        // Three successive 100ms windows must rise into a real local peak;
        // all of the following 100ms must fall below half that peak. A rising
        // final fill alone cannot authorize cutting off the crescendo.
        let slice = u64::from(rate) / 10;
        let mean = |a: u64, z: u64| -> Option<f64> {
            let lo = bins.partition_point(|b| b.start < a);
            let hi = bins.partition_point(|b| b.end <= z);
            let rows = bins.get(lo..hi)?;
            (rows.len() >= 3).then(|| rows.iter().map(energy).sum::<f64>() / rows.len() as f64)
        };
        let levels: Option<Vec<_>> = (0..3)
            .map(|n| {
                mean(
                    first.start.saturating_sub((3 - n) * slice),
                    first.start.saturating_sub((2 - n) * slice),
                )
            })
            .collect();
        let release = levels.is_some_and(|v| {
            v[0] > 0.003
                && v[1] >= v[0] * 1.2
                && v[2] >= v[1] * 1.2
                && v[2] >= v[0] * 1.8
                && tail.iter().all(|b| energy(b) <= v[2] * 0.50)
        });
        if pause || release {
            exits.push(PhraseExit {
                start_frame: first.start + guard,
                end_frame: finish,
                kind: if pause { PhraseExitKind::AccompanimentPause } else { PhraseExitKind::CrescendoRelease },
                evidence: format!("retained bass+other PCM: {} in final half-bar; 20ms envelope, 20ms post-event guard, 80ms exchange; both lanes measured independently", if pause { "relative and absolute pause" } else { "three rising 100ms windows followed by sustained >=50% release" }),
            });
        }
    }
    ExitScan {
        exits,
        complete: true,
    }
}

pub(crate) fn measure(
    store: &Store,
    record: &TacticTrackAnalysis,
    placement: &Placement,
    anchors: &[u64],
    facts: &PassageFacts,
    cancel: &Cancellation,
) -> Result<ExitScan> {
    if anchors.len() < 5 || facts.stems.is_none() {
        return Ok(ExitScan::default());
    }
    let Some(assets) = [StemLane::Bass, StemLane::Other]
        .map(|l| record.stems.get(&l))
        .into_iter()
        .collect::<Option<Vec<_>>>()
    else {
        return Ok(ExitScan::default());
    };
    let map = &placement.time_map;
    let last_bar = anchors[anchors.len() - 2];
    let end = *anchors.last().unwrap();
    let source_start = map.source_at(anchors[anchors.len() - 3] as f64).ceil() as u64;
    let source_end = map.source_at(end as f64).floor() as u64;
    let rate = record.source_sample_rate_hz;
    let block = u64::from((rate / 50).max(1));
    let mut bins: Vec<Bin> = vec![];
    for (lane, asset) in assets.iter().enumerate() {
        let mut wav = WavReader::open(&store.path(&asset.path)?)?;
        require(
            wav.format.sample_rate_hz == rate
                && wav.format.frames == record.source_frames
                && wav.format.channels == 2,
            "TACTIC_STEM_FORMAT",
            "phrase-exit stem timebase changed",
        )?;
        wav.seek_frame(source_start)?;
        let mut at = source_start;
        let mut index = 0;
        while at + block <= source_end {
            cancel.check()?;
            let frames = wav.read_frames(block as usize)?;
            require(
                frames.len() == block as usize,
                "TACTIC_STEM_FORMAT",
                "phrase-exit source ended early",
            )?;
            let rms = (frames
                .iter()
                .flatten()
                .map(|v| f64::from(*v).powi(2))
                .sum::<f64>()
                / (2 * block) as f64)
                .sqrt();
            let peak = frames
                .iter()
                .flatten()
                .map(|v| f64::from(*v).abs())
                .fold(0., f64::max);
            require(
                rms.is_finite() && peak.is_finite(),
                "TACTIC_STEM_FORMAT",
                "nonfinite phrase-exit PCM",
            )?;
            if lane == 0 {
                let (Some(start), Some(end)) = (
                    output_at(map, at as f64),
                    output_at(map, (at + block) as f64),
                ) else {
                    return Ok(ExitScan::default());
                };
                bins.push(Bin {
                    start: start.ceil() as u64,
                    end: end.floor() as u64,
                    rms: [0.; 2],
                    peak: [0.; 2],
                });
            }
            bins[index].rms[lane] = rms;
            bins[index].peak[lane] = peak;
            index += 1;
            at += block;
        }
    }
    Ok(detect(
        &bins,
        last_bar,
        end,
        map.output_sample_rate_hz,
        facts.build_release_frame.unwrap_or(0),
    ))
}

/// A rhythm-led end to a dense mashup need not become quiet. Propose an
/// on-grid incoming drum accent in the final half-bar; actual extended-hold
/// density, harmony, isolation and vocals are admitted separately afterward.
pub(crate) fn rhythmic_exits(
    store: &Store,
    record: &TacticTrackAnalysis,
    placement: &Placement,
    facts: &crate::tactics::CandidateFacts,
    cancel: &Cancellation,
) -> Result<ExitScan> {
    if facts.timing_basis != TacticTimingBasis::Bars
        || facts.bar_frames.len() < 9
        || facts.a.endpoint_reasons.is_empty() && facts.b.endpoint_reasons.is_empty()
    {
        return Ok(ExitScan::default());
    }
    let Some(asset) = record.stems.get(&StemLane::Drums) else {
        return Ok(ExitScan::default());
    };
    let map = &placement.time_map;
    let bars = &facts.bar_frames;
    let last = bars[bars.len() - 2];
    let end = *bars.last().unwrap();
    let Some(meter) = facts.a.bars.last().and_then(|b| b.beats_per_bar) else {
        return Ok(ExitScan::default());
    };
    if meter < 2 {
        return Ok(ExitScan::default());
    }
    let mut wav = WavReader::open(&store.path(&asset.path)?)?;
    require(
        wav.format.sample_rate_hz == record.source_sample_rate_hz
            && wav.format.frames == record.source_frames
            && wav.format.channels == 2,
        "TACTIC_STEM_FORMAT",
        "rhythmic-exit drum timebase changed",
    )?;
    let mut complete = true;
    let mut rms = |a: u64, b: u64| -> Result<f64> {
        cancel.check()?;
        complete &=
            a >= map.anchors[0].output_frame && b <= map.anchors.last().unwrap().output_frame;
        let lo = map.source_at(a as f64).ceil() as u64;
        let hi = map.source_at(b as f64).floor() as u64;
        if hi <= lo {
            complete = false;
            return Ok(0.);
        }
        wav.seek_frame(lo)?;
        let mut energy = 0.;
        let mut remaining = hi - lo;
        while remaining > 0 {
            cancel.check()?;
            let count = remaining.min(65536) as usize;
            let samples = wav.read_frames(count)?;
            require(
                samples.len() == count,
                "TACTIC_STEM_FORMAT",
                "rhythmic-exit drum source ended early",
            )?;
            energy += samples
                .iter()
                .flatten()
                .map(|v| f64::from(*v).powi(2))
                .sum::<f64>();
            remaining -= count as u64;
        }
        let level = (energy / (2 * (hi - lo)) as f64).sqrt();
        require(
            level.is_finite(),
            "TACTIC_STEM_FORMAT",
            "nonfinite rhythmic-exit PCM",
        )?;
        Ok(level)
    };
    let reference = rms(bars[bars.len() - 3], last)?;
    let rate = u64::from(facts.output_sample_rate_hz);
    let exits = rhythmic_candidates(
        [last, end],
        meter,
        rate,
        facts.a.build_release_frame,
        reference,
        rms,
    )?;
    Ok(ExitScan { exits, complete })
}

fn rhythmic_candidates(
    [last, end]: [u64; 2],
    meter: u16,
    rate: u64,
    release: Option<u64>,
    reference: f64,
    mut rms: impl FnMut(u64, u64) -> Result<f64>,
) -> Result<Vec<PhraseExit>> {
    let mut exits = Vec::new();
    for pulse in meter.div_ceil(2)..meter {
        let at = last + (end - last) * u64::from(pulse) / u64::from(meter);
        let finish = at + rate * 80 / 1000;
        if finish > end || release.is_some_and(|r| r > at) {
            continue;
        }
        let before = rms(at.saturating_sub(rate / 10), at)?;
        let attack = rms(at, finish)?;
        if strong_accent(reference, before, attack) {
            exits.push(PhraseExit { start_frame: at, end_frame: finish,
                kind: PhraseExitKind::RhythmicHandoff,
                evidence: format!("incoming drum PCM accent on final-bar pulse {}/{} before a measured musical endpoint: 80ms RMS {:.3}x preceding-bar RMS and {:.3}x preceding 100ms; dense extended hold must pass independent pair admission; no accompaniment silence required", pulse+1,meter,attack/reference,attack/before.max(1e-9)) });
        }
    }
    Ok(exits)
}
fn strong_accent(reference: f64, before: f64, attack: f64) -> bool {
    reference >= 0.003 && attack >= reference * 1.2 && attack >= before * 1.4
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bins() -> Vec<Bin> {
        (0..200)
            .map(|n| Bin {
                start: n * 20,
                end: (n + 1) * 20,
                rms: [0.1; 2],
                peak: [0.2; 2],
            })
            .collect()
    }
    #[test]
    fn rhythmic_accent_requires_real_activity_and_local_attack() {
        assert!(strong_accent(0.04, 0.025, 0.072));
        assert!(!strong_accent(0.04, 0.070, 0.072));
        assert!(!strong_accent(0.04, 0.005, 0.02));
        assert!(!strong_accent(0.0001, 0.00001, 0.001));
    }
    #[test]
    fn all_measured_final_half_pulses_survive_the_detector() {
        let rms = |a, b| {
            Ok(if matches!((a, b), (3500, 3580) | (3750, 3830)) {
                0.08
            } else {
                0.02
            })
        };
        let starts = |release| {
            rhythmic_candidates([3000, 4000], 4, 1000, release, 0.04, rms)
                .unwrap()
                .into_iter()
                .map(|exit| exit.start_frame)
                .collect::<Vec<_>>()
        };
        assert_eq!(starts(None), [3500, 3750]);
        assert_eq!(starts(Some(3600)), [3750]);
    }

    #[test]
    fn an_early_pause_before_release_does_not_hide_a_later_pause() {
        let mut b = bins();
        for v in &mut b[160..167] {
            v.rms = [0.001; 2];
            v.peak = [0.002; 2];
        }
        for v in &mut b[185..192] {
            v.rms = [0.001; 2];
            v.peak = [0.002; 2];
        }
        let starts = |release| {
            detect(&b, 2000, 4000, 1000, release)
                .exits
                .into_iter()
                .map(|exit| exit.start_frame)
                .collect::<Vec<_>>()
        };
        assert_eq!(starts(0), [3220, 3240, 3260, 3720, 3740, 3760]);
        assert_eq!(starts(3500), [3720, 3740, 3760]);
    }
    #[test]
    fn pause_requires_both_retained_lanes_and_final_half_bar() {
        let mut b = bins();
        let empty = detect(&b, 2000, 4000, 1000, 0);
        assert!(empty.exits.is_empty() && empty.complete);
        let missing = detect(&b[..8], 2000, 4000, 1000, 0);
        assert!(missing.exits.is_empty() && !missing.complete);
        for v in &mut b[160..170] {
            v.rms[0] = 0.001;
            v.peak[0] = 0.002;
        }
        assert!(detect(&b, 2000, 4000, 1000, 0).exits.is_empty());
        for v in &mut b[160..170] {
            v.rms[1] = 0.001;
            v.peak[1] = 0.002;
        }
        let e = &detect(&b, 2000, 4000, 1000, 0).exits[0];
        assert_eq!((e.start_frame, e.end_frame), (3220, 3300));
        assert_eq!(e.kind, PhraseExitKind::AccompanimentPause);
        assert!(detect(&b[..162], 2000, 4000, 1000, 0).exits.is_empty());
        let mut early = bins();
        for v in &mut early[110..120] {
            v.rms = [0.001; 2];
            v.peak = [0.002; 2];
        }
        assert!(detect(&early, 2000, 4000, 1000, 0).exits.is_empty());
    }
    #[test]
    fn crescendo_must_release_before_exchange() {
        let mut b = bins();
        for (i, level) in [0.10, 0.15, 0.24].into_iter().enumerate() {
            for v in &mut b[160 + i * 5..165 + i * 5] {
                v.rms = [level; 2];
                v.peak = [level * 2.; 2];
            }
        }
        for v in &mut b[175..] {
            v.rms = [0.24; 2];
            v.peak = [0.48; 2];
        }
        assert!(detect(&b, 2000, 4000, 1000, 0).exits.is_empty());
        for v in &mut b[175..] {
            v.rms = [0.08; 2];
            v.peak = [0.16; 2];
        }
        let e = &detect(&b, 2000, 4000, 1000, 0).exits[0];
        assert_eq!(e.kind, PhraseExitKind::CrescendoRelease);
        assert!(e.start_frame > 3500);
    }
}
