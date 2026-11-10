//! Measured one- to four-bar tiles. No inferred vocals or fixed-meter repair.
use mixlib_types::*;
use std::collections::{BTreeMap, BTreeSet};

pub const SEGMENTS: &str = "bar-occupancy-novelty/1";

pub fn analyze_segments(
    beats: &Beats,
    features: &[FeatureFrame],
    format: &AudioFormat,
) -> Result<SegmentAnalysis> {
    beats.validate()?;
    require(
        features
            .windows(2)
            .all(|w| w[0].source_frame < w[1].source_frame),
        "FEATURE_ORDER",
        "segment input feature frames are not ordered",
    )?;
    let ids: BTreeSet<_> = beats.downbeat_ids.iter().collect();
    let downbeats: Vec<_> = beats
        .events
        .iter()
        .enumerate()
        .filter(|(_, beat)| ids.contains(&beat.event_id))
        .map(|(i, _)| i)
        .collect();
    let mut singles = Vec::new();
    for indices in downbeats.windows(2) {
        let (first, last) = (indices[0], indices[1]);
        let pulses = last - first;
        let start = beats.events[first].source_frame;
        let end = beats.events[last].source_frame;
        if !(2..=12).contains(&pulses)
            || end <= start
            || beats
                .unreliable_intervals
                .iter()
                .any(|r| r.start_frame < end && r.end_frame > start)
            || beats.meter_segments.iter().any(|m| {
                m.start_frame < end && m.end_frame > start && m.pulses_per_bar != pulses as u32
            })
            || beats.events[first..=last].iter().any(|b| b.score < 0.5)
        {
            continue;
        }
        if let Some(tile) = describe(
            features,
            &beats.events[first..=last],
            start,
            end,
            1,
            pulses as u32,
            format.sample_rate_hz,
        ) {
            singles.push(tile);
        }
    }
    let mut tiles = Vec::new();
    for i in 0..singles.len() {
        for count in 1..=4usize {
            let Some(window) = singles.get(i..i + count) else {
                break;
            };
            if window.windows(2).any(|w| {
                w[0].end_frame != w[1].start_frame || w[0].pulses_per_bar != w[1].pulses_per_bar
            }) {
                break;
            }
            let start = window[0].start_frame;
            let end = window.last().unwrap().end_frame;
            let lo = beats.events.partition_point(|b| b.source_frame < start);
            let hi = beats.events.partition_point(|b| b.source_frame <= end);
            if let Some(mut tile) = describe(
                features,
                &beats.events[lo..hi],
                start,
                end,
                count as u32,
                window[0].pulses_per_bar,
                format.sample_rate_hz,
            ) {
                let mean = window.iter().map(|b| b.rms).sum::<f64>() / count as f64;
                tile.level_variation = window.iter().map(|b| (b.rms - mean).abs()).sum::<f64>()
                    / (count as f64 * mean.max(1e-9));
                tile.harmonic_variation = window
                    .iter()
                    .map(|b| distance(&b.chroma, &tile.chroma))
                    .sum::<f64>()
                    / count as f64;
                tiles.push(tile);
            }
        }
    }
    let novelty: Vec<_> = singles
        .iter()
        .enumerate()
        .map(|(i, b)| {
            if i == 0 || singles[i - 1].end_frame != b.start_frame {
                return 0.;
            }
            let a = &singles[i - 1];
            let level = (a.rms - b.rms).abs() / a.rms.max(b.rms).max(1e-9);
            let spectrum = a
                .band_occupancy
                .iter()
                .zip(b.band_occupancy)
                .map(|(a, b)| (a - b).abs())
                .sum::<f64>()
                / 2.;
            let rhythm = (a.onset_rate_hz - b.onset_rate_hz).abs()
                / a.onset_rate_hz.max(b.onset_rate_hz).max(1.);
            (0.3 * level + 0.3 * spectrum + 0.25 * distance(&a.chroma, &b.chroma) + 0.15 * rhythm)
                .clamp(0., 1.)
        })
        .collect();
    let mut boundaries = Vec::new();
    for (i, tile) in singles.iter().enumerate() {
        if novelty[i] < 0.18 {
            continue;
        }
        // A period is evidence only with repeated salient changes and a
        // continuous, unchanged meter. It never claims a lyric has ended.
        let periodic = [32usize, 16, 8, 4].into_iter().find_map(|period| {
            let first = i.checked_sub(period * 2)?;
            if singles[first..=i].windows(2).any(|w| {
                w[0].end_frame != w[1].start_frame || w[0].pulses_per_bar != w[1].pulses_per_bar
            }) {
                return None;
            }
            (novelty[i - period] >= 0.18 && novelty[first] >= 0.18).then_some(period as u32)
        });
        boundaries.push(StructuralBoundary {
            source_frame: tile.start_frame, novelty: novelty[i],
            supported_period_bars: periodic, repeated_support: if periodic.is_some() { 2 } else { 0 },
            evidence: "adjacent-bar energy, spectral occupancy, chroma and onset change; repeated period is structural evidence, not phrase or lyric completion".into(),
        });
    }
    let mut unavailable = vec![
        "vocal_activity".into(),
        "vocal_phrase_completion".into(),
        "stem_quality".into(),
        "calibrated_loudness_lufs".into(),
        "foreground_lead_identity".into(),
    ];
    if singles.is_empty() {
        unavailable.push("reliable_bar_grid".into());
    }
    let record = SegmentAnalysis {
        header: Header::default(), segments_id: id(), source_sample_rate_hz: format.sample_rate_hz,
        source_frames: format.frames, method: SEGMENTS.into(),
        score_semantics: "bounded acoustic heuristics, not calibrated probabilities; RMS is not LUFS; unmeasured voice and separation stay null".into(),
        tiles, boundaries, unavailable,
    };
    record.validate()?;
    Ok(record)
}

