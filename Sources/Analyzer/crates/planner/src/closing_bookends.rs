//! Selection and enumeration of musical openings and endings.
use super::*;

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
    pub(super) attack: u64,
    pub(super) accent: Option<u64>,
    pub(super) lead: u64,
}
impl ChainOpening {
    pub fn before(&self, frame: u64) -> u64 {
        self.accent
            .filter(|accent| *accent <= frame)
            .map_or(self.attack, |accent| accent.saturating_sub(self.lead))
            .min(frame)
    }

    /// Existing opening first, followed by the measured attack and all clear
    /// accented starts before the first pair's preserved source context.
    pub fn candidates_before(&self, frame: u64, accents: &[u64]) -> Vec<u64> {
        let preferred = self.before(frame);
        let mut choices = vec![preferred];
        let mut seen = BTreeSet::from([preferred]);
        choices.extend(
            std::iter::once(self.attack)
                .filter(|&attack| attack <= frame)
                .chain(
                    accents
                        .iter()
                        .take_while(|&&at| at <= frame)
                        .map(|&at| at.saturating_sub(self.lead)),
                )
                .filter(|&start| seen.insert(start)),
        );
        choices
    }

    pub fn read_with_candidates(
        store: &Store,
        track_id: &str,
        cancel: &Cancellation,
    ) -> Result<(Self, Vec<u64>)> {
        analyze_opening(store, track_id, true, cancel)
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
    Ok(analyze_opening(store, track_id, false, cancel)?.0)
}

fn analyze_opening(
    store: &Store,
    track_id: &str,
    all_accents: bool,
    cancel: &Cancellation,
) -> Result<(ChainOpening, Vec<u64>)> {
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
    let mut entries = Vec::new();
    for beat in &first_track.beats.events {
        if !all_accents && beat.source_frame > first_audible + 2 * opening_rate {
            break;
        }
        if accents.contains(&beat.event_id)
            && beat.score >= 0.5
            && beat.source_frame >= first_audible
        {
            cancel.check()?;
            let at = beat.source_frame;
            let before = opening.rms(at.saturating_sub(opening_rate / 3), at);
            let after = opening.rms(at, at + opening_rate / 2);
            if after >= threshold && before <= after * 0.30 {
                entries.push(at);
                if !all_accents {
                    break;
                }
            }
        }
    }
    let entry = entries
        .first()
        .copied()
        .filter(|&at| at <= first_audible + 2 * opening_rate);
    Ok((
        ChainOpening {
            attack,
            accent: entry,
            lead: opening_rate / 20,
        },
        entries,
    ))
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
    chain_ending_candidates(store, start, last, window, cancel)?
        .into_iter()
        .next()
        .ok_or_else(|| {
            Error::new(
                "CHAIN_BOOKEND",
                "no clear musical ending within the set duration window",
            )
        })
}

/// Enumerate the existing natural-tail and clear phrase-fade choices for a
/// fixed opening. The preferred legacy choice comes first.
pub fn chain_ending_candidates(
    store: &Store,
    start: u64,
    last: &Placement,
    window: Option<ChainClosingWindow>,
    cancel: &Cancellation,
) -> Result<Vec<ChainBookends>> {
    cancel.check()?;
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
    let mut endings = Vec::new();
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
            endings.push(ChainBookends {
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
    let mut ranked = Vec::new();
    for bar in bars.windows(2) {
        cancel.check()?;
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
        if score.is_finite() {
            ranked.push((
                score,
                ChainBookends {
                    start,
                    end,
                    outro_fade_frames: fade,
                    ending: "phrase_fade",
                },
            ));
        }
    }
    ranked.sort_by(|a, b| a.0.total_cmp(&b.0));
    endings.extend(ranked.into_iter().map(|(_, ending)| ending));
    if window.is_none() && endings.is_empty() {
        endings.push(ChainBookends {
            start,
            end: source_end,
            outro_fade_frames: 2 * rate,
            ending: "source_tail_fade",
        });
    }
    Ok(endings)
}

pub(super) fn closing_is_clear(record: &TacticTrackAnalysis, start: u64, end: u64) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_domain_keeps_the_preferred_attack_and_later_accents_once() {
        let cue = ChainOpening {
            attack: 75,
            accent: Some(150),
            lead: 5,
        };
        let accents = [150, 200, 200, 400];
        assert_eq!(cue.candidates_before(50, &accents), [50]);
        assert_eq!(cue.candidates_before(149, &accents), [75]);
        assert_eq!(cue.candidates_before(200, &accents), [145, 75, 195]);
        assert_eq!(cue.candidates_before(500, &accents), [145, 75, 195, 395]);
    }
}
