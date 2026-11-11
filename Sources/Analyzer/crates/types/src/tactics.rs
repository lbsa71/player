//! Executable transition contracts. All event times are absolute output frames unless named source.
use super::*;

pub const TACTICS_EXTENSION: &str = "mixing_tactics";
pub const TACTICS_POLICY_VERSION: &str = "mixing_tactics/1";

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
pub enum TacticId {
    M01,
    M02,
    M03,
    M04,
    M05,
    M06,
    S01,
    S02,
    S03,
    S04,
    S05,
    S06,
    S07,
    S08,
    S09,
    S10,
    S11,
    S12,
    D01,
    D02,
    C01,
    C02,
    C03,
    C04,
    C05,
    C06,
    C07,
    C08,
    C09,
    C10,
    C11,
    C12,
    C13,
    C14,
    E01,
    E02,
    E03,
    E04,
}
impl TacticId {
    pub const ALL: [Self; 38] = [
        Self::M01,
        Self::M02,
        Self::M03,
        Self::M04,
        Self::M05,
        Self::M06,
        Self::S01,
        Self::S02,
        Self::S03,
        Self::S04,
        Self::S05,
        Self::S06,
        Self::S07,
        Self::S08,
        Self::S09,
        Self::S10,
        Self::S11,
        Self::S12,
        Self::D01,
        Self::D02,
        Self::C01,
        Self::C02,
        Self::C03,
        Self::C04,
        Self::C05,
        Self::C06,
        Self::C07,
        Self::C08,
        Self::C09,
        Self::C10,
        Self::C11,
        Self::C12,
        Self::C13,
        Self::C14,
        Self::E01,
        Self::E02,
        Self::E03,
        Self::E04,
    ];
    pub fn permission(self) -> TacticPermission {
        match self {
            Self::C05
            | Self::C06
            | Self::C08
            | Self::C09
            | Self::C10
            | Self::C11
            | Self::C12
            | Self::C13
            | Self::C14 => TacticPermission::ExpressiveOptIn,
            Self::E01 | Self::E02 | Self::E03 | Self::E04 => TacticPermission::EmergencyOnly,
            _ => TacticPermission::Normal,
        }
    }
    pub fn is_modifier(self) -> bool {
        matches!(self, Self::D01 | Self::D02)
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TacticPermission {
    Normal,
    ExpressiveOptIn,
    EmergencyOnly,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TacticArrangement {
    Mashup,
    Blend,
    Handoff,
    Bridge,
    Replacement,
    Reset,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TacticCharacter {
    Subtle,
    Sudden,
    Abrupt,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TacticTimingBasis {
    #[default]
    Bars,
    Structural,
    IndependentPulses,
}
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum StemLane {
    Stereo,
    Drums,
    Bass,
    Other,
    Vocals,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ScalarPoint {
    pub output_frame: u64,
    pub value: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LaneSchedule {
    pub placement: usize,
    pub lane: StemLane,
    pub gain: Vec<ScalarPoint>,
    /// Empty means unity. Ordered low/mid/high band gains use the plan crossovers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub eq: Vec<BandGainPoint>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BandGainPoint {
    pub output_frame: u64,
    pub gains: [f64; 3],
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StemAsset {
    pub placement: usize,
    pub lane: StemLane,
    pub audio: FileRef,
    pub source_offset_frame: u64,
    pub source_end_frame: u64,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FilterMode {
    HighPass,
    LowPass,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EffectKind {
    Filter {
        mode: FilterMode,
        cutoff_hz: Vec<ScalarPoint>,
        resonance: f64,
    },
    Delay {
        delay_frames: Vec<ScalarPoint>,
        feedback: f64,
        freeze_at: Option<u64>,
        high_pass_hz: f64,
        low_pass_hz: f64,
    },
    Reverb {
        decay_seconds: f64,
        freeze_at: Option<u64>,
        high_pass_hz: f64,
        low_pass_hz: f64,
    },
    Gate {
        period_frames: u64,
        phase_frames: u64,
        duty_cycle: f64,
        smoothing_frames: u64,
    },
    Noise {
        seed: u64,
        gain: f64,
    },
    Spiral {
        delay_frames: f64,
        feedback: f64,
        pitch_semitones: Vec<ScalarPoint>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EffectSpec {
    pub placement: usize,
    pub lane: StemLane,
    pub start_frame: u64,
    /// Input and dry audio stop here; stored wet audio can continue to end_frame.
    pub input_end_frame: u64,
    pub end_frame: u64,
    pub wet: Vec<ScalarPoint>,
    pub kind: EffectKind,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourcePositionPoint {
    pub output_frame: u64,
    pub source_frame: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PitchNote {
    pub start_frame: u64,
    pub end_frame: u64,
    pub semitones: f64,
    pub gain: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceEditKind {
    /// One captured source cycle stretched to period_frames output frames and repeated.
    Loop {
        period_frames: u64,
    },
    /// Target output period evaluated at each repetition boundary.
    LoopSequence {
        periods: Vec<ScalarPoint>,
    },
    Reverse,
    SlipReverse,
    Brake {
        initial_speed: f64,
    },
    Backspin {
        initial_speed: f64,
    },
    Scratch {
        positions: Vec<SourcePositionPoint>,
    },
    /// Sample retriggers; resampling changes pitch and sample duration intentionally.
    PitchSequence {
        notes: Vec<PitchNote>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceTransport {
    ContinueUnderlying,
    ResumeAt { source_frame: u64 },
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceEditSpec {
    pub placement: usize,
    pub lane: StemLane,
    pub source_sample_rate_hz: u32,
    pub source_start_frame: u64,
    pub source_end_frame: u64,
    pub output_start_frame: u64,
    pub output_end_frame: u64,
    pub declick_frames: u64,
    pub transport: SourceTransport,
    pub kind: SourceEditKind,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VocalOccupancy {
    pub placement: usize,
    pub start_frame: u64,
    pub end_frame: u64,
    /// Includes intelligible leakage and every effect tail.
    pub tail_end_frame: u64,
    pub supported_entry: bool,
    pub supported_exit: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EmergencyActivation {
    pub reason: EmergencyReason,
    pub activated_output_frame: u64,
    pub failed_condition: String,
    pub rejected_safe_recoveries: Vec<String>,
    pub next_source_verified: bool,
    pub healthy_buffer_frames: u64,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EmergencyReason {
    UnexpectedSourceLoss,
    UnexpectedDecodeFailure,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TacticPlan {
    pub policy_version: String,
    pub tactic_id: TacticId,
    pub modifiers: Vec<TacticId>,
    pub arrangement: TacticArrangement,
    pub character: TacticCharacter,
    pub permission: TacticPermission,
    pub outgoing_placement: usize,
    pub incoming_placement: usize,
    pub start_frame: u64,
    pub hold_end_frame: u64,
    pub exit_start_frame: u64,
    pub end_frame: u64,
    pub bar_frames: Vec<u64>,
    #[serde(default)]
    pub timing_basis: TacticTimingBasis,
    pub bar_bpm: Option<Vec<f64>>,
    pub pulse_ratio: Option<f64>,
    pub lanes: Vec<LaneSchedule>,
    pub stem_assets: Vec<StemAsset>,
    pub effects: Vec<EffectSpec>,
    pub source_edits: Vec<SourceEditSpec>,
    /// Absolute playback-speed magnitudes for pitch-preserving repeat/reverse edits.
    pub edit_speed_ratio_limits: [f64; 2],
    pub vocal_occupancy: Vec<VocalOccupancy>,
    pub vocal_guard_frames: u64,
    pub evidence: Vec<String>,
    pub selection_reason: String,
    pub emergency: Option<EmergencyActivation>,
}

pub fn validate_curve(
    points: &[ScalarPoint],
    start: u64,
    end: u64,
    lo: f64,
    hi: f64,
) -> Result<()> {
    require(
        points.len() >= 2
            && points[0].output_frame == start
            && points.last().unwrap().output_frame == end,
        "TACTIC_CURVE",
        "automation must cover its declared interval",
    )?;
    require(
        points
            .windows(2)
            .all(|w| w[0].output_frame < w[1].output_frame)
            && points
                .iter()
                .all(|p| p.value.is_finite() && (lo..=hi).contains(&p.value)),
        "TACTIC_CURVE",
        "automation must be ordered, finite and bounded",
    )
}
impl EffectSpec {
    pub fn validate(&self, rate: u32) -> Result<()> {
        require(
            rate > 0
                && self.start_frame < self.input_end_frame
                && self.input_end_frame <= self.end_frame
                && self.end_frame <= MAX_FRAME
                && self.end_frame - self.start_frame <= rate as u64 * 300,
            "TACTIC_EFFECT",
            "invalid effect lifetime",
        )?;
        validate_curve(&self.wet, self.start_frame, self.end_frame, 0., 1.)?;
        let filter = |hp: f64, lp: f64| {
            require(
                hp.is_finite() && lp.is_finite() && hp >= 0. && hp < lp && lp < (rate as f64 / 2.),
                "TACTIC_EFFECT",
                "invalid tail filter bounds",
            )
        };
        let freeze = |f: Option<u64>| {
            require(
                f.is_none_or(|f| f > self.start_frame && f <= self.input_end_frame),
                "TACTIC_EFFECT",
                "freeze needs captured audio before input close",
            )
        };
        match &self.kind {
            EffectKind::Filter {
                cutoff_hz,
                resonance,
                ..
            } => {
                validate_curve(
                    cutoff_hz,
                    self.start_frame,
                    self.end_frame,
                    10.,
                    rate as f64 * 0.45,
                )?;
                require(
                    resonance.is_finite() && (0.5..=2.).contains(resonance),
                    "TACTIC_EFFECT",
                    "filter Q outside supported bounds",
                )?;
            }
            EffectKind::Delay {
                delay_frames,
                feedback,
                freeze_at,
                high_pass_hz,
                low_pass_hz,
            } => {
                validate_curve(
                    delay_frames,
                    self.start_frame,
                    self.end_frame,
                    1.,
                    rate as f64 * 8.,
                )?;
                require(
                    feedback.is_finite() && (0.0..=0.95).contains(feedback),
                    "TACTIC_EFFECT",
                    "unbounded delay feedback",
                )?;
                filter(*high_pass_hz, *low_pass_hz)?;
                freeze(*freeze_at)?;
            }
            EffectKind::Reverb {
                decay_seconds,
                freeze_at,
                high_pass_hz,
                low_pass_hz,
            } => {
                require(
                    decay_seconds.is_finite() && (0.05..=30.).contains(decay_seconds),
                    "TACTIC_EFFECT",
                    "unbounded reverb decay",
                )?;
                filter(*high_pass_hz, *low_pass_hz)?;
                freeze(*freeze_at)?;
            }
            EffectKind::Gate {
                period_frames,
                phase_frames,
                duty_cycle,
                smoothing_frames,
            } => require(
                *period_frames >= 2
                    && *phase_frames < *period_frames
                    && duty_cycle.is_finite()
                    && (0.05..=0.95).contains(duty_cycle)
                    && *smoothing_frames > 0
                    && *smoothing_frames < *period_frames / 2,
                "TACTIC_EFFECT",
                "invalid gate pattern",
            )?,
            EffectKind::Noise { gain, .. } => require(
                gain.is_finite() && (0.0..=0.25).contains(gain),
                "TACTIC_EFFECT",
                "noise gain outside safe range",
            )?,
            EffectKind::Spiral {
                delay_frames,
                feedback,
                pitch_semitones,
            } => {
                require(
                    delay_frames.is_finite()
                        && (1.0..=rate as f64 * 8.).contains(delay_frames)
                        && feedback.is_finite()
                        && (0.0..=0.95).contains(feedback),
                    "TACTIC_EFFECT",
                    "invalid spiral delay",
                )?;
                validate_curve(pitch_semitones, self.start_frame, self.end_frame, -24., 24.)?;
            }
        }
        Ok(())
    }
}
impl SourceEditSpec {
    pub fn validate(&self) -> Result<()> {
        require(
            self.source_sample_rate_hz > 0
                && self.source_start_frame < self.source_end_frame
                && self.source_end_frame <= MAX_FRAME
                && self.source_end_frame - self.source_start_frame
                    <= self.source_sample_rate_hz as u64 * 60
                && self.output_start_frame < self.output_end_frame
                && self.output_end_frame <= MAX_FRAME
                && self.declick_frames > 0
                && self.declick_frames <= (self.output_end_frame - self.output_start_frame) / 2,
            "TACTIC_EDIT",
            "invalid edit timebase or de-click bounds",
        )?;
        if let SourceTransport::ResumeAt { source_frame } = self.transport {
            require(
                source_frame <= MAX_FRAME,
                "TACTIC_EDIT",
                "invalid resume frame",
            )?;
        }
        match &self.kind {
            SourceEditKind::Loop { period_frames } => require(
                *period_frames >= 2 * self.declick_frames,
                "TACTIC_EDIT",
                "loop period too short",
            )?,
            SourceEditKind::LoopSequence { periods } => validate_curve(
                periods,
                self.output_start_frame,
                self.output_end_frame,
                (2 * self.declick_frames) as f64,
                MAX_FRAME as f64,
            )?,
            SourceEditKind::Brake { initial_speed }
            | SourceEditKind::Backspin { initial_speed } => require(
                initial_speed.is_finite() && (0.1..=8.).contains(initial_speed),
                "TACTIC_EDIT",
                "invalid motion speed",
            )?,
            SourceEditKind::Scratch { positions } => {
                require(
                    positions.len() >= 3
                        && positions[0].output_frame == self.output_start_frame
                        && positions.last().unwrap().output_frame == self.output_end_frame
                        && positions
                            .windows(2)
                            .all(|w| w[0].output_frame < w[1].output_frame)
                        && positions.iter().all(|p| {
                            p.source_frame.is_finite()
                                && p.source_frame >= self.source_start_frame as f64
                                && p.source_frame < self.source_end_frame as f64
                        }),
                    "TACTIC_EDIT",
                    "scratch path outside captured buffer",
                )?;
            }
            SourceEditKind::PitchSequence { notes } => {
                require(
                    !notes.is_empty()
                        && notes.len() <= 256
                        && notes.windows(2).all(|w| w[0].end_frame <= w[1].start_frame)
                        && notes.iter().all(|n| {
                            n.start_frame >= self.output_start_frame
                                && n.start_frame < n.end_frame
                                && n.end_frame <= self.output_end_frame
                                && n.semitones.is_finite()
                                && (-24.0..=24.).contains(&n.semitones)
                                && n.gain.is_finite()
                                && (0.0..=1.).contains(&n.gain)
                        }),
                    "TACTIC_EDIT",
                    "invalid pitch-note sequence",
                )?;
            }
            SourceEditKind::SlipReverse => require(
                matches!(self.transport, SourceTransport::ContinueUnderlying),
                "TACTIC_EDIT",
                "slip must preserve underlying transport",
            )?,
            SourceEditKind::Reverse => {}
        }
        Ok(())
    }
}

impl TacticPlan {
    /// Move a prepared join into a chain without changing any source coordinates,
    /// sample durations, oscillator phases, effect values or transport positions.
    pub fn translated(&self, output_offset: u64, placement_offset: usize) -> Result<Self> {
        fn frame(value: &mut u64, offset: u64) -> Result<()> {
            *value = value
                .checked_add(offset)
                .filter(|v| *v <= MAX_FRAME)
                .ok_or_else(|| {
                    Error::new(
                        "TACTIC_TRANSLATION",
                        "translated frame exceeds safe JSON integer range",
                    )
                })?;
            Ok(())
        }
        fn placement(value: &mut usize, offset: usize) -> Result<()> {
            *value = value.checked_add(offset).ok_or_else(|| {
                Error::new("TACTIC_TRANSLATION", "translated placement index overflow")
            })?;
            Ok(())
        }
        fn curve(points: &mut [ScalarPoint], offset: u64) -> Result<()> {
            for point in points {
                frame(&mut point.output_frame, offset)?;
            }
            Ok(())
        }
        let mut t = self.clone();
        placement(&mut t.outgoing_placement, placement_offset)?;
        placement(&mut t.incoming_placement, placement_offset)?;
        for value in [
            &mut t.start_frame,
            &mut t.hold_end_frame,
            &mut t.exit_start_frame,
            &mut t.end_frame,
        ] {
            frame(value, output_offset)?;
        }
        for value in &mut t.bar_frames {
            frame(value, output_offset)?;
        }
        for lane in &mut t.lanes {
            placement(&mut lane.placement, placement_offset)?;
            curve(&mut lane.gain, output_offset)?;
            for point in &mut lane.eq {
                frame(&mut point.output_frame, output_offset)?;
            }
        }
        for asset in &mut t.stem_assets {
            placement(&mut asset.placement, placement_offset)?;
        }
        for effect in &mut t.effects {
            placement(&mut effect.placement, placement_offset)?;
            for value in [
                &mut effect.start_frame,
                &mut effect.input_end_frame,
                &mut effect.end_frame,
            ] {
                frame(value, output_offset)?;
            }
            curve(&mut effect.wet, output_offset)?;
            match &mut effect.kind {
                EffectKind::Filter { cutoff_hz, .. } => curve(cutoff_hz, output_offset)?,
                EffectKind::Delay {
                    delay_frames,
                    freeze_at,
                    ..
                } => {
                    curve(delay_frames, output_offset)?;
                    if let Some(at) = freeze_at {
                        frame(at, output_offset)?;
                    }
                }
                EffectKind::Reverb { freeze_at, .. } => {
                    if let Some(at) = freeze_at {
                        frame(at, output_offset)?;
                    }
                }
                EffectKind::Spiral {
                    pitch_semitones, ..
                } => curve(pitch_semitones, output_offset)?,
                EffectKind::Gate { .. } | EffectKind::Noise { .. } => {}
            }
        }
        for edit in &mut t.source_edits {
            placement(&mut edit.placement, placement_offset)?;
            frame(&mut edit.output_start_frame, output_offset)?;
            frame(&mut edit.output_end_frame, output_offset)?;
            match &mut edit.kind {
                SourceEditKind::LoopSequence { periods } => curve(periods, output_offset)?,
                SourceEditKind::Scratch { positions } => {
                    for point in positions {
                        frame(&mut point.output_frame, output_offset)?;
                    }
                }
                SourceEditKind::PitchSequence { notes } => {
                    for note in notes {
                        frame(&mut note.start_frame, output_offset)?;
                        frame(&mut note.end_frame, output_offset)?;
                    }
                }
                _ => {}
            }
        }
        for voice in &mut t.vocal_occupancy {
            placement(&mut voice.placement, placement_offset)?;
            frame(&mut voice.start_frame, output_offset)?;
            frame(&mut voice.end_frame, output_offset)?;
            frame(&mut voice.tail_end_frame, output_offset)?;
        }
        if let Some(emergency) = &mut t.emergency {
            frame(&mut emergency.activated_output_frame, output_offset)?;
        }
        Ok(t)
    }

    pub fn validate(&self, plan: &MixPlan) -> Result<()> {
        require(
            self.policy_version == TACTICS_POLICY_VERSION
                && !self.tactic_id.is_modifier()
                && self.permission == self.tactic_id.permission()
                && self.modifiers.iter().all(|id| id.is_modifier())
                && self.outgoing_placement < plan.placements.len()
                && self.incoming_placement < plan.placements.len()
                && self.outgoing_placement != self.incoming_placement
                && self.start_frame < self.end_frame
                && self.start_frame <= self.hold_end_frame
                && self.hold_end_frame <= self.end_frame
                && self.start_frame <= self.exit_start_frame
                && self.exit_start_frame <= self.end_frame
                && self.end_frame <= plan.total_output_frames,
            "TACTIC_PLAN",
            "invalid tactic identity or interval",
        )?;
        require(
            self.incoming_placement == self.outgoing_placement + 1
                && plan
                    .transitions
                    .get(self.outgoing_placement)
                    .is_some_and(|t| {
                        t.start_output_frame == self.start_frame
                            && t.end_output_frame == self.end_frame
                            && t.from_track_id == plan.placements[self.outgoing_placement].track_id
                            && t.to_track_id == plan.placements[self.incoming_placement].track_id
                    }),
            "TACTIC_PLAN",
            "tactic must bind the exact adjacent transition",
        )?;
        require(
            self.bar_frames.len() >= 2
                && self.bar_frames[0] == self.start_frame
                && self.bar_frames.last() == Some(&self.end_frame)
                && self.bar_frames.windows(2).all(|w| w[0] < w[1]),
            "TACTIC_PLAN",
            "invalid bar anchors",
        )?;
        require(
            self.modifiers
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .len()
                == self.modifiers.len(),
            "TACTIC_MODIFIER",
            "duplicate tempo modifier",
        )?;
        require(
            self.modifiers.contains(&TacticId::D01) == self.bar_bpm.is_some()
                && self.bar_bpm.as_ref().is_none_or(|v| {
                    v.len() == self.bar_frames.len() && v.iter().all(|b| b.is_finite() && *b > 0.)
                }),
            "TACTIC_MODIFIER",
            "tempo trajectory does not match declared modifier",
        )?;
        require(
            self.modifiers.contains(&TacticId::D02) == self.pulse_ratio.is_some()
                && self.pulse_ratio.is_none_or(|r| {
                    r.is_finite() && ((r - 2.).abs() < 0.02 || (r - 0.5).abs() < 0.01)
                }),
            "TACTIC_MODIFIER",
            "invalid half/double-time pulse mapping",
        )?;
        require(
            !self.evidence.is_empty() && !self.selection_reason.trim().is_empty(),
            "TACTIC_EVIDENCE",
            "tactic needs saved decision evidence",
        )?;
        let [edit_lo, edit_hi] = self.edit_speed_ratio_limits;
        require(
            edit_lo.is_finite()
                && edit_hi.is_finite()
                && edit_lo >= 0.125
                && edit_hi <= 8.
                && edit_lo <= 1.
                && edit_hi >= 1.,
            "TACTIC_EDIT",
            "invalid independent edit-quality limits",
        )?;
        if self.permission == TacticPermission::Normal {
            require(
                edit_lo >= 1. - plan.policy.max_speed_deviation
                    && edit_hi <= 1. + plan.policy.max_speed_deviation,
                "TACTIC_EDIT",
                "ordinary edit limits exceed the saved stretch policy",
            )?;
        }
        if self.tactic_id == TacticId::M01 {
            require(
                self.timing_basis == TacticTimingBasis::Bars
                    && self.bar_frames.len() >= 9
                    && self.hold_end_frame == self.bar_frames[self.bar_frames.len() - 3]
                    && self.exit_start_frame == self.hold_end_frame,
                "TACTIC_MASHUP",
                "M01 must hold at least six bars and resolve over the final two bars",
            )?;
        }
        if matches!(
            self.tactic_id,
            TacticId::S12 | TacticId::C05 | TacticId::C06 | TacticId::C13
        ) {
            let first = self.bar_frames[1] - self.bar_frames[0];
            require(
                !self.modifiers.contains(&TacticId::D01)
                    && self
                        .bar_frames
                        .windows(2)
                        .all(|w| (w[1] - w[0]).abs_diff(first) <= 1),
                "TACTIC_PHASE",
                "fixed-period loop/gate schedule cannot run on a changing mapped bar clock or combine with D01",
            )?;
        }
        if self.tactic_id == TacticId::C13 {
            require(self.source_edits.iter().any(|edit| {
                edit.placement==self.incoming_placement && edit.lane==StemLane::Drums
                    && edit.output_start_frame==self.start_frame && edit.output_end_frame==self.end_frame
                    && matches!(edit.kind, SourceEditKind::Loop { period_frames }
                        if period_frames>=16 && (edit.output_end_frame-edit.output_start_frame)%period_frames==0)
            }),"TACTIC_PHASE","polyrhythm loop must complete whole cycles at its declared common landing")?;
        }
        if self.timing_basis == TacticTimingBasis::Structural {
            require(
                matches!(
                    self.tactic_id,
                    TacticId::S07
                        | TacticId::S11
                        | TacticId::C04
                        | TacticId::E01
                        | TacticId::E02
                        | TacticId::E03
                        | TacticId::E04
                ) && self.modifiers.is_empty(),
                "TACTIC_TIMING",
                "unmetered structure cannot support a rhythmic tactic or tempo modifier",
            )?;
        }
        if self.timing_basis == TacticTimingBasis::IndependentPulses {
            require(
                self.tactic_id == TacticId::C14 && self.modifiers.is_empty(),
                "TACTIC_TIMING",
                "independent pulses require a nonoverlapping percussion tempo bridge",
            )?;
        }
        let placements = [self.outgoing_placement, self.incoming_placement];
        for index in placements {
            plan.placements[index].time_map.validate()?;
        }
        let mut lane_ids = BTreeSet::new();
        for lane in &self.lanes {
            require(
                placements.contains(&lane.placement)
                    && lane_ids.insert((lane.placement, lane.lane)),
                "TACTIC_LANE",
                "duplicate or unrelated lane",
            )?;
            validate_curve(&lane.gain, self.start_frame, self.end_frame, 0., 1.)?;
            if !lane.eq.is_empty() {
                for band in 0..3 {
                    validate_curve(
                        &lane
                            .eq
                            .iter()
                            .map(|p| ScalarPoint {
                                output_frame: p.output_frame,
                                value: p.gains[band],
                            })
                            .collect::<Vec<_>>(),
                        self.start_frame,
                        self.end_frame,
                        0.,
                        1.,
                    )?;
                }
            }
        }
        require(
            placements
                .iter()
                .all(|p| lane_ids.iter().any(|(index, _)| index == p)),
            "TACTIC_LANE",
            "both participating placements need an explicit lane schedule",
        )?;
        for (placement, lane) in &lane_ids {
            if *lane == StemLane::Stereo {
                require(
                    !lane_ids
                        .iter()
                        .any(|(p, l)| p == placement && *l != StemLane::Stereo),
                    "TACTIC_LANE",
                    "stereo and isolated lanes would double audio",
                )?;
            }
        }
        let mut asset_ids = BTreeSet::new();
        for asset in &self.stem_assets {
            require(
                asset.lane != StemLane::Stereo
                    && placements.contains(&asset.placement)
                    && asset_ids.insert((asset.placement, asset.lane))
                    && asset.source_offset_frame < asset.source_end_frame
                    && asset.source_end_frame <= MAX_FRAME,
                "TACTIC_STEM",
                "invalid or duplicate stem asset binding",
            )?;
            let map = &plan.placements[asset.placement].time_map;
            require(
                asset.source_offset_frame as f64 <= map.source_at(self.start_frame as f64)
                    && asset.source_end_frame as f64 >= map.source_at(self.end_frame as f64),
                "TACTIC_STEM",
                "stem asset does not cover the ownership window",
            )?;
        }
        require(
            lane_ids
                .iter()
                .all(|(p, l)| *l == StemLane::Stereo || asset_ids.contains(&(*p, *l))),
            "TACTIC_STEM",
            "isolated lane has no bound source asset",
        )?;
        for fx in &self.effects {
            require(
                lane_ids.contains(&(fx.placement, fx.lane))
                    && fx.start_frame >= self.start_frame
                    && fx.input_end_frame <= self.end_frame
                    && fx.end_frame <= plan.total_output_frames,
                "TACTIC_EFFECT",
                "effect lacks a lane, input extends past ownership, or tail exceeds output",
            )?;
            fx.validate(plan.output.sample_rate_hz)?;
        }
        for edit in &self.source_edits {
            require(
                lane_ids.contains(&(edit.placement, edit.lane))
                    && edit.output_start_frame >= self.start_frame
                    && edit.output_end_frame <= self.end_frame,
                "TACTIC_EDIT",
                "edit lacks lane or exceeds transition",
            )?;
            let p = &plan.placements[edit.placement];
            require(
                edit.source_sample_rate_hz == p.time_map.source_sample_rate_hz
                    && edit.source_start_frame >= p.source_start_frame
                    && edit.source_end_frame <= p.source_end_frame,
                "TACTIC_EDIT",
                "edit capture outside placement source",
            )?;
            require(
                edit.output_end_frame - edit.output_start_frame
                    <= plan.output.sample_rate_hz as u64 * 180,
                "TACTIC_EDIT",
                "edit exceeds bounded buffer duration",
            )?;
            edit.validate()?;
            let capture_seconds = (edit.source_end_frame - edit.source_start_frame) as f64
                / edit.source_sample_rate_hz as f64;
            let speed_ok = |period: f64| {
                let speed = capture_seconds * plan.output.sample_rate_hz as f64 / period;
                speed >= edit_lo - 1e-7 && speed <= edit_hi + 1e-7
            };
            match &edit.kind {
                SourceEditKind::Loop { period_frames } => require(
                    speed_ok(*period_frames as f64),
                    "TACTIC_EDIT",
                    "loop stretch exceeds declared quality limits",
                )?,
                SourceEditKind::LoopSequence { periods } => require(
                    periods.iter().all(|p| speed_ok(p.value)),
                    "TACTIC_EDIT",
                    "loop tempo curve exceeds declared quality limits",
                )?,
                SourceEditKind::Reverse | SourceEditKind::SlipReverse => require(
                    speed_ok((edit.output_end_frame - edit.output_start_frame) as f64),
                    "TACTIC_EDIT",
                    "reverse stretch exceeds declared quality limits",
                )?,
                SourceEditKind::PitchSequence { notes } => require(
                    notes.iter().all(|n| {
                        let speed = 2f64.powf(n.semitones / 12.);
                        speed >= edit_lo && speed <= edit_hi
                    }),
                    "TACTIC_EDIT",
                    "sample pitch exceeds declared quality limits",
                )?,
                SourceEditKind::Brake { .. }
                | SourceEditKind::Backspin { .. }
                | SourceEditKind::Scratch { .. } => require(
                    self.permission == TacticPermission::EmergencyOnly,
                    "TACTIC_EDIT",
                    "turntable recovery motion is emergency-only",
                )?,
            }
        }
        for (i, a) in self.source_edits.iter().enumerate() {
            for b in &self.source_edits[i + 1..] {
                require(
                    a.placement != b.placement
                        || a.lane != b.lane
                        || a.output_end_frame <= b.output_start_frame
                        || b.output_end_frame <= a.output_start_frame,
                    "TACTIC_EDIT",
                    "source edits overlap on the same lane",
                )?;
            }
        }
        if self.tactic_id == TacticId::M01 {
            let gains = |placement, lane| {
                self.lanes
                    .iter()
                    .find(|l| l.placement == placement && l.lane == lane)
                    .map(|l| &l.gain)
            };
            require(
                gains(self.outgoing_placement, StemLane::Drums)
                    .is_some_and(|g| g.iter().all(|p| p.value == 0.))
                    && gains(self.incoming_placement, StemLane::Drums)
                        .is_some_and(|g| g.iter().all(|p| p.value == 1.)),
                "TACTIC_MASHUP",
                "M01 requires complete drum replacement and steady B drums",
            )?;
            for lane in [StemLane::Bass, StemLane::Other] {
                for (placement, hold, final_gain) in [
                    (self.outgoing_placement, 1., 0.),
                    (self.incoming_placement, 0., 1.),
                ] {
                    require(
                        gains(placement, lane).is_some_and(|g| {
                            g.iter()
                                .filter(|p| p.output_frame <= self.exit_start_frame)
                                .all(|p| p.value == hold)
                                && g.iter().any(|p| {
                                    p.output_frame == self.exit_start_frame && p.value == hold
                                })
                                && g.last().is_some_and(|p| p.value == final_gain)
                        }),
                        "TACTIC_MASHUP",
                        "M01 accompaniment cannot move before the selected hold ends",
                    )?;
                }
            }
        }
        require(
            self.vocal_guard_frames >= ((plan.output.sample_rate_hz as f64 * 0.300).ceil() as u64),
            "TACTIC_VOCALS",
            "vocal guard below 300 ms",
        )?;
        for v in &self.vocal_occupancy {
            require(
                placements.contains(&v.placement)
                    && v.start_frame < v.end_frame
                    && v.end_frame <= v.tail_end_frame
                    && v.tail_end_frame <= plan.total_output_frames
                    && v.supported_entry
                    && v.supported_exit,
                "TACTIC_VOCALS",
                "invalid or incomplete vocal phrase",
            )?;
        }
        for a in &self.vocal_occupancy {
            for b in &self.vocal_occupancy {
                if a.placement != b.placement && a.start_frame <= b.start_frame {
                    require(
                        a.tail_end_frame.saturating_add(self.vocal_guard_frames) <= b.start_frame,
                        "TACTIC_VOCALS",
                        "vocals or wet tails overlap the guard interval",
                    )?;
                }
            }
        }
        match (&self.emergency, self.permission) {
            (Some(e), TacticPermission::EmergencyOnly) => {
                require(
                    e.next_source_verified
                        && !e.failed_condition.trim().is_empty()
                        && !e.rejected_safe_recoveries.is_empty()
                        && e.activated_output_frame >= self.start_frame
                        && e.activated_output_frame < self.end_frame,
                    "TACTIC_EMERGENCY",
                    "missing verified recovery evidence",
                )?;
                if self.tactic_id != TacticId::E04 {
                    require(
                        e.healthy_buffer_frames > 0,
                        "TACTIC_EMERGENCY",
                        "effect recovery requires healthy buffered audio",
                    )?;
                }
            }
            (None, TacticPermission::Normal | TacticPermission::ExpressiveOptIn) => {}
            _ => {
                return Err(Error::new(
                    "TACTIC_EMERGENCY",
                    "emergency permission and activation must agree",
                ))
            }
        }
        use TacticId::*;
        let executable = match self.tactic_id {
            M01 | M02 | M03 | M04 | M05 | M06 | S05 | S06 => placements.iter().all(|p| {
                [
                    StemLane::Drums,
                    StemLane::Bass,
                    StemLane::Other,
                    StemLane::Vocals,
                ]
                .iter()
                .all(|l| lane_ids.contains(&(*p, *l)))
            }),
            S01 | S03 | S04 => self.lanes.iter().any(|l| !l.eq.is_empty()),
            S08 => self
                .effects
                .iter()
                .any(|e| matches!(e.kind, EffectKind::Filter { .. })),
            S09 | S10 => self
                .effects
                .iter()
                .any(|e| matches!(e.kind, EffectKind::Delay { .. })),
            S11 => self
                .effects
                .iter()
                .any(|e| matches!(e.kind, EffectKind::Reverb { .. })),
            S12 | C05 => self.source_edits.iter().any(|e| {
                matches!(
                    e.kind,
                    SourceEditKind::Loop { .. } | SourceEditKind::LoopSequence { .. }
                )
            }),
            C06 => self
                .effects
                .iter()
                .any(|e| matches!(e.kind, EffectKind::Gate { .. })),
            C08 => self.source_edits.iter().any(|e| {
                matches!(e.kind, SourceEditKind::PitchSequence { .. }) && e.lane != StemLane::Stereo
            }),
            C09 => self
                .effects
                .iter()
                .any(|e| matches!(e.kind, EffectKind::Noise { .. })),
            C10 => self
                .effects
                .iter()
                .any(|e| matches!(e.kind, EffectKind::Spiral { .. })),
            C11 => self.source_edits.iter().any(|e| {
                e.placement == self.incoming_placement
                    && matches!(e.kind, SourceEditKind::Loop { .. })
            }),
            C12 => self.source_edits.iter().any(|e| {
                matches!(
                    e.kind,
                    SourceEditKind::Reverse | SourceEditKind::SlipReverse
                )
            }),
            C13 => self.source_edits.iter().any(|e| {
                e.placement == self.incoming_placement
                    && e.lane == StemLane::Drums
                    && matches!(e.kind, SourceEditKind::Loop { .. })
            }),
            C14 => self.source_edits.iter().any(|e| {
                e.placement == self.outgoing_placement
                    && e.lane == StemLane::Drums
                    && matches!(e.kind, SourceEditKind::LoopSequence { .. })
            }),
            E01 => self
                .source_edits
                .iter()
                .any(|e| matches!(e.kind, SourceEditKind::Brake { .. })),
            E02 => self
                .source_edits
                .iter()
                .any(|e| matches!(e.kind, SourceEditKind::Backspin { .. })),
            E03 => self
                .source_edits
                .iter()
                .any(|e| matches!(e.kind, SourceEditKind::Scratch { .. })),
            _ => true,
        };
        require(
            executable,
            "TACTIC_EXECUTION",
            "tactic name has no corresponding executable lane/effect/edit schedule",
        )?;
        Ok(())
    }
}

pub fn read_tactics(plan: &MixPlan) -> Result<Vec<TacticPlan>> {
    let Some(value) = plan.header.extensions.get(TACTICS_EXTENSION) else {
        return Ok(vec![]);
    };
    let tactics: Vec<TacticPlan> = serde_json::from_value(value.clone())
        .map_err(|e| Error::new("TACTIC_SCHEMA", e.to_string()))?;
    require(
        tactics.len() <= plan.transitions.len(),
        "TACTIC_PLAN",
        "more tactic schedules than transitions",
    )?;
    validate_tactics(plan, &tactics)?;
    Ok(tactics)
}
pub fn write_tactics(plan: &mut MixPlan, tactics: Vec<TacticPlan>) -> Result<()> {
    validate_tactics(plan, &tactics)?;
    require(
        tactics.len() <= plan.transitions.len(),
        "TACTIC_PLAN",
        "more tactic schedules than transitions",
    )?;
    plan.header.extensions.insert(
        TACTICS_EXTENSION.into(),
        serde_json::to_value(tactics).map_err(|e| Error::new("TACTIC_SCHEMA", e.to_string()))?,
    );
    Ok(())
}

fn validate_tactics(plan: &MixPlan, tactics: &[TacticPlan]) -> Result<()> {
    let mut transitions = BTreeSet::new();
    for tactic in tactics {
        tactic.validate(plan)?;
        require(
            transitions.insert(tactic.outgoing_placement),
            "TACTIC_PLAN",
            "duplicate tactic ownership of transition",
        )?;
    }
    for (i, a) in tactics.iter().enumerate() {
        for b in &tactics[i + 1..] {
            for av in &a.vocal_occupancy {
                for bv in &b.vocal_occupancy {
                    if av.placement == bv.placement {
                        continue;
                    }
                    let guard = a.vocal_guard_frames.max(b.vocal_guard_frames);
                    let safe = if av.start_frame <= bv.start_frame {
                        av.tail_end_frame.saturating_add(guard) <= bv.start_frame
                    } else {
                        bv.tail_end_frame.saturating_add(guard) <= av.start_frame
                    };
                    require(
                        safe,
                        "TACTIC_VOCALS",
                        "vocal tail collides across chained tactic windows",
                    )?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (MixPlan, TacticPlan) {
        let rate = 48000;
        let end = 192000;
        let total = end * 3;
        let policy = MixingConfig::default();
        let placements = (0..2)
            .map(|i| Placement {
                track_id: format!("track-{i}"),
                audio_hash: hash_bytes(b"fixture"),
                audio_revision: 1,
                analysis: FileRef {
                    path: format!("analysis-{i}"),
                    hash: hash_bytes(b"analysis"),
                },
                override_revision: 0,
                source_start_frame: 0,
                source_end_frame: total,
                time_map: TimeMap {
                    header: Header::default(),
                    map_id: format!("map-{i}"),
                    source_sample_rate_hz: rate,
                    output_sample_rate_hz: rate,
                    interpolation: "monotone_cubic".into(),
                    anchors: vec![
                        Anchor {
                            source_frame: 0,
                            output_frame: 0,
                        },
                        Anchor {
                            source_frame: total,
                            output_frame: total,
                        },
                    ],
                    speed_ratio_limits: [0.94, 1.06],
                    max_speed_change_per_second: 0.02,
                    preserve_pitch: true,
                },
                automation: vec![
                    AutomationPoint {
                        output_frame: 0,
                        gain_linear: 1.,
                        bands_linear: [1.; 3],
                        mute: false,
                    },
                    AutomationPoint {
                        output_frame: total,
                        gain_linear: 1.,
                        bands_linear: [1.; 3],
                        mute: false,
                    },
                ],
            })
            .collect();
        let plan = MixPlan {
            header: Header::default(),
            plan_id: "fixture".into(),
            planner_version: "fixture".into(),
            renderer_version: "fixture".into(),
            policy_hash: canonical_hash(&policy).unwrap(),
            policy,
            seed: 0,
            objective: "fixture".into(),
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
            placements,
            transitions: vec![Transition {
                transition_id: "join".into(),
                from_track_id: "track-0".into(),
                to_track_id: "track-1".into(),
                outgoing_region_id: "exit".into(),
                incoming_region_id: "entry".into(),
                start_output_frame: 0,
                end_output_frame: end,
                template: Template::ShortCrossfade,
                costs: BTreeMap::new(),
                fallback_reason: None,
            }],
            total_output_frames: total,
            crossover_hz: [200., 2500.],
            crossover_design: "complementary_one_pole_v1".into(),
            automation_interpolation: "smoothstep_linear_amplitude".into(),
            diagnostics: vec![],
        };
        let tactic = TacticPlan {
            policy_version: TACTICS_POLICY_VERSION.into(),
            tactic_id: TacticId::S02,
            modifiers: vec![],
            arrangement: TacticArrangement::Blend,
            character: TacticCharacter::Subtle,
            permission: TacticPermission::Normal,
            outgoing_placement: 0,
            incoming_placement: 1,
            start_frame: 0,
            hold_end_frame: 0,
            exit_start_frame: 0,
            end_frame: end,
            bar_frames: vec![0, end / 2, end],
            timing_basis: TacticTimingBasis::Bars,
            bar_bpm: None,
            pulse_ratio: None,
            lanes: vec![
                LaneSchedule {
                    placement: 0,
                    lane: StemLane::Stereo,
                    gain: vec![
                        ScalarPoint {
                            output_frame: 0,
                            value: 1.,
                        },
                        ScalarPoint {
                            output_frame: end,
                            value: 0.,
                        },
                    ],
                    eq: vec![],
                },
                LaneSchedule {
                    placement: 1,
                    lane: StemLane::Stereo,
                    gain: vec![
                        ScalarPoint {
                            output_frame: 0,
                            value: 0.,
                        },
                        ScalarPoint {
                            output_frame: end,
                            value: 1.,
                        },
                    ],
                    eq: vec![],
                },
            ],
            stem_assets: vec![],
            effects: vec![],
            source_edits: vec![],
            edit_speed_ratio_limits: [0.94, 1.06],
            vocal_occupancy: vec![],
            vocal_guard_frames: 14400,
            evidence: vec!["measured-fixture".into()],
            selection_reason: "complete phrase and aligned pulse".into(),
            emergency: None,
        };
        (plan, tactic)
    }
    fn loop_edit() -> SourceEditSpec {
        SourceEditSpec {
            placement: 0,
            lane: StemLane::Stereo,
            source_sample_rate_hz: 48000,
            source_start_frame: 0,
            source_end_frame: 96000,
            output_start_frame: 0,
            output_end_frame: 192000,
            declick_frames: 96,
            transport: SourceTransport::ContinueUnderlying,
            kind: SourceEditKind::Loop {
                period_frames: 96000,
            },
        }
    }

    #[test]
    fn saved_fixed_loop_roll_and_gate_schedules_reject_changing_clocks() {
        for id in [TacticId::S12, TacticId::C05, TacticId::C06] {
            let (p, mut t) = fixture();
            t.tactic_id = id;
            t.permission = id.permission();
            if id == TacticId::C06 {
                t.effects = (0..2)
                    .map(|placement| EffectSpec {
                        placement,
                        lane: StemLane::Stereo,
                        start_frame: 0,
                        input_end_frame: t.end_frame,
                        end_frame: t.end_frame,
                        wet: vec![
                            ScalarPoint {
                                output_frame: 0,
                                value: 1.,
                            },
                            ScalarPoint {
                                output_frame: t.end_frame,
                                value: 1.,
                            },
                        ],
                        kind: EffectKind::Gate {
                            period_frames: 24000,
                            phase_frames: if placement == 0 { 0 } else { 12000 },
                            duty_cycle: 0.5,
                            smoothing_frames: 240,
                        },
                    })
                    .collect();
            } else {
                t.source_edits.push(loop_edit());
            }
            t.validate(&p).unwrap();
            let mut bad = t.clone();
            bad.modifiers.push(TacticId::D01);
            bad.bar_bpm = Some(vec![120., 121., 122.]);
            assert_eq!(bad.validate(&p).unwrap_err().code, "TACTIC_PHASE", "{id:?}");
            let mut bad = t;
            bad.bar_frames[1] += 100;
            assert_eq!(bad.validate(&p).unwrap_err().code, "TACTIC_PHASE", "{id:?}");
        }
    }
    #[test]
    fn saved_polyrhythm_requires_a_steady_clock_and_whole_loop_landing() {
        let (p, mut t) = fixture();
        t.tactic_id = TacticId::C13;
        t.permission = TacticPermission::ExpressiveOptIn;
        t.edit_speed_ratio_limits = [0.5, 2.];
        t.bar_frames = vec![0, 64000, 128000, 192000];
        t.lanes[1].lane = StemLane::Drums;
        t.stem_assets = vec![StemAsset {
            placement: 1,
            lane: StemLane::Drums,
            audio: FileRef {
                path: "verified-drums.wav".into(),
                hash: hash_bytes(b"drums"),
            },
            source_offset_frame: 0,
            source_end_frame: 192000,
        }];
        let mut edit = loop_edit();
        edit.placement = 1;
        edit.lane = StemLane::Drums;
        edit.source_end_frame = 64000;
        edit.kind = SourceEditKind::Loop {
            period_frames: 48000,
        };
        t.source_edits = vec![edit];
        t.validate(&p).unwrap();
        let mut bad = t.clone();
        bad.source_edits[0].kind = SourceEditKind::Loop {
            period_frames: 48001,
        };
        assert_eq!(bad.validate(&p).unwrap_err().code, "TACTIC_PHASE");
        let mut bad = t.clone();
        bad.bar_frames[1] += 100;
        assert_eq!(bad.validate(&p).unwrap_err().code, "TACTIC_PHASE");
        let mut bad = t;
        bad.modifiers.push(TacticId::D01);
        bad.bar_bpm = Some(vec![180., 181., 182., 183.]);
        assert_eq!(bad.validate(&p).unwrap_err().code, "TACTIC_PHASE");
    }
    #[test]
    fn legacy_plans_remain_valid_and_typed_extension_rejects_unknown_fields() {
        let (mut p, t) = fixture();
        assert!(read_tactics(&p).unwrap().is_empty());
        write_tactics(&mut p, vec![t]).unwrap();
        assert_eq!(read_tactics(&p).unwrap().len(), 1);
        p.header.extensions.get_mut(TACTICS_EXTENSION).unwrap()[0]["ignored_dsp"] =
            serde_json::json!(true);
        assert_eq!(read_tactics(&p).unwrap_err().code, "TACTIC_SCHEMA");
    }
    #[test]
    fn duplicate_schedule_wrong_binding_and_missing_effect_are_rejected() {
        let (mut p, t) = fixture();
        assert_eq!(
            write_tactics(&mut p, vec![t.clone(), t.clone()])
                .unwrap_err()
                .code,
            "TACTIC_PLAN"
        );
        let mut bad = t.clone();
        bad.end_frame -= 1;
        assert!(bad.validate(&p).is_err());
        let mut bad = t;
        bad.tactic_id = TacticId::S08;
        assert_eq!(bad.validate(&p).unwrap_err().code, "TACTIC_EXECUTION");
    }
    #[test]
    fn normal_loop_cannot_bypass_stretch_policy_and_edits_cannot_overlap() {
        let (p, mut t) = fixture();
        t.tactic_id = TacticId::S12;
        t.source_edits = vec![loop_edit()];
        t.validate(&p).unwrap();
        t.source_edits[0].kind = SourceEditKind::Loop {
            period_frames: 48000,
        };
        assert_eq!(t.validate(&p).unwrap_err().code, "TACTIC_EDIT");
        t.source_edits = vec![loop_edit(), loop_edit()];
        assert_eq!(t.validate(&p).unwrap_err().code, "TACTIC_EDIT");
    }
    #[test]
    fn effects_cannot_capture_after_ownership_and_freeze_requires_real_capture() {
        let (p, mut t) = fixture();
        t.tactic_id = TacticId::S09;
        let fx = EffectSpec {
            placement: 0,
            lane: StemLane::Stereo,
            start_frame: 0,
            input_end_frame: 96000,
            end_frame: 192000,
            wet: vec![
                ScalarPoint {
                    output_frame: 0,
                    value: 0.3,
                },
                ScalarPoint {
                    output_frame: 192000,
                    value: 0.,
                },
            ],
            kind: EffectKind::Delay {
                delay_frames: vec![
                    ScalarPoint {
                        output_frame: 0,
                        value: 24000.,
                    },
                    ScalarPoint {
                        output_frame: 192000,
                        value: 24000.,
                    },
                ],
                feedback: 0.4,
                freeze_at: Some(96000),
                high_pass_hz: 100.,
                low_pass_hz: 8000.,
            },
        };
        fx.validate(48000).unwrap();
        t.effects.push(fx);
        t.validate(&p).unwrap();
        t.effects[0].input_end_frame = t.end_frame + 1;
        t.effects[0].end_frame = t.end_frame + 2;
        assert_eq!(t.validate(&p).unwrap_err().code, "TACTIC_EFFECT");
    }
    #[test]
    fn isolated_lanes_require_complete_unique_asset_bindings() {
        let (p, mut t) = fixture();
        t.lanes[0].lane = StemLane::Other;
        assert_eq!(t.validate(&p).unwrap_err().code, "TACTIC_STEM");
        let asset = StemAsset {
            placement: 0,
            lane: StemLane::Other,
            audio: FileRef {
                path: "stem.wav".into(),
                hash: hash_bytes(b"stem"),
            },
            source_offset_frame: 0,
            source_end_frame: t.end_frame,
        };
        t.stem_assets.push(asset.clone());
        t.validate(&p).unwrap();
        t.stem_assets.push(asset);
        assert_eq!(t.validate(&p).unwrap_err().code, "TACTIC_STEM");
        t.stem_assets.pop();
        t.stem_assets[0].source_offset_frame = 1;
        assert_eq!(t.validate(&p).unwrap_err().code, "TACTIC_STEM");
    }
    #[test]
    fn cross_join_vocal_tail_is_validated_against_the_following_track() {
        let (mut p, mut a) = fixture();
        let mut third = p.placements[1].clone();
        third.track_id = "track-2".into();
        p.placements.push(third);
        let mut join = p.transitions[0].clone();
        join.transition_id = "join-2".into();
        join.from_track_id = "track-1".into();
        join.to_track_id = "track-2".into();
        join.start_output_frame = 192000;
        join.end_output_frame = 384000;
        p.transitions.push(join);
        a.vocal_occupancy.push(VocalOccupancy {
            placement: 0,
            start_frame: 48000,
            end_frame: 96000,
            tail_end_frame: 210000,
            supported_entry: true,
            supported_exit: true,
        });
        let mut b = a.clone();
        b.outgoing_placement = 1;
        b.incoming_placement = 2;
        b.start_frame = 192000;
        b.end_frame = 384000;
        b.hold_end_frame = 192000;
        b.exit_start_frame = 192000;
        b.bar_frames = vec![192000, 288000, 384000];
        for l in &mut b.lanes {
            l.placement += 1;
            for point in &mut l.gain {
                point.output_frame += 192000;
            }
        }
        b.vocal_occupancy = vec![VocalOccupancy {
            placement: 2,
            start_frame: 211000,
            end_frame: 250000,
            tail_end_frame: 250000,
            supported_entry: true,
            supported_exit: true,
        }];
        assert_eq!(
            write_tactics(&mut p, vec![a.clone(), b.clone()])
                .unwrap_err()
                .code,
            "TACTIC_VOCALS"
        );
        b.vocal_occupancy[0].start_frame = 225000;
        write_tactics(&mut p, vec![a, b]).unwrap();
    }
    #[test]
    fn malformed_time_map_does_not_panic_while_validating_stems() {
        let (mut p, mut t) = fixture();
        p.placements[0].time_map.anchors.clear();
        t.lanes[0].lane = StemLane::Other;
        assert_eq!(t.validate(&p).unwrap_err().code, "TIME_MAP");
    }

    #[test]
    fn chain_translation_shifts_nested_output_time_and_preserves_source_time() {
        let (_, mut t) = fixture();
        t.effects = vec![
            EffectSpec {
                placement: 0,
                lane: StemLane::Stereo,
                start_frame: 0,
                input_end_frame: 100,
                end_frame: 200,
                wet: vec![
                    ScalarPoint {
                        output_frame: 0,
                        value: 0.3,
                    },
                    ScalarPoint {
                        output_frame: 200,
                        value: 0.,
                    },
                ],
                kind: EffectKind::Delay {
                    delay_frames: vec![
                        ScalarPoint {
                            output_frame: 0,
                            value: 24.,
                        },
                        ScalarPoint {
                            output_frame: 200,
                            value: 24.,
                        },
                    ],
                    feedback: 0.4,
                    freeze_at: Some(100),
                    high_pass_hz: 100.,
                    low_pass_hz: 8000.,
                },
            },
            EffectSpec {
                placement: 1,
                lane: StemLane::Stereo,
                start_frame: 0,
                input_end_frame: 200,
                end_frame: 200,
                wet: vec![
                    ScalarPoint {
                        output_frame: 0,
                        value: 1.,
                    },
                    ScalarPoint {
                        output_frame: 200,
                        value: 1.,
                    },
                ],
                kind: EffectKind::Gate {
                    period_frames: 100,
                    phase_frames: 50,
                    duty_cycle: 0.5,
                    smoothing_frames: 3,
                },
            },
        ];
        let mut e = loop_edit();
        e.kind = SourceEditKind::Scratch {
            positions: vec![
                SourcePositionPoint {
                    output_frame: 0,
                    source_frame: 2.,
                },
                SourcePositionPoint {
                    output_frame: 96000,
                    source_frame: 95999.,
                },
                SourcePositionPoint {
                    output_frame: 192000,
                    source_frame: 20.,
                },
            ],
        };
        e.transport = SourceTransport::ResumeAt { source_frame: 123 };
        t.source_edits.push(e);
        let mut e = loop_edit();
        e.kind = SourceEditKind::LoopSequence {
            periods: vec![
                ScalarPoint {
                    output_frame: 0,
                    value: 96000.,
                },
                ScalarPoint {
                    output_frame: 192000,
                    value: 64000.,
                },
            ],
        };
        t.source_edits.push(e);
        let mut e = loop_edit();
        e.kind = SourceEditKind::PitchSequence {
            notes: vec![PitchNote {
                start_frame: 100,
                end_frame: 200,
                semitones: 3.,
                gain: 0.8,
            }],
        };
        t.source_edits.push(e);
        t.vocal_occupancy.push(VocalOccupancy {
            placement: 0,
            start_frame: 100,
            end_frame: 200,
            tail_end_frame: 300,
            supported_entry: true,
            supported_exit: true,
        });
        t.stem_assets.push(StemAsset {
            placement: 1,
            lane: StemLane::Other,
            audio: FileRef {
                path: "full-stem.wav".into(),
                hash: hash_bytes(b"stem"),
            },
            source_offset_frame: 0,
            source_end_frame: 900000,
        });
        t.emergency = Some(EmergencyActivation {
            reason: EmergencyReason::UnexpectedSourceLoss,
            activated_output_frame: 80,
            failed_condition: "fixture".into(),
            rejected_safe_recoveries: vec!["fixture".into()],
            next_source_verified: true,
            healthy_buffer_frames: 400,
        });
        let moved = t.translated(1000, 2).unwrap();
        assert_eq!(
            (
                moved.start_frame,
                moved.outgoing_placement,
                moved.incoming_placement
            ),
            (1000, 2, 3)
        );
        assert_eq!(moved.lanes[0].gain[0].output_frame, 1000);
        assert_eq!(moved.effects[0].input_end_frame, 1100);
        if let EffectKind::Delay {
            delay_frames,
            freeze_at,
            ..
        } = &moved.effects[0].kind
        {
            assert_eq!(*freeze_at, Some(1100));
            assert_eq!(
                (delay_frames[0].output_frame, delay_frames[0].value),
                (1000, 24.)
            );
        } else {
            panic!()
        }
        if let EffectKind::Gate { phase_frames, .. } = moved.effects[1].kind {
            assert_eq!(phase_frames, 50);
        } else {
            panic!()
        }
        if let SourceEditKind::Scratch { positions } = &moved.source_edits[0].kind {
            assert_eq!(
                (positions[1].output_frame, positions[1].source_frame),
                (97000, 95999.)
            );
        } else {
            panic!()
        }
        if let SourceTransport::ResumeAt { source_frame } = moved.source_edits[0].transport {
            assert_eq!(source_frame, 123);
        } else {
            panic!()
        }
        assert_eq!(
            (
                moved.source_edits[0].source_start_frame,
                moved.source_edits[0].source_end_frame
            ),
            (0, 96000)
        );
        if let SourceEditKind::LoopSequence { periods } = &moved.source_edits[1].kind {
            assert_eq!(
                (periods[1].output_frame, periods[1].value),
                (193000, 64000.)
            );
        } else {
            panic!()
        }
        if let SourceEditKind::PitchSequence { notes } = &moved.source_edits[2].kind {
            assert_eq!(
                (notes[0].start_frame, notes[0].end_frame, notes[0].semitones),
                (1100, 1200, 3.)
            );
        } else {
            panic!()
        }
        assert_eq!(
            (
                moved.vocal_occupancy[0].placement,
                moved.vocal_occupancy[0].tail_end_frame
            ),
            (2, 1300)
        );
        assert_eq!(
            (
                moved.stem_assets[0].placement,
                moved.stem_assets[0].source_end_frame
            ),
            (3, 900000)
        );
        assert_eq!(moved.emergency.unwrap().activated_output_frame, 1080);
        assert_eq!(
            serde_json::to_value(t.translated(0, 0).unwrap()).unwrap(),
            serde_json::to_value(&t).unwrap()
        );
        assert_eq!(
            t.translated(MAX_FRAME, 0).unwrap_err().code,
            "TACTIC_TRANSLATION"
        );
        assert_eq!(
            t.translated(0, usize::MAX).unwrap_err().code,
            "TACTIC_TRANSLATION"
        );
    }
}