fn distance(a: &[f64; 12], b: &[f64; 12]) -> f64 {
    (a.iter().zip(b).map(|(a, b)| (a - b).abs()).sum::<f64>() / 2.).clamp(0., 1.)
}

fn describe(
    features: &[FeatureFrame],
    beats: &[Beat],
    start: u64,
    end: u64,
    bars: u32,
    pulses: u32,
    rate: u32,
) -> Option<SegmentTile> {
    let lo = features.partition_point(|f| f.source_frame < start);
    let hi = features.partition_point(|f| f.source_frame < end);
    let window = &features[lo..hi];
    if window.is_empty() || beats.len() < 2 {
        return None;
    }
    let n = window.len() as f64;
    let seconds = (end - start) as f64 / f64::from(rate);
    let rms = (window.iter().map(|f| f.rms * f.rms).sum::<f64>() / n).sqrt();
    let peak = window.iter().map(|f| f.peak).fold(0., f64::max);
    let bands: [f64; 3] =
        std::array::from_fn(|c| window.iter().map(|f| f.band_energy[c]).sum::<f64>() / n);
    let norm = bands.iter().sum::<f64>().max(1e-12);
    let band_occupancy = bands.map(|e| (e / norm).clamp(0., 1.));
    let mut chroma: [f64; 12] =
        std::array::from_fn(|c| window.iter().map(|f| f.chroma[c] * f.rms).sum::<f64>());
    let norm = chroma.iter().sum::<f64>().max(1e-12);
    chroma
        .iter_mut()
        .for_each(|v| *v = (*v / norm).clamp(0., 1.));
    let threshold = (rms * 0.12).max(0.0005);
    let mut last_onset = None;
    let count = window
        .iter()
        .filter(|f| {
            let hit = f.onset >= threshold
                && last_onset.is_none_or(|last| f.source_frame - last >= u64::from(rate) / 12);
            if hit {
                last_onset = Some(f.source_frame)
            }
            hit
        })
        .count();
    let onset_rate_hz = count as f64 / seconds;
    let rest = (1. - rms / 0.002).clamp(0., 1.);
    let percussive = (onset_rate_hz / 3.).clamp(0., 1.);
    let sustained = (1. - percussive) * (1. - rest);
    let roles = BTreeMap::from([
        ("rest".into(), rest),
        ("sustained_texture".into(), sustained),
        ("low_percussion_bed".into(), (1. - percussive) * (1. - rest)),
        ("rhythmic_activity".into(), percussive * (1. - rest)),
        ("mixed".into(), (1. - rest) * (0.5 + 0.5 * percussive)),
    ]);
    Some(SegmentTile {
        start_frame: start,
        end_frame: end,
        bars,
        pulses_per_bar: pulses,
        pulse_score_min: beats.iter().map(|b| b.score).fold(1., f64::min),
        local_bpm: 60. * f64::from(pulses * bars) / seconds,
        rms,
        peak,
        crest_factor_db: if rms > 1e-12 {
            20. * (peak / rms).max(1.).log10()
        } else {
            0.
        },
        band_occupancy,
        onset_rate_hz,
        chroma,
        level_variation: 0.,
        harmonic_variation: 0.,
        roles,
        vocal_activity: None,
        separation_quality: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Beats, Vec<FeatureFrame>, AudioFormat) {
        let format = AudioFormat {
            sample_rate_hz: 48000,
            channels: 2,
            container_bits: 32,
            valid_bits: 32,
            channel_mask: 0,
            encoding: Encoding::IeeeFloat,
            frames: 48000 * 20,
        };
        let beats = Beats {
            header: Header::default(),
            beats_id: id(),
            source_sample_rate_hz: 48000,
            source_frames: format.frames,
            events: (0..=32)
                .map(|i| Beat {
                    event_id: format!("b{i}"),
                    source_frame: i * 24000,
                    score: 0.9,
                    local_bpm: Some(120.),
                })
                .collect(),
            downbeat_ids: (0..=32).step_by(4).map(|i| format!("b{i}")).collect(),
            meter_segments: vec![],
            unreliable_intervals: vec![],
            score_semantics: "fixture".into(),
        };
        let features = (0..160)
            .map(|i| FeatureFrame {
                source_frame: i * 4800,
                chroma: [1. / 12.; 12],
                band_energy: [0.01, 0.04, 0.01],
                rms: 0.1,
                peak: 0.2,
                onset: if i % 5 == 0 { 0.04 } else { 0. },
            })
            .collect();
        (beats, features, format)
    }
    #[test]
    fn describes_complete_variable_spans_without_inventing_vocals() {
        let (b, f, a) = fixture();
        let result = analyze_segments(&b, &f, &a).unwrap();
        assert_eq!(result.tiles.iter().filter(|t| t.bars == 1).count(), 8);
        assert_eq!(result.tiles.iter().filter(|t| t.bars == 4).count(), 5);
        let first = &result.tiles[0];
        assert!((first.local_bpm - 120.).abs() < 1e-9);
        assert!((first.crest_factor_db - 6.0206).abs() < 0.001);
        assert!((first.onset_rate_hz - 2.).abs() < 1e-9);
        assert!(result
            .tiles
            .iter()
            .all(|t| t.vocal_activity.is_none() && t.separation_quality.is_none()));
        assert!(result.boundaries.is_empty());
    }
    #[test]
    fn never_bridges_unreliable_bars_or_contradictory_meter() {
        let (mut b, f, a) = fixture();
        b.unreliable_intervals.push(Interval {
            start_frame: 96000,
            end_frame: 192000,
            reason: "weak pulse".into(),
        });
        let result = analyze_segments(&b, &f, &a).unwrap();
        assert!(result
            .tiles
            .iter()
            .all(|t| t.end_frame <= 96000 || t.start_frame >= 192000));
        b.meter_segments.push(Meter {
            start_frame: 0,
            end_frame: a.frames,
            pulses_per_bar: 3,
            pulse_unit: "beat".into(),
        });
        assert!(analyze_segments(&b, &f, &a).unwrap().tiles.is_empty());
    }
}
