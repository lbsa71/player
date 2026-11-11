//! Source-time pacing between joins. Phrase evidence governs early admission;
//! development length is a preference, independent of tactic safety admission.
use super::*;
use serde::Serialize;

const UNKNOWN_SOLO_SECONDS: u64 = 45;
const PREFERRED_PHRASES: usize = 4;

#[derive(Clone, Debug)]
struct PhraseSpan {
    start: u64,
    end: u64,
    anchor: u64,
}

#[derive(Clone, Debug)]
pub struct TrackPacing {
    sample_rate: u32,
    phrases: Vec<PhraseSpan>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PacingDecision {
    pub method: &'static str,
    pub previous_join_end_frame: u64,
    pub next_join_start_frame: u64,
    pub solo_seconds: f64,
    pub complete_phrases: Option<usize>,
    pub complete_bars: Option<usize>,
    pub penalty_quality_seconds: f64,
    pub evidence: String,
}

impl TrackPacing {
    /// Unknown phrase evidence retains the conservative time-based admission.
    pub fn unknown(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            phrases: vec![],
        }
    }

    pub fn from_features(beats: &Beats, features: &[FeatureFrame]) -> Self {
        let positions = phrase::infer_phrase_positions(beats, features);
        let points: Vec<_> = positions.iter().collect();
        let mut phrases = vec![];
        for run in points.windows(5) {
            if run[0].1.boundary
                && run[4].1.boundary
                && run.iter().enumerate().all(|(i, (_, p))| {
                    p.anchor_frame == run[0].1.anchor_frame && p.bar_in_phrase == i % 4
                })
            {
                phrases.push(PhraseSpan {
                    start: *run[0].0,
                    end: *run[4].0,
                    anchor: run[0].1.anchor_frame,
                });
            }
        }
        Self {
            sample_rate: beats.source_sample_rate_hz,
            phrases,
        }
    }

    /// An optimistic lower bound. The actual exit still needs assess(): a
    /// timestamp after this bound is not automatically a musical endpoint.
    pub fn earliest_exit(&self, entry_end: u64) -> u64 {
        if entry_end == 0 {
            return 0;
        }
        let conservative =
            entry_end.saturating_add(UNKNOWN_SOLO_SECONDS * u64::from(self.sample_rate));
        self.phrases
            .iter()
            .find(|p| p.start.saturating_add(1) >= entry_end)
            .map_or(conservative, |p| p.end.min(conservative))
    }

    fn completed_at(&self, entry_end: u64, exit: u64) -> Option<usize> {
        let end = self
            .phrases
            .iter()
            .position(|p| p.end.abs_diff(exit) <= 1)?;
        let mut next = self.phrases[end].end;
        let anchor = self.phrases[end].anchor;
        let mut count = 0;
        for p in self.phrases[..=end].iter().rev() {
            if p.end != next || p.anchor != anchor || p.start.saturating_add(1) < entry_end {
                break;
            }
            count += 1;
            next = p.start;
        }
        (count > 0).then_some(count)
    }

    pub fn assess(&self, entry_end: u64, exit: u64) -> Option<PacingDecision> {
        if exit < entry_end {
            return None;
        }
        if entry_end == 0 {
            return Some(PacingDecision {
                method: "phrase-pacing/1",
                previous_join_end_frame: 0,
                next_join_start_frame: exit,
                solo_seconds: 0.,
                complete_phrases: None,
                complete_bars: None,
                penalty_quality_seconds: 0.,
                evidence: "Opening track; bookend selection supplies the entry.".into(),
            });
        }
        let seconds = (exit - entry_end) as f64 / f64::from(self.sample_rate);
        let phrases = self.completed_at(entry_end, exit);
        if phrases.is_none() && seconds < UNKNOWN_SOLO_SECONDS as f64 {
            return None;
        }
        let penalty = phrases.map_or(0., |n| PREFERRED_PHRASES.saturating_sub(n) as f64 * 0.75);
        Some(PacingDecision { method:"phrase-pacing/1",previous_join_end_frame:entry_end,
            next_join_start_frame:exit,solo_seconds:seconds,complete_phrases:phrases,
            complete_bars:phrases.map(|n|n*4),penalty_quality_seconds:penalty,
            evidence:phrases.map_or_else(|| "No complete inferred phrase chain at this exit; conservative 45-second runway retained. Tactic vocal/build/overlap checks remain mandatory.".into(),
                |n|format!("{n} complete measured four-bar phrases after the previous join, ending at a supported phrase boundary; four phrases preferred, shorter stays incur a bounded pacing penalty. Tactic vocal/build/overlap checks remain mandatory.")) })
    }
}

pub fn read_track_pacing(
    store: &Store,
    track_id: &str,
    cancel: &Cancellation,
) -> Result<TrackPacing> {
    let track = load_analyzed(store, store.track(track_id)?)?;
    let harmony: Harmony = store.read_ref(&track.analysis.artifacts["harmony"])?;
    let mut frames = Vec::new();
    for reference in &harmony.feature_chunks {
        cancel.check()?;
        let chunk: FeatureChunk = store.read_ref(reference)?;
        frames.extend(chunk.frames);
    }
    frames.sort_unstable_by_key(|f| f.source_frame);
    Ok(TrackPacing::from_features(&track.beats, &frames))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn grid(seconds_per_phrase: u64) -> TrackPacing {
        TrackPacing {
            sample_rate: 100,
            phrases: (0..12)
                .map(|i| PhraseSpan {
                    start: 100 + i * seconds_per_phrase * 100,
                    end: 100 + (i + 1) * seconds_per_phrase * 100,
                    anchor: 100,
                })
                .collect(),
        }
    }
    #[test]
    fn complete_phrases_not_elapsed_seconds_admit_short_stays() {
        let fast = grid(8);
        let decision = fast.assess(100, 2500).unwrap();
        assert_eq!(decision.complete_bars, Some(12));
        assert_eq!(decision.solo_seconds, 24.);
        assert!(decision.penalty_quality_seconds > 0.);
        assert!(
            fast.assess(100, 2400).is_none(),
            "mid-phrase exit must not become legal merely by shortening the runway"
        );
        assert_eq!(grid(16).assess(100, 4900).unwrap().complete_bars, Some(12));
        assert!(fast.assess(900, 100).is_none(), "joins cannot overlap");
    }
    #[test]
    fn partial_phrase_and_uncertain_gaps_do_not_count_as_complete() {
        let mut p = grid(8);
        assert!(p.assess(200, 900).is_none());
        assert_eq!(p.assess(200, 1700).unwrap().complete_phrases, Some(1));
        p.phrases.remove(1);
        assert_eq!(p.assess(100, 2500).unwrap().complete_phrases, Some(1));
        let unknown = TrackPacing::unknown(100);
        assert!(unknown.assess(100, 2500).is_none());
        assert!(unknown.assess(100, 4600).is_some());
    }
    #[test]
    fn earlier_arrivals_dominate_later_arrivals_for_reachability() {
        let p = grid(8);
        for exit in (100..10000).step_by(100) {
            for later in (100..exit).step_by(100) {
                if p.assess(later, exit).is_some() {
                    assert!(p.assess(100, exit).is_some());
                }
            }
        }
    }
}
