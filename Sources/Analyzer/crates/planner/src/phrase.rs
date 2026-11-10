//! Four-bar phrase phase inferred from repeated changes in reliable bars.
use mixlib_types::{Beats, FeatureFrame};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub(super) struct PhrasePosition {
    pub bar_in_phrase: usize,
    // Separation between the best phase and its runner-up, not a probability.
    pub confidence: f64,
    pub boundary: bool,
    pub anchor_frame: u64,
    pub supporting_changes: usize,
}

struct Bar {
    start: u64,
    end: u64,
    spectrum: [f64; 4],
    chroma: [f64; 12],
}

fn describe_bar(features: &[FeatureFrame], start: u64, end: u64) -> Option<Bar> {
    let lo = features.partition_point(|f| f.source_frame < start);
    let hi = features.partition_point(|f| f.source_frame < end);
    let frames = &features[lo..hi];
    if frames.is_empty() {
        return None;
    }
    let mut spectrum = [0.; 4];
    let mut chroma = [0.; 12];
    for frame in frames {
        for (sum, energy) in spectrum[..3].iter_mut().zip(frame.band_energy) {
            *sum += energy;
        }
        spectrum[3] += frame.onset;
        for (sum, value) in chroma.iter_mut().zip(frame.chroma) {
            *sum += value * frame.rms;
        }
    }
    for value in &mut spectrum[..3] {
        *value = (*value / frames.len() as f64).sqrt();
    }
    spectrum[3] /= frames.len() as f64;
    let total: f64 = chroma.iter().sum();
    if total > 0. {
        for value in &mut chroma {
            *value /= total;
        }
    }
    Some(Bar {
        start,
        end,
        spectrum,
        chroma,
    })
}

fn novelty(previous: &Bar, current: &Bar) -> f64 {
    let spectral = previous
        .spectrum
        .iter()
        .zip(current.spectrum)
        .map(|(a, b)| ((b + 1e-6) / (a + 1e-6)).ln().abs())
        .sum::<f64>()
        / 4.;
    let harmonic = previous
        .chroma
        .iter()
        .zip(current.chroma)
        .map(|(a, b)| (a - b).abs())
        .sum::<f64>()
        * 0.125;
    (spectral + harmonic).min(1.5)
}

fn infer_run(bars: &[Bar], positions: &mut BTreeMap<u64, PhrasePosition>) {
    if bars.len() < 9 {
        return;
    }
    let changes: Vec<_> = bars
        .windows(2)
        .map(|pair| novelty(&pair[0], &pair[1]))
        .collect();
    let mut scores = [0.; 4];
    let mut support = [0; 4];
    for phase in 0..4 {
        let mut evidence: Vec<_> = changes
            .iter()
            .enumerate()
            .filter_map(|(i, &change)| ((i + 1) % 4 == phase).then_some(change))
            .collect();
        evidence.sort_by(|a, b| b.total_cmp(a));
        // A few repeated section changes carry phase through a steady passage.
        // Capping and requiring multiple changes prevent one transient winning.
        scores[phase] = evidence.iter().take(4).sum();
        support[phase] = evidence.iter().filter(|&&value| value >= 0.12).count();
    }
    let phase = (0..4)
        .max_by(|&a, &b| scores[a].total_cmp(&scores[b]))
        .unwrap();
    let best = scores[phase];
    let runner_up = (0..4)
        .filter(|&p| p != phase)
        .map(|p| scores[p])
        .fold(0., f64::max);
    let confidence = (best - runner_up) / best.max(1e-6);
    if support[phase] < 2 || confidence < 0.20 {
        return;
    }
    let anchor = changes
        .iter()
        .enumerate()
        .find(|(i, change)| (i + 1) % 4 == phase && **change >= 0.12)
        .map(|(i, _)| bars[i + 1].start)
        .unwrap();
    for (index, frame) in bars
        .iter()
        .map(|bar| bar.start)
        .chain(std::iter::once(bars.last().unwrap().end))
        .enumerate()
    {
        let bar_in_phrase = (index + 4 - phase) % 4;
        positions.insert(
            frame,
            PhrasePosition {
                bar_in_phrase,
                confidence,
                boundary: bar_in_phrase == 0,
                anchor_frame: anchor,
                supporting_changes: support[phase],
            },
        );
    }
}

