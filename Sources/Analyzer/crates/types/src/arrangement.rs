//! Explicit repeats on the rendered base timeline. Source maps and measured
//! transitions retain their original coordinates; this is a separate clock.
use super::*;

pub const ARRANGEMENT_EXTENSION: &str = "set_arrangement";
pub const ARRANGEMENT_RENDERER: &str = "signalsmith-set-replay/1";
pub const MAX_REPLAY_FRACTION: f64 = 0.15;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PassageReplay {
    pub placement: usize,
    pub capture_start: u64,
    pub capture_end: u64,
    /// Insert before this base-timeline frame, then resume that exact frame.
    pub insert_before: u64,
    pub repetitions: u32,
    pub bars: u32,
    pub declick_frames: u64,
    pub evidence: Value,
}
impl PassageReplay {
    pub fn added_frames(&self) -> u64 {
        (self.capture_end - self.capture_start) * u64::from(self.repetitions)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetArrangement {
    pub version: u32,
    pub base_output_frames: u64,
    pub replays: Vec<PassageReplay>,
}
impl SetArrangement {
    /// The original frame is heard after any insertion at that boundary.
    pub fn output_at(&self, base: u64) -> u64 {
        base + self
            .replays
            .iter()
            .filter(|r| r.insert_before <= base)
            .map(PassageReplay::added_frames)
            .sum::<u64>()
    }
    pub fn added_frames(&self) -> u64 {
        self.replays.iter().map(PassageReplay::added_frames).sum()
    }
    pub fn validate_for(&self, plan: &MixPlan) -> Result<()> {
        require(
            self.version == 1
                && !self.replays.is_empty()
                && self.base_output_frames > 0
                && self.base_output_frames <= MAX_FRAME,
            "SET_REPLAY",
            "invalid arrangement version or base duration",
        )?;
        let rate = u64::from(plan.output.sample_rate_hz);
        let mut previous = 0;
        let mut owners = BTreeSet::new();
        let mut added = 0u64;
        let tactics = read_tactics(plan)?;
        for r in &self.replays {
            let p = plan
                .placements
                .get(r.placement)
                .ok_or_else(|| Error::new("SET_REPLAY", "unknown replay placement"))?;
            let begin = if r.placement == 0 {
                0
            } else {
                plan.transitions[r.placement - 1].end_output_frame
            };
            let end = plan
                .transitions
                .get(r.placement)
                .map_or(self.base_output_frames, |t| t.start_output_frame);
            require(
                owners.insert(r.placement)
                    && r.capture_start >= begin.saturating_add(rate)
                    && r.capture_start < r.capture_end
                    && r.capture_end <= r.insert_before
                    && r.insert_before.saturating_add(rate) < end
                    && r.insert_before > previous
                    && (1..=3).contains(&r.repetitions)
                    && matches!(r.bars, 4 | 8 | 16)
                    && r.capture_end - r.capture_start <= 60 * rate
                    && r.declick_frames > 0
                    && r.declick_frames <= rate / 100
                    && 2 * r.declick_frames < r.capture_end - r.capture_start,
                "SET_REPLAY",
                "replay must be a bounded whole phrase inside one solo passage",
            )?;
            // Replaying a ramp or a filtered introduction changes the splice.
            // Require one steady source clock and constant audible automation.
            let slopes = p.time_map.slopes();
            let active_slopes: Vec<_> = p
                .time_map
                .anchors
                .windows(2)
                .enumerate()
                .filter(|(_, w)| {
                    w[0].output_frame < r.insert_before + rate
                        && w[1].output_frame > r.capture_start - rate
                })
                .flat_map(|(i, _)| [slopes[i], slopes[i + 1]])
                .collect();
            require(
                active_slopes
                    .iter()
                    .all(|s| (s - active_slopes[0]).abs() < 1e-6),
                "SET_REPLAY",
                "replay crosses a tempo ramp",
            )?;
            require(
                !tactics.iter().any(|t| {
                    let end = t
                        .effects
                        .iter()
                        .map(|e| e.end_frame)
                        .max()
                        .unwrap_or(t.end_frame)
                        .max(t.end_frame);
                    t.start_frame < r.insert_before.saturating_add(rate)
                        && end > r.capture_start.saturating_sub(rate)
                }),
                "SET_REPLAY",
                "replay crosses a transition or effect tail",
            )?;
            let points: Vec<_> = p
                .automation
                .windows(2)
                .filter(|w| {
                    w[0].output_frame < r.insert_before + rate
                        && w[1].output_frame > r.capture_start - rate
                })
                .flat_map(|w| w.iter())
                .collect();
            require(
                !points.is_empty()
                    && points.iter().all(|a| {
                        !a.mute
                            && a.gain_linear > 0.
                            && a.gain_linear == points[0].gain_linear
                            && a.bands_linear == points[0].bands_linear
                    }),
                "SET_REPLAY",
                "replay crosses changing automation",
            )?;
            added = added
                .checked_add(r.added_frames())
                .ok_or_else(|| Error::new("SET_REPLAY", "replay duration overflow"))?;
            previous = r.insert_before;
        }
        require(
            added as f64 <= self.base_output_frames as f64 * MAX_REPLAY_FRACTION
                && self.base_output_frames.checked_add(added) == Some(plan.total_output_frames)
                && plan.renderer_version == ARRANGEMENT_RENDERER,
            "SET_REPLAY",
            "replay duration, repetition budget or renderer mismatch",
        )
    }
}

pub fn read_arrangement(plan: &MixPlan) -> Result<Option<SetArrangement>> {
    plan.header
        .extensions
        .get(ARRANGEMENT_EXTENSION)
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(Into::into)
}