/// Missing positions are ambiguous or lie outside a reliable four-beat run.
/// Derive this from existing features so older analyses need no migration.
pub(super) fn infer_phrase_positions(
    beats: &Beats,
    features: &[FeatureFrame],
) -> BTreeMap<u64, PhrasePosition> {
    let accents: BTreeSet<_> = beats.downbeat_ids.iter().collect();
    let downbeats: Vec<_> = beats
        .events
        .iter()
        .enumerate()
        .filter_map(|(i, beat)| accents.contains(&beat.event_id).then_some(i))
        .collect();
    let mut positions = BTreeMap::new();
    let mut run = vec![];
    for pair in downbeats.windows(2) {
        let (first, last) = (pair[0], pair[1]);
        let (start, end) = (
            beats.events[first].source_frame,
            beats.events[last].source_frame,
        );
        let pulse = (end - start) as f64 / 4.;
        let reliable = last - first == 4
            && beats.events[first..=last]
                .iter()
                .all(|beat| beat.score >= 0.5)
            && beats.events[first..=last].windows(2).all(|pair| {
                let duration = (pair[1].source_frame - pair[0].source_frame) as f64;
                (0.8 * pulse..=1.2 * pulse).contains(&duration)
            })
            && !beats
                .unreliable_intervals
                .iter()
                .any(|interval| interval.start_frame < end && interval.end_frame > start)
            && !beats.meter_segments.iter().any(|meter| {
                meter.start_frame < end && meter.end_frame > start && meter.pulses_per_bar != 4
            });
        let bar = reliable
            .then(|| describe_bar(features, start, end))
            .flatten();
        if let Some(bar) = bar {
            run.push(bar);
        } else {
            infer_run(&run, &mut positions);
            run.clear();
        }
    }
    infer_run(&run, &mut positions);
    positions
}

#[cfg(test)]
mod tests {
    use super::*;
    use mixlib_types::{Beat, Header, Interval};

    fn fixture(levels: &[f64]) -> (Beats, Vec<FeatureFrame>) {
        let events: Vec<_> = (0..=levels.len() * 4)
            .map(|i| Beat {
                event_id: format!("beat-{i}"),
                source_frame: i as u64 * 100,
                score: 1.,
                local_bpm: Some(120.),
            })
            .collect();
        let downbeat_ids = events
            .iter()
            .step_by(4)
            .map(|beat| beat.event_id.clone())
            .collect();
        let features = levels
            .iter()
            .enumerate()
            .map(|(i, &level)| FeatureFrame {
                source_frame: i as u64 * 400,
                chroma: [1.; 12],
                band_energy: [level * level; 3],
                rms: level,
                peak: level,
                onset: level,
            })
            .collect();
        (
            Beats {
                header: Header::default(),
                beats_id: "test".into(),
                source_sample_rate_hz: 200,
                source_frames: levels.len() as u64 * 400 + 1,
                events,
                downbeat_ids,
                meter_segments: vec![],
                unreliable_intervals: vec![],
                score_semantics: "fixture".into(),
            },
            features,
        )
    }

    #[test]
    fn structural_changes_define_phase_after_a_partial_intro() {
        let levels: Vec<_> = (0..24)
            .map(|i| if (i + 1) / 4 % 2 == 0 { 0.2 } else { 0.8 })
            .collect();
        let (beats, features) = fixture(&levels);
        let positions = infer_phrase_positions(&beats, &features);
        assert!(!positions[&0].boundary);
        assert!(positions[&1200].boundary);
        assert_eq!(positions[&800].bar_in_phrase, 3);
        assert_eq!(positions[&8800].bar_in_phrase, 3);
        assert!(positions[&9200].boundary);
        assert_eq!(positions[&9200].anchor_frame, 1200);
    }

    #[test]
    fn steady_or_two_bar_material_is_ambiguous() {
        for levels in [
            vec![0.2; 24],
            (0..24).map(|i| if i % 4 < 2 { 0.2 } else { 0.8 }).collect(),
        ] {
            let (beats, features) = fixture(&levels);
            assert!(infer_phrase_positions(&beats, &features).is_empty());
        }
    }

    #[test]
    fn unreliable_gap_does_not_carry_phase_into_unknown_material() {
        let mut levels: Vec<_> = (0..16)
            .map(|i| if (i + 1) / 4 % 2 == 0 { 0.2 } else { 0.8 })
            .collect();
        levels.extend([0.2; 20]);
        let (mut beats, features) = fixture(&levels);
        beats.unreliable_intervals.push(Interval {
            start_frame: 6400,
            end_frame: 6800,
            reason: "missing beat".into(),
        });
        let positions = infer_phrase_positions(&beats, &features);
        assert!(positions[&6000].boundary);
        assert!(!positions.contains_key(&6800));
        assert!(!positions.contains_key(&10000));
    }
}
