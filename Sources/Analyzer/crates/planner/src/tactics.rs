//! Pure cue/tactic selection from measured, time-bounded evidence.
use mixlib_types::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum TacticRole {
    Rest,
    LowPercussionBed,
    SustainedTexture,
    DrumReturn,
    SteadyGroove,
    Mixed,
    Intro,
    Outro,
    Breakdown,
    Build,
    Drop,
    Verse,
    Chorus,
    VocalForeground,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceWindow {
    pub sample_rate_hz: u32,
    pub start_frame: u64,
    pub end_frame: u64,
    pub output_start_frame: u64,
    pub output_end_frame: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BarEvidence {
    pub start_frame: u64,
    pub end_frame: u64,
    pub rms: Option<f64>,
    pub crest_db: Option<f64>,
    pub onsets_per_beat: Option<f64>,
    pub low_occupancy: Option<f64>,
    pub mid_occupancy: Option<f64>,
    #[serde(default)]
    pub high_occupancy: Option<f64>,
    pub active_stems: Option<u8>,
    pub chroma: Option<[f64; 12]>,
    pub beats_per_bar: Option<u16>,
    pub grid_score: Option<f64>,
    pub stability: Option<f64>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BoundaryKind {
    Entry,
    Exit,
    Both,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PhraseBoundary {
    pub output_frame: u64,
    /// Supported structural unit only; Some(4) does not assert an eight- or
    /// sixteen-bar phrase, nor lyric completion.
    #[serde(default)]
    pub supported_period_bars: Option<u16>,
    pub kind: BoundaryKind,
    pub score_margin: f64,
    pub complete: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VocalPhrase {
    pub start_frame: u64,
    pub end_frame: u64,
    pub tail_end_frame: u64,
    pub entry_supported: bool,
    pub exit_supported: bool,
    pub confidence_margin: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StemEvidence {
    pub available: BTreeSet<StemLane>,
    pub max_vocal_leakage_db: f64,
    pub reconstruction_error: f64,
    pub artifact_score: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StemLeakageWindow {
    pub lane: StemLane,
    pub start_frame: u64,
    pub end_frame: u64,
    /// Mapped suspect spans; None is unassessed, Some([]) is measured with
    /// no leakage veto. This proxy does not measure leakage amplitude in dB.
    pub suspect_spans: Option<Vec<[u64; 2]>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoopEvidence {
    pub lane: StemLane,
    pub source_start_frame: u64,
    pub source_end_frame: u64,
    pub output_start_frame: u64,
    pub output_end_frame: u64,
    pub seam_error: f64,
    pub tonal_cycle_distance: f64,
    pub vocal_free: bool,
    pub contains_complete_language: Option<bool>,
    pub confidence_margin: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MotifEvidence {
    pub source_start_frame: u64,
    pub source_end_frame: u64,
    pub phrase_end_frame: u64,
    pub lane: StemLane,
    pub isolation_score: f64,
    pub transcription_margin: f64,
    #[serde(default)]
    pub sample: Option<MotifSample>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MotifSample {
    pub source_start_frame: u64,
    pub source_end_frame: u64,
    pub midi_note: f64,
    pub single_fundamental_score: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PassageFacts {
    pub placement: usize,
    pub source: SourceWindow,
    pub bars: Vec<BarEvidence>,
    /// Scores are classifier margins/heuristics, never calibrated probability.
    pub roles: BTreeMap<TacticRole, f64>,
    pub phrase_boundaries: Vec<PhraseBoundary>,
    /// None means unknown; Some([]) is affirmative measured absence.
    pub vocal_phrases: Option<Vec<VocalPhrase>>,
    /// Affirmative, PCM-checked quiet intervals in mapped output frames.
    /// Outside these intervals the vocal lane may contain a line.
    #[serde(default)]
    pub vocal_quiet: Option<Vec<[u64; 2]>>,
    pub stems: Option<StemEvidence>,
    #[serde(default)]
    pub stem_leakage: Option<Vec<StemLeakageWindow>>,
    #[serde(default)]
    pub endpoint_reasons: Vec<String>,
    #[serde(default)]
    pub phrase_exit: Option<PhraseExit>,
    pub stem_assets: Vec<StemAsset>,
    pub loop_candidates: Vec<LoopEvidence>,
    pub motif: Option<MotifEvidence>,
    pub version_identity: Option<String>,
    pub natural_end_frame: Option<u64>,
    /// Verified drop/attack arrival, rather than a role label over a whole window.
    pub arrival_frame: Option<u64>,
    /// Downbeats with a measured attack, before phrase or vocal admission.
    #[serde(default)]
    pub cut_attacks: Vec<u64>,
    /// End of a measured build touching the window, or a frame past the
    /// window when the current placement cannot reach the release.
    #[serde(default)]
    pub build_release_frame: Option<u64>,
    pub ready_buffer_frames: u64,
    pub evidence: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SemanticRelation {
    pub relation: String,
    pub confidence_margin: f64,
    pub evidence: Vec<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PairProfile {
    pub method: String,
    pub evidence: Vec<String>,
    pub harmonic_distance: Option<f64>,
    pub lead_conflict: Option<f64>,
    pub complementarity: Option<f64>,
    pub predicted_peak_dbfs: Option<f64>,
    pub selected_composite_density: Option<Vec<f64>>,
    pub retained_bed_density: Option<Vec<f64>>,
    /// Source content on the sustained scheduled lanes, before intentional
    /// gain fades. These are separate deck measurements, never composite RMS.
    #[serde(default)]
    pub outgoing_layer_stability: Option<Vec<f64>>,
    #[serde(default)]
    pub incoming_layer_stability: Option<Vec<f64>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShortFadeMatch {
    QuietNearMatch,
    AllBandsMatch,
    OneBandDifference,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PairEvidence {
    #[serde(default)]
    pub profiles: BTreeMap<TacticId, PairProfile>,
    #[serde(default)]
    pub swift_profiles: BTreeMap<TacticId, PairProfile>,
    #[serde(default)]
    pub rhythmic_exit: Option<PhraseExit>,
    #[serde(default)]
    pub rhythmic_profiles: BTreeMap<TacticId, PairProfile>,
    pub beat_skew_ms: Option<f64>,
    pub harmonic_distance: Option<f64>,
    pub lead_conflict: Option<f64>,
    pub complementarity: Option<f64>,
    pub predicted_peak_dbfs: Option<f64>,
    /// Measured after planned stem selection and gain, one value per candidate bar.
    pub selected_composite_density: Option<Vec<f64>>,
    pub retained_bed_density: Option<Vec<f64>>,
    pub pulse_ratio: Option<f64>,
    pub target_tempo_ratio: Option<f64>,
    pub groove_similarity: Option<f64>,
    #[serde(default)]
    pub short_fade_match: Option<ShortFadeMatch>,
    /// Positive mapped PCM agreement in every band around the actual cut.
    /// Absence of overlap or matching key/BPM alone cannot establish this.
    #[serde(default)]
    pub cut_band_matches: BTreeSet<TacticId>,
    pub semantic_relation: Option<SemanticRelation>,
    pub shared_motif: Option<Vec<PitchNote>>,
    /// Output tempo at every bar anchor, validated against both actual time maps.
    pub shared_bar_bpm: Option<Vec<f64>>,
    pub tempo_maps_validated: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CandidateFacts {
    /// Active exit variant; absent keeps the conventional resolution.
    #[serde(default)]
    pub phrase_exit: Option<PhraseExit>,
    pub output_sample_rate_hz: u32,
    pub bar_frames: Vec<u64>,
    #[serde(default)]
    pub timing_basis: TacticTimingBasis,
    pub a: PassageFacts,
    pub b: PassageFacts,
    pub pair: PairEvidence,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum RenderCapability {
    Gain,
    Eq,
    Stems,
    Filter,
    Delay,
    Reverb,
    Gate,
    Noise,
    Spiral,
    SourceEdits,
    PitchSampler,
    TempoMap,
    PulseMapping,
    EmergencyRecovery,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TacticPolicy {
    pub allow_expressive: bool,
    #[serde(default)]
    pub allow_one_sided_filter: bool,
    pub emergency: Option<EmergencyActivation>,
    pub max_beat_skew_ms: f64,
    pub max_harmonic_distance: f64,
    pub max_lead_conflict: f64,
    pub max_vocal_leakage_db: f64,
    pub max_reconstruction_error: f64,
    pub max_artifact_score: f64,
    pub max_peak_dbfs: f64,
    pub minimum_boundary_margin: f64,
    pub normal_speed_deviation: f64,
    pub expressive_edit_speed_limits: [f64; 2],
    pub capabilities: BTreeSet<RenderCapability>,
}
impl Default for TacticPolicy {
    fn default() -> Self {
        Self {
            allow_expressive: false,
            allow_one_sided_filter: false,
            emergency: None,
            max_beat_skew_ms: 30.,
            max_harmonic_distance: 0.30,
            max_lead_conflict: 0.25,
            max_vocal_leakage_db: -35.,
            max_reconstruction_error: 0.05,
            max_artifact_score: 0.20,
            max_peak_dbfs: -1.,
            minimum_boundary_margin: 0.20,
            normal_speed_deviation: 0.06,
            expressive_edit_speed_limits: [0.5, 2.],
            capabilities: [
                RenderCapability::Gain,
                RenderCapability::Eq,
                RenderCapability::TempoMap,
            ]
            .into_iter()
            .collect(),
        }
    }
}
impl TacticPolicy {
    pub fn validate(&self) -> Result<()> {
        require(
            self.max_beat_skew_ms.is_finite()
                && (0.0..=30.).contains(&self.max_beat_skew_ms)
                && [
                    self.max_harmonic_distance,
                    self.max_lead_conflict,
                    self.max_reconstruction_error,
                    self.max_artifact_score,
                    self.minimum_boundary_margin,
                ]
                .iter()
                .all(|v| v.is_finite() && (0.0..=1.).contains(v))
                && self.max_vocal_leakage_db.is_finite()
                && self.max_vocal_leakage_db < 0.
                && self.max_peak_dbfs.is_finite()
                && self.max_peak_dbfs <= -1.
                && self.normal_speed_deviation.is_finite()
                && (0.0..=0.2).contains(&self.normal_speed_deviation)
                && self.expressive_edit_speed_limits[0].is_finite()
                && self.expressive_edit_speed_limits[1].is_finite()
                && (0.125..=1.).contains(&self.expressive_edit_speed_limits[0])
                && (1.0..=8.).contains(&self.expressive_edit_speed_limits[1]),
            "TACTIC_POLICY",
            "invalid or unbounded tactic thresholds",
        )
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TacticRejection {
    pub tactic_id: TacticId,
    pub reasons: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RankedTactic {
    pub plan: TacticPlan,
    pub score: f64,
    #[serde(default)]
    pub last_resort: bool,
    #[serde(default)]
    pub dense_quality_seconds: f64,
    #[serde(default)]
    pub handoff_quality_seconds: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TacticSelection {
    pub candidates: Vec<RankedTactic>,
    pub rejected: Vec<TacticRejection>,
}

fn known(value: Option<f64>, test: impl FnOnce(f64) -> bool) -> bool {
    value.is_some_and(|v| v.is_finite() && test(v))
}
fn role(p: &PassageFacts, roles: &[TacticRole]) -> bool {
    roles
        .iter()
        .any(|r| p.roles.get(r).is_some_and(|v| v.is_finite() && *v >= 0.60))
}
fn stable(p: &PassageFacts) -> bool {
    !p.bars.is_empty() && p.bars.iter().all(|b| known(b.stability, |v| v >= 0.65))
}
pub(crate) fn sustained_tactic(id: TacticId) -> bool {
    matches!(
        id,
        TacticId::M01 | TacticId::M02 | TacticId::S05 | TacticId::S06
    )
}
fn hold_end(id: TacticId, f: &CandidateFacts) -> u64 {
    if let Some(exit) = f.phrase_exit.as_ref().filter(|_| sustained_tactic(id)) {
        return exit.start_frame;
    }
    let bars = f.bar_frames.len() - 1;
    match id {
        TacticId::M01 | TacticId::S05 | TacticId::S06 => f.bar_frames[bars.saturating_sub(2)],
        TacticId::M02 => f.bar_frames[bars.saturating_sub(1)],
        _ => f.bar_frames[0],
    }
}
fn held_bars(id: TacticId, f: &CandidateFacts) -> usize {
    f.bar_frames
        .iter()
        .take_while(|&&at| at < hold_end(id, f))
        .count()
}

fn stable_hold(p: &PassageFacts, bars: usize) -> bool {
    bars > 0
        && p.bars.len() >= bars
        && p.bars[..bars]
            .iter()
            .all(|b| known(b.stability, |v| v >= 0.65))
}
// Shared low-band dominance is a ranking risk, not a substitute for measuring
// collisions in the scheduled band allocation.
pub(crate) fn shared_low_band_dominance(a: f64, b: f64) -> bool {
    a >= 0.70 && b >= 0.70
}

fn band_exchange_order(f: &CandidateFacts) -> Option<([usize; 3], [[f64; 3]; 2])> {
    let mut occupancy = [[0.; 3]; 2];
    for (side, passage) in [&f.a, &f.b].into_iter().enumerate() {
        if passage.bars.is_empty() {
            return None;
        }
        for bar in &passage.bars {
            for (band, value) in [bar.low_occupancy, bar.mid_occupancy, bar.high_occupancy]
                .into_iter()
                .enumerate()
            {
                let value = value.filter(|v| v.is_finite() && (0.0..=1.).contains(v))?;
                occupancy[side][band] += value / passage.bars.len() as f64;
            }
        }
    }
    let mut order = [2, 0, 1];
    // Bring in B where it contributes measured material and A leaves space;
    // preserve A's occupied regions until later. Ties are deterministic.
    order.sort_by(|a, b| {
        (occupancy[1][*b] * (1. - occupancy[0][*b]))
            .total_cmp(&(occupancy[1][*a] * (1. - occupancy[0][*a])))
    });
    Some((order, occupancy))
}
fn grid(p: &PassageFacts) -> bool {
    !p.bars.is_empty()
        && p.bars
            .iter()
            .all(|b| known(b.grid_score, |v| v >= 0.5) && b.beats_per_bar.is_some_and(|n| n >= 2))
}
fn boundary(p: &PassageFacts, frame: u64, entry: bool, policy: &TacticPolicy) -> bool {
    p.phrase_boundaries.iter().any(|b| {
        b.output_frame == frame
            && b.complete
            && b.score_margin >= policy.minimum_boundary_margin
            && (b.kind == BoundaryKind::Both
                || b.kind
                    == if entry {
                        BoundaryKind::Entry
                    } else {
                        BoundaryKind::Exit
                    })
    })
}
fn stems(p: &PassageFacts, policy: &TacticPolicy, lanes: &[StemLane]) -> bool {
    p.stems.as_ref().is_some_and(|s| {
        (p.stem_leakage.is_some()
            || s.max_vocal_leakage_db.is_finite()
                && s.max_vocal_leakage_db <= policy.max_vocal_leakage_db)
            && s.reconstruction_error.is_finite()
            && s.reconstruction_error <= policy.max_reconstruction_error
            && s.artifact_score.is_finite()
            && s.artifact_score <= policy.max_artifact_score
            && lanes.iter().all(|l| s.available.contains(l))
    })
}
fn tonal(f: &CandidateFacts, p: &TacticPolicy) -> bool {
    known(f.pair.harmonic_distance, |v| v <= p.max_harmonic_distance)
        && known(f.pair.lead_conflict, |v| v <= p.max_lead_conflict)
}
fn complementary(f: &CandidateFacts) -> bool {
    known(f.pair.complementarity, |v| v >= 0.65)
}
/// Measured crowding on each side of the filter transfer. Both measurements
/// must be known even when the quieter side need not reach the threshold.
pub fn filter_crowding(facts: &CandidateFacts) -> [Option<f64>; 2] {
    [density(&facts.a), density(&facts.b)]
}

fn density(p: &PassageFacts) -> Option<f64> {
    if p.bars.is_empty() {
        return None;
    }
    let mut score = 0.;
    for b in &p.bars {
        let (Some(mid), Some(onsets), Some(crest), Some(active)) = (
            b.mid_occupancy,
            b.onsets_per_beat,
            b.crest_db,
            b.active_stems,
        ) else {
            return None;
        };
        if ![mid, onsets, crest].iter().all(|v| v.is_finite()) {
            return None;
        }
        score += 0.35 * mid.clamp(0., 1.)
            + 0.25 * (onsets / 2.).clamp(0., 1.)
            + 0.20 * (1. - crest / 18.).clamp(0., 1.)
            + 0.20 * (active as f64 / 4.).clamp(0., 1.);
    }
    Some(score / p.bars.len() as f64)
}
fn loop_candidate<'a>(p: &'a PassageFacts, policy: &TacticPolicy) -> Option<&'a LoopEvidence> {
    loop_candidate_for(p, policy, StemLane::Stereo)
}
fn steady_bar_clock(f: &CandidateFacts) -> bool {
    let Some(first) = f.bar_frames.windows(2).next().map(|w| w[1] - w[0]) else {
        return false;
    };
    f.bar_frames
        .windows(2)
        .all(|w| (w[1] - w[0]).abs_diff(first) <= 1)
        && !f
            .pair
            .shared_bar_bpm
            .as_ref()
            .is_some_and(|v| v.windows(2).any(|w| (w[0] - w[1]).abs() > 0.01))
}
/// C13 currently renders a fixed three-beat cycle. Beat-count divisibility
/// alone is insufficient when mapped bars change duration: the actual output
/// span must contain whole cycles on a steady clock. C14 owns moving loops.
fn polyrhythm_period(f: &CandidateFacts) -> Option<u64> {
    if !steady_bar_clock(f) {
        return None;
    }
    let first = f.a.bars.first()?;
    let meter = u64::from(first.beats_per_bar?);
    if meter == 0 {
        return None;
    }
    let period = ((first.end_frame - first.start_frame) as f64 * 3. / meter as f64).round() as u64;
    let start = *f.bar_frames.first()?;
    let end = *f.bar_frames.last()?;
    if period < 16 || (end - start) % period != 0 {
        return None;
    }
    f.a.bars
        .iter()
        .all(|bar| {
            bar.beats_per_bar.is_some_and(|beats| {
                beats > 0
                    && ((bar.end_frame - bar.start_frame) as f64
                        - period as f64 * f64::from(beats) / 3.)
                        .abs()
                        <= 1.
            })
        })
        .then_some(period)
}
fn loop_candidate_for<'a>(
    p: &'a PassageFacts,
    policy: &TacticPolicy,
    lane: StemLane,
) -> Option<&'a LoopEvidence> {
    p.loop_candidates.iter().find(|l| {
        l.lane == lane
            && l.source_start_frame < l.source_end_frame
            && l.output_start_frame < l.output_end_frame
            && l.seam_error.is_finite()
            && l.seam_error <= 0.05
            && l.tonal_cycle_distance.is_finite()
            && l.tonal_cycle_distance <= 0.20
            && l.confidence_margin >= policy.minimum_boundary_margin
            && (l.vocal_free || l.contains_complete_language == Some(true))
    })
}
fn vocal_free(p: &PassageFacts) -> bool {
    p.vocal_phrases.as_ref().is_some_and(Vec::is_empty)
}
fn all_lanes() -> [StemLane; 4] {
    [
        StemLane::Drums,
        StemLane::Bass,
        StemLane::Other,
        StemLane::Vocals,
    ]
}
fn needs_stems(id: TacticId) -> bool {
    matches!(
        id,
        TacticId::M01
            | TacticId::M02
            | TacticId::M03
            | TacticId::M04
            | TacticId::M05
            | TacticId::M06
            | TacticId::S05
            | TacticId::S06
            | TacticId::C08
            | TacticId::C11
            | TacticId::C13
            | TacticId::C14
    )
}
fn capabilities(id: TacticId) -> BTreeSet<RenderCapability> {
    use RenderCapability as R;
    let mut out = BTreeSet::from([R::Gain]);
    if needs_stems(id) {
        out.insert(R::Stems);
    }
    match id {
        TacticId::S01 | TacticId::S03 | TacticId::S04 => {
            out.insert(R::Eq);
        }
        TacticId::S08 => {
            out.insert(R::Filter);
        }
        TacticId::S09 | TacticId::S10 => {
            out.insert(R::Delay);
        }
        TacticId::S11 => {
            out.insert(R::Reverb);
        }
        TacticId::S12
        | TacticId::C05
        | TacticId::C11
        | TacticId::C12
        | TacticId::C13
        | TacticId::C14
        | TacticId::E01
        | TacticId::E02
        | TacticId::E03 => {
            out.insert(R::SourceEdits);
        }
        TacticId::C06 => {
            out.insert(R::Gate);
        }
        TacticId::C08 => {
            out.insert(R::PitchSampler);
            out.insert(R::SourceEdits);
        }
        TacticId::C09 => {
            out.insert(R::Noise);
        }
        TacticId::C10 => {
            out.insert(R::Spiral);
        }
        TacticId::D01 => {
            out.insert(R::TempoMap);
        }
        TacticId::D02 => {
            out.insert(R::PulseMapping);
        }
        _ => {}
    }
    if id.permission() == TacticPermission::EmergencyOnly {
        out.insert(R::EmergencyRecovery);
    }
    out
}

impl CandidateFacts {
    pub fn validate(&self) -> Result<()> {
        require(
            self.output_sample_rate_hz > 0
                && self.bar_frames.len() >= 2
                && self.bar_frames.windows(2).all(|w| w[0] < w[1])
                && self.bar_frames.last().unwrap() <= &MAX_FRAME,
            "TACTIC_FACTS",
            "invalid candidate output timebase",
        )?;
        require(
            self.a.placement != self.b.placement,
            "TACTIC_FACTS",
            "candidate requires distinct placements",
        )?;
        if let Some(exit) = &self.phrase_exit {
            let end = *self.bar_frames.last().unwrap();
            let last_bar = self.bar_frames[self.bar_frames.len() - 2];
            require(
                self.timing_basis == TacticTimingBasis::Bars
                    && (self.a.phrase_exit.as_ref() == Some(exit)
                        || self.pair.rhythmic_exit.as_ref() == Some(exit))
                    && exit.start_frame >= last_bar + (end - last_bar) / 2
                    && exit.start_frame < exit.end_frame
                    && exit.end_frame <= end
                    && exit.end_frame - exit.start_frame
                        <= u64::from(self.output_sample_rate_hz) * 120 / 1000
                    && !exit.evidence.is_empty(),
                "TACTIC_PHRASE_EXIT",
                "invalid measured swift exit",
            )?;
        }
        for p in [&self.a, &self.b] {
            require(
                p.source.sample_rate_hz > 0
                    && p.source.start_frame < p.source.end_frame
                    && p.source.end_frame <= MAX_FRAME
                    && p.source.output_start_frame < p.source.output_end_frame
                    && !p.evidence.is_empty(),
                "TACTIC_FACTS",
                "missing source timebase or evidence provenance",
            )?;
            let independent = p.placement == self.b.placement
                && (self.timing_basis == TacticTimingBasis::IndependentPulses
                    || self.pair.tempo_maps_validated
                        && self
                            .pair
                            .pulse_ratio
                            .is_some_and(|r| (r - 2.).abs() < 0.02 || (r - 0.5).abs() < 0.01));
            require(
                self.timing_basis == TacticTimingBasis::Structural && p.bars.is_empty()
                    || independent
                        && !p.bars.is_empty()
                        && p.bars.iter().all(|b| b.start_frame < b.end_frame)
                        && p.bars
                            .windows(2)
                            .all(|w| w[0].end_frame == w[1].start_frame)
                    || p.bars.len() + 1 == self.bar_frames.len()
                        && p.bars.iter().enumerate().all(|(i, b)| {
                            b.start_frame == self.bar_frames[i]
                                && b.end_frame == self.bar_frames[i + 1]
                        }),
                "TACTIC_FACTS",
                "bar evidence must cover the complete candidate",
            )?;
            require(
                p.roles
                    .values()
                    .all(|v| v.is_finite() && (0.0..=1.).contains(v)),
                "TACTIC_FACTS",
                "invalid role score",
            )?;
            for bar in &p.bars {
                require(
                    [
                        bar.rms,
                        bar.crest_db,
                        bar.onsets_per_beat,
                        bar.low_occupancy,
                        bar.mid_occupancy,
                        bar.high_occupancy,
                        bar.grid_score,
                        bar.stability,
                    ]
                    .iter()
                    .flatten()
                    .all(|v| v.is_finite() && *v >= 0.)
                        && bar
                            .chroma
                            .as_ref()
                            .is_none_or(|c| c.iter().all(|v| v.is_finite() && *v >= 0.)),
                    "TACTIC_FACTS",
                    "invalid numeric bar evidence",
                )?;
            }
            if let Some(v) = &p.vocal_phrases {
                require(
                    v.windows(2).all(|w| w[0].end_frame <= w[1].start_frame)
                        && v.iter().all(|v| {
                            v.start_frame < v.end_frame
                                && v.end_frame <= v.tail_end_frame
                                && v.confidence_margin.is_finite()
                        }),
                    "TACTIC_FACTS",
                    "invalid vocal evidence intervals",
                )?;
            }
            if let Some(quiet) = &p.vocal_quiet {
                require(
                    quiet.iter().all(|&[a, b]| a < b && b <= MAX_FRAME)
                        && quiet.windows(2).all(|w| w[0][1] <= w[1][0]),
                    "TACTIC_FACTS",
                    "invalid vocal quiet intervals",
                )?;
            }
        }
        Ok(())
    }
}

fn eligibility(id: TacticId, f: &CandidateFacts, p: &TacticPolicy) -> Vec<String> {
    use TacticId::*;
    use TacticRole as R;
    let mut out = vec![];
    let mut need = |ok: bool, why: &str| {
        if !ok {
            out.push(why.into());
        }
    };
    let start = f.bar_frames[0];
    let end = *f.bar_frames.last().unwrap();
    let bars = f.bar_frames.len() - 1;
    let landing = if id == C01 {
        phrase_cut_landing(f).unwrap_or(start)
    } else if id == C08 {
        f.pair
            .shared_motif
            .as_ref()
            .and_then(|n| n.last())
            .map(|n| n.end_frame)
            .unwrap_or(start)
    } else {
        f.b.arrival_frame.unwrap_or(start)
    };
    let cut = matches!(id, C01 | C02 | C03 | C04 | C08);
    need(
        id.permission() != TacticPermission::ExpressiveOptIn || p.allow_expressive,
        "expressive tactic is not enabled",
    );
    need(
        id.permission() != TacticPermission::EmergencyOnly || p.emergency.is_some(),
        "offline incompatibility is not an emergency",
    );
    need(
        capabilities(id).is_subset(&p.capabilities),
        "required renderer capability is unavailable",
    );
    need(
        known(f.pair.predicted_peak_dbfs, |v| v <= p.max_peak_dbfs),
        "composite headroom is unknown or insufficient",
    );
    let pulse_free = matches!(id, S07 | S11 | C04 | C14 | E01 | E02 | E03 | E04);
    if matches!(id, S12 | C05 | C06 | C13) {
        need(steady_bar_clock(f),
            "fixed-period loop/gate schedule requires a steady output bar clock; D01 is unsupported");
    }
    need(
        f.timing_basis == TacticTimingBasis::Bars
            || f.timing_basis == TacticTimingBasis::IndependentPulses && id == C14
            || matches!(id, S07 | S11 | C04 | E01 | E02 | E03 | E04),
        "unmetered structure cannot support this rhythmic tactic",
    );
    if !pulse_free {
        need(
            grid(&f.a)
                && grid(&f.b)
                && known(f.pair.beat_skew_ms, |v| v >= 0. && v <= p.max_beat_skew_ms),
            "rhythmic overlap is not grid-locked",
        );
    }
    if matches!(id, M01 | M02 | M03 | M04 | M05 | M06 | S05) {
        need(
            boundary(&f.a, start, true, p),
            "mashup/drum entry must follow a supported complete outgoing phrase",
        );
    }
    if id.permission() != TacticPermission::EmergencyOnly {
        need(
            f.a.build_release_frame.is_none_or(|release| {
                release <= start
                    || sustained_tactic(id)
                        && f.phrase_exit
                            .as_ref()
                            .is_some_and(|exit| release <= exit.start_frame)
                    || matches!(id, C01 | C02 | C03 | C04 | C07)
                        && landing >= release
                        && release < end
            }),
            "outgoing buildup must reach its measured release before the transfer starts",
        );
        let a_exit = if cut { landing } else { end };
        let b_entry = if cut { landing } else { start };
        let local_cut = id == C01
            && f.bar_frames.contains(&landing)
            && crate::vocal_audibility::quiet_landing(f, landing);
        need(
            id == S13
                || local_cut
                || boundary(&f.a, a_exit, false, p) && boundary(&f.b, b_entry, true, p),
            "supported complete entry/exit phrase boundaries are missing",
        );
    }
    if needs_stems(id) {
        need(
            stems(&f.a, p, &all_lanes()) && stems(&f.b, p, &all_lanes()),
            "required stem isolation or leakage evidence failed",
        );
    }
    if matches!(
        id,
        M01 | M02
            | M03
            | M04
            | M05
            | M06
            | S01
            | S03
            | S04
            | S05
            | S06
            | S07
            | S08
            | S10
            | S11
            | S12
            | S13
            | C07
            | C11
    ) {
        need(
            tonal(f, p),
            "selected pitched layers have unknown or incompatible harmony/lead occupancy",
        );
    }
    if f.phrase_exit
        .as_ref()
        .is_some_and(|e| e.kind == PhraseExitKind::RhythmicHandoff)
    {
        need(
            id == M01
                && known(f.pair.complementarity, |v| v >= 0.65)
                && known(f.pair.harmonic_distance, |v| v <= 0.30)
                && known(f.pair.lead_conflict, |v| v <= 0.20),
            "rhythmic exit needs a high-quality dense combination through its extended hold",
        );
    }
    match id {
        M01 => {
            need(
                !f.a.endpoint_reasons.is_empty() || !f.b.endpoint_reasons.is_empty(),
                "dense mashup needs a measured musical endpoint; a seed duration is not an ending",
            );
            need(
                bars >= 8,
                "dense mashup needs at least six hold bars and a two-bar resolution",
            );
            need(
                role(&f.a, &[R::SustainedTexture, R::VocalForeground, R::Mixed])
                    && role(&f.b, &[R::SteadyGroove]),
                "dense mashup needs a retained bed and incoming groove",
            );
            need(
                stable_hold(&f.a, held_bars(id, f))
                    && stable_hold(&f.b, held_bars(id, f))
                    && complementary(f),
                "mashup layers do not remain complementary through the full window",
            );
            need(
                f.pair.selected_composite_density.as_ref().is_some_and(|v| {
                    v.len() == bars
                        && v.len() >= 6
                        && v.iter().all(|x| x.is_finite() && (0.0..=1.).contains(x))
                        && v[..held_bars(id, f)].iter().all(|x| *x >= 0.62)
                }),
                "selected-layer density is unknown or not high through the hold",
            );
        }
        M02 => {
            need(
                bars >= 5,
                "brisk mashup needs at least four hold bars plus its exit",
            );
            need(
                role(&f.a, &[R::SustainedTexture, R::LowPercussionBed])
                    && role(&f.b, &[R::SteadyGroove]),
                "spacious mashup requires a texture and stable incoming drums",
            );
            need(
                stable_hold(&f.a, held_bars(id, f))
                    && stable_hold(&f.b, held_bars(id, f))
                    && complementary(f),
                "mashup does not remain stable/complementary",
            );
            need(
                f.pair.retained_bed_density.as_ref().is_some_and(|v| {
                    v.len() == bars && v.iter().all(|x| x.is_finite() && *x >= 0. && *x < 0.62)
                }),
                "sparse selected-layer evidence is missing",
            );
            need(
                f.b.arrival_frame
                    .is_some_and(|v| v >= f.bar_frames[bars - 1] && v <= end),
                "brisk resolution needs a measured incoming arrival",
            );
        }
        M03 => {
            need(
                f.a.vocal_phrases.as_ref().is_some_and(|v| !v.is_empty()) && complementary(f),
                "acapella requires a complete vocal and complementary backing",
            );
            need(
                bars >= 4,
                "vocal carryover needs a complete four-bar minimum window",
            );
        }
        M04 => {
            need(
                f.a.motif.as_ref().is_some_and(|m| {
                    matches!(m.lane, StemLane::Other | StemLane::Bass)
                        && m.isolation_score >= 0.8
                        && m.phrase_end_frame <= end
                }) && complementary(f),
                "overlay requires an isolated complete motif and space in B",
            );
        }
        M05 => {
            need(
                role(&f.a, &[R::Drop, R::Chorus])
                    && role(&f.b, &[R::Drop, R::Chorus])
                    && complementary(f),
                "double drop requires complementary measured arrivals",
            );
            need(
                f.a.arrival_frame == Some(start) && f.b.arrival_frame == Some(start),
                "drop arrivals must coincide at entry",
            );
        }
        M06 => {
            need(f.pair.semantic_relation.as_ref().is_some_and(|s|s.confidence_margin>=0.25 && !s.evidence.is_empty() && !s.relation.is_empty()) && f.a.vocal_phrases.as_ref().is_some_and(|v|!v.is_empty()) && f.b.vocal_phrases.as_ref().is_some_and(|v|!v.is_empty()),"call-and-response needs complete answering phrases and verified semantic/musical relation");
        }
        S01 => {
            need(
                bars >= 4
                    && role(&f.a, &[R::Outro, R::SustainedTexture])
                    && role(&f.b, &[R::Intro, R::LowPercussionBed])
                    && stable(&f.a)
                    && stable(&f.b),
                "long blend requires sustained sparse intro/outro material",
            );
        }
        S02 => {
            need(bars <= 2, "short crossfade is limited to one or two bars");
        }
        S03 => {
            need(
                f.a.bars
                    .iter()
                    .all(|b| known(b.low_occupancy, |v| v >= 0.4))
                    && f.b
                        .bars
                        .iter()
                        .all(|b| known(b.low_occupancy, |v| v >= 0.4)),
                "bass swap requires known active low-end on both sides",
            );
        }
        S04 => {
            let exchange = band_exchange_order(f);
            need(
                bars >= 3 && exchange.is_some(),
                "three-band exchange needs three measured bars and complete low/mid/high occupancy",
            );
            need(
                complementary(f),
                "band exchange requires compatible remaining upper layers",
            );
        }
        S05 => {
            need(
                bars >= 4
                    && role(&f.a, &[R::LowPercussionBed, R::Breakdown])
                    && role(&f.b, &[R::SteadyGroove])
                    && stable_hold(&f.b, held_bars(id, f)),
                "drum linger requires a quiet outgoing bed and stable incoming groove",
            );
        }
        S06 => {
            need(
                role(&f.a, &[R::DrumReturn])
                    && f.a.arrival_frame == Some(start)
                    && boundary(&f.a, start, true, p),
                "drum replacement must coincide with A's supported return",
            );
            need(
                bars >= 4 && role(&f.b, &[R::SteadyGroove]) && stable_hold(&f.b, held_bars(id, f)),
                "drum replacement needs a stable B groove",
            );
        }
        S07 => {
            need(
                role(&f.a, &[R::Breakdown, R::Outro, R::SustainedTexture])
                    && role(&f.b, &[R::Intro, R::Breakdown])
                    && (role(&f.a, &[R::LowPercussionBed, R::Rest])
                        || role(&f.b, &[R::Rest, R::LowPercussionBed])),
                "ambient bridge requires a verified pulse-free interval",
            );
        }
        S08 => {
            let [a, b] = filter_crowding(f);
            let crowded = a.zip(b).is_some_and(|(a, b)| {
                if p.allow_one_sided_filter {
                    a.max(b) >= 0.5
                } else {
                    a.min(b) >= 0.5
                }
            });
            need(
                crowded,
                "filter transfer requires measured crowded frequency occupancy",
            );
        }
        S09 => {
            need(
                bars >= 2 && known(f.pair.lead_conflict, |v| v <= p.max_lead_conflict),
                "echo requires a clean ending and room for its tail",
            );
        }
        S10 => {
            need(
                loop_candidate(&f.a, p).is_some_and(|l| l.vocal_free) && complementary(f),
                "freeze requires a safe nonvocal capture and space in B",
            );
        }
        S11 => {
            need(
                role(&f.a, &[R::SustainedTexture, R::LowPercussionBed])
                    && role(&f.b, &[R::Intro, R::Breakdown, R::Rest]),
                "reverb bridge requires an isolated texture and spacious B entry",
            );
        }
        S12 => {
            need(
                loop_candidate(&f.a, p).is_some() || loop_candidate(&f.b, p).is_some(),
                "no verified repeatable intro/outro span",
            );
        }
        S13 => {
            need(
                (4..=8).contains(&bars),
                "baseline gradual crossfade requires four to eight measured bars",
            );
        }
        D01 => {
            need(
                f.pair.tempo_maps_validated
                    && f.pair.shared_bar_bpm.as_ref().is_some_and(|v| {
                        v.len() == f.bar_frames.len()
                            && v.iter().all(|x| x.is_finite() && *x > 0.)
                            && v.windows(2).any(|w| (w[0] - w[1]).abs() > 0.01)
                    }),
                "tempo modifier lacks validated changing per-bar maps",
            );
        }
        D02 => {
            need(
                known(f.pair.pulse_ratio, |r| {
                    (r - 2.).abs() < 0.02 || (r - 0.5).abs() < 0.01
                }) && known(f.pair.groove_similarity, |v| v >= 0.8)
                    && f.pair.tempo_maps_validated,
                "half/double-time requires verified pulse hierarchy and mapped groove",
            );
        }
        C01 => {
            need(
                phrase_cut_landing(f).is_some_and(|v| v >= start && v <= end),
                "phrase cut needs a supported incoming attack",
            );
        }
        C02 => {
            need(
                role(&f.a, &[R::Build, R::Drop])
                    && role(&f.b, &[R::Drop])
                    && f.b.arrival_frame.is_some(),
                "drop replacement requires a measured drop landing",
            );
        }
        C03 => {
            need(
                role(&f.a, &[R::Build])
                    && role(&f.b, &[R::Drop])
                    && f.a.build_release_frame.is_some()
                    && f.b.arrival_frame.is_some()
                    && tonal(f, p),
                "build-to-drop requires compatible buildup and measured arrival",
            );
        }
        C04 => {
            need(
                f.a.natural_end_frame == Some(landing)
                    && role(&f.b, &[R::Intro])
                    && f.b.arrival_frame.is_some(),
                "end-to-end requires a measured natural ending and intentional start",
            );
        }
        C05 => {
            need(
                loop_candidate(&f.a, p).is_some_and(|l| l.vocal_free)
                    && f.b.arrival_frame.is_some_and(|v| v > start && v <= end),
                "roll requires a clean rhythmic capture and later landing",
            );
        }
        C06 => {
            need(
                complementary(f) && vocal_free(&f.a) && vocal_free(&f.b),
                "transform cuts require complementary vocal-free rhythmic gaps",
            );
        }
        C07 => {
            need(
                f.a.version_identity.is_some()
                    && f.a.version_identity == f.b.version_identity
                    && f.pair.shared_motif.is_some(),
                "version switch requires verified related recordings and corresponding motif",
            );
        }
        C08 => {
            need(
                f.a.motif.as_ref().is_some_and(|m| {
                    m.isolation_score >= 0.9
                        && m.transcription_margin >= 0.35
                        && m.sample.as_ref().is_some_and(|s| {
                            s.source_start_frame < s.source_end_frame
                                && s.single_fundamental_score >= 0.90
                        })
                }) && f.pair.shared_motif.as_ref().is_some_and(|n| !n.is_empty()),
                "pitch play requires isolated sample and verified note transcription",
            );
        }
        C09 => {
            need(
                role(&f.a, &[R::Build, R::Rest])
                    && f.b.arrival_frame.is_some_and(|v| v > start && v <= end),
                "riser requires a supported build/gap and measured landing",
            );
        }
        C10 => {
            need(
                loop_candidate(&f.a, p).is_some_and(|l| l.vocal_free)
                    && known(f.pair.lead_conflict, |v| v <= p.max_lead_conflict)
                    && f.b.arrival_frame.is_some_and(|v| v > start && v <= end),
                "spiral requires clean capture, tail room and landing",
            );
        }
        C11 => {
            need(
                f.b.motif.as_ref().is_some_and(|m| m.isolation_score >= 0.8)
                    && complementary(f)
                    && f.b.arrival_frame.is_some_and(|v| v > start && v <= end),
                "tease requires verified recognizable incoming motif, space and later entry",
            );
        }
        C12 => {
            need(
                loop_candidate(&f.a, p).is_some_and(|l| l.vocal_free)
                    && f.b.arrival_frame.is_some_and(|v| v > start && v <= end),
                "reverse flourish requires dispensable nonvocal capture and exact return",
            );
        }
        C13 => {
            need(
                loop_candidate_for(&f.b, p, StemLane::Drums).is_some_and(|l| l.vocal_free)
                    && role(&f.a, &[R::SteadyGroove])
                    && stable(&f.a)
                    && complementary(f),
                "polyrhythm needs clean incoming percussion and uncluttered steady A",
            );
            need(
                f.a.bars
                    .iter()
                    .map(|b| b.beats_per_bar.unwrap_or(0) as usize)
                    .sum::<usize>()
                    % 3
                    == 0,
                "polyrhythm window must end at its common pulse boundary",
            );
            need(polyrhythm_period(f).is_some(),
                "fixed polyrhythm requires steady mapped pulses and whole output cycles at its landing");
        }
        C14 => {
            need(grid(&f.a) && grid(&f.b) && loop_candidate_for(&f.a,p,StemLane::Drums).is_some_and(|l|l.vocal_free) && known(f.pair.target_tempo_ratio,|r|(0.25..=4.).contains(&r) && (r-1.).abs()>0.06) && f.b.arrival_frame.is_some_and(|v|v>start && v<=end),"percussion tempo bridge requires reliable separate pulses, bounded loop move and prepared arrival");
        }
        E01 | E02 | E03 | E04 => {
            need(
                p.emergency.as_ref().is_some_and(|e| {
                    e.next_source_verified
                        && !e.failed_condition.is_empty()
                        && !e.rejected_safe_recoveries.is_empty()
                }),
                "recovery evidence or verified next source is missing",
            );
            if id != E04 {
                need(
                    f.a.ready_buffer_frames > 0
                        && p.emergency
                            .as_ref()
                            .is_some_and(|e| e.healthy_buffer_frames > 0),
                    "effect recovery requires healthy captured audio",
                );
            }
        }
    }
    out
}

fn curve(start: u64, end: u64, points: &[(u64, f64)]) -> Vec<ScalarPoint> {
    let mut ordered = BTreeMap::new();
    for &(frame, value) in points {
        ordered.insert(frame.clamp(start, end), value);
    }
    let first = *ordered.values().next().unwrap_or(&0.);
    let last = *ordered.values().last().unwrap_or(&0.);
    ordered.entry(start).or_insert(first);
    ordered.entry(end).or_insert(last);
    ordered
        .into_iter()
        .map(|(output_frame, value)| ScalarPoint {
            output_frame,
            value,
        })
        .collect()
}
pub(crate) fn phrase_cut_landing(f: &CandidateFacts) -> Option<u64> {
    f.b.cut_attacks
        .iter()
        .copied()
        .find(|at| f.bar_frames.contains(at) && crate::vocal_audibility::quiet_landing(f, *at))
        .or(f.b.arrival_frame)
}

pub(crate) fn transfer(
    start: u64,
    end: u64,
    from: u64,
    to: u64,
    incoming: bool,
) -> Vec<ScalarPoint> {
    let (a, b) = if incoming { (0., 1.) } else { (1., 0.) };
    curve(start, end, &[(start, a), (from, a), (to, b), (end, b)])
}
pub(crate) fn constant(start: u64, end: u64, value: f64) -> Vec<ScalarPoint> {
    curve(start, end, &[(start, value), (end, value)])
}
fn lane(placement: usize, lane: StemLane, gain: Vec<ScalarPoint>) -> LaneSchedule {
    LaneSchedule {
        placement,
        lane,
        gain,
        eq: vec![],
    }
}
fn safe_add(a: u64, b: u64) -> u64 {
    a.saturating_add(b)
}
pub(crate) fn curve_value(points: &[ScalarPoint], frame: u64) -> f64 {
    if frame <= points[0].output_frame {
        return points[0].value;
    }
    for w in points.windows(2) {
        if frame <= w[1].output_frame {
            let x =
                (frame - w[0].output_frame) as f64 / (w[1].output_frame - w[0].output_frame) as f64;
            return w[0].value + (w[1].value - w[0].value) * x;
        }
    }
    points.last().unwrap().value
}
fn apply_vocal_contract(plan: &mut TacticPlan, f: &CandidateFacts, p: &TacticPolicy) -> Result<()> {
    let start = plan.start_frame;
    let end = plan.end_frame;
    let guard = plan.vocal_guard_frames;
    let stem_mode = plan.lanes.iter().any(|l| l.lane == StemLane::Vocals);
    let av =
        f.a.vocal_phrases
            .as_ref()
            .ok_or_else(|| Error::new("TACTIC_VOCALS", "A vocal evidence unknown"))?;
    let bv =
        f.b.vocal_phrases
            .as_ref()
            .ok_or_else(|| Error::new("TACTIC_VOCALS", "B vocal evidence unknown"))?;
    let valid = |v: &VocalPhrase| {
        v.entry_supported && v.exit_supported && v.confidence_margin >= p.minimum_boundary_margin
    };
    if stem_mode
        && !matches!(
            plan.tactic_id,
            TacticId::M04
                | TacticId::M06
                | TacticId::C08
                | TacticId::C11
                | TacticId::C13
                | TacticId::C14
        )
    {
        let minimum = if plan.phrase_exit.is_some()
            || matches!(plan.tactic_id, TacticId::M01 | TacticId::M02)
        {
            plan.hold_end_frame
        } else {
            plan.bar_frames[(plan.bar_frames.len() - 1).min(4)]
        };
        let carried: Vec<_> = av
            .iter()
            .filter(|v| v.end_frame > start && v.start_frame < minimum)
            .collect();
        require(
            carried.iter().all(|v| valid(v) && v.end_frame <= end),
            "TACTIC_VOCALS",
            "outgoing phrase cannot finish intact in this window",
        )?;
        let off = carried
            .iter()
            .map(|v| v.end_frame)
            .max()
            .unwrap_or(start)
            .max(minimum);
        let tail = carried
            .iter()
            .map(|v| v.tail_end_frame)
            .max()
            .unwrap_or(start);
        require(
            off <= end
                && tail <= end
                && plan.phrase_exit.as_ref().is_none_or(|e| {
                    off + u64::from(f.output_sample_rate_hz) / 200 <= e.end_frame
                        && tail <= e.end_frame
                }),
            "TACTIC_VOCALS",
            "outgoing phrase/tail exceeds transition",
        )?;
        let earliest = if carried.is_empty() {
            start
        } else {
            safe_add(tail, guard)
        };
        let next = bv
            .iter()
            .find(|v| v.start_frame >= earliest && v.start_frame >= start && valid(v));
        let on = next.map(|v| v.start_frame).unwrap_or(end);
        require(
            on <= end
                && !bv
                    .iter()
                    .any(|v| v.start_frame < end && v.end_frame > end && v.start_frame < on),
            "TACTIC_VOCALS",
            "B would enter mid-phrase at transition completion",
        )?;
        // Gate changes occur in measured silence. They are independent of accompaniment fades.
        let ramp = (f.output_sample_rate_hz as u64 / 200).max(1);
        for l in &mut plan.lanes {
            if l.lane != StemLane::Vocals {
                continue;
            }
            l.gain = if l.placement == f.a.placement {
                if off >= end {
                    constant(start, end, 1.)
                } else {
                    transfer(start, end, off, (off + ramp).min(end), false)
                }
            } else if on <= start {
                constant(start, end, 1.)
            } else {
                transfer(start, end, on.saturating_sub(ramp).max(start), on, true)
            };
        }
        for v in carried {
            plan.vocal_occupancy.push(VocalOccupancy {
                placement: f.a.placement,
                start_frame: v.start_frame,
                end_frame: v.end_frame,
                tail_end_frame: v.tail_end_frame,
                supported_entry: true,
                supported_exit: true,
            });
        }
        for v in bv
            .iter()
            .filter(|v| v.start_frame >= on && v.start_frame < end)
        {
            require(
                valid(v),
                "TACTIC_VOCALS",
                "B entry lacks complete phrase/pickup evidence",
            )?;
            plan.vocal_occupancy.push(VocalOccupancy {
                placement: f.b.placement,
                start_frame: v.start_frame,
                end_frame: v.end_frame,
                tail_end_frame: v.tail_end_frame,
                supported_entry: true,
                supported_exit: true,
            });
        }
    } else {
        for (passage, phrases) in [(&f.a, av), (&f.b, bv)] {
            let lane = plan.lanes.iter().find(|l| {
                l.placement == passage.placement
                    && l.lane
                        == if stem_mode {
                            StemLane::Vocals
                        } else {
                            StemLane::Stereo
                        }
            });
            let Some(lane) = lane else {
                continue;
            };
            for v in phrases {
                if v.end_frame <= start || v.start_frame >= end {
                    continue;
                }
                let lo = v.start_frame.max(start);
                let hi = v.end_frame.min(end);
                let effect = plan
                    .effects
                    .iter()
                    .filter(|fx| {
                        fx.placement == passage.placement
                            && (fx.lane == StemLane::Stereo || fx.lane == StemLane::Vocals)
                    })
                    .find(|fx| v.start_frame < fx.input_end_frame && v.end_frame > fx.start_frame);
                let mut checks = vec![lo, hi.saturating_sub(1)];
                checks.extend(
                    lane.gain
                        .iter()
                        .filter(|p| p.output_frame >= lo && p.output_frame < hi)
                        .map(|p| p.output_frame),
                );
                let heard = checks.iter().any(|&t| {
                    curve_value(&lane.gain, t) > 1e-4
                        && effect.is_none_or(|fx| t < fx.input_end_frame)
                });
                if !heard {
                    continue;
                }
                require(
                    valid(v) && (passage.placement == f.a.placement || v.start_frame >= start),
                    "TACTIC_VOCALS",
                    "audible vocal starts in an unsupported phrase/pickup",
                )?;
                require(
                    v.end_frame <= end
                        || passage.placement == f.b.placement
                            && curve_value(&lane.gain, end) > 0.999,
                    "TACTIC_VOCALS",
                    "outgoing phrase would be truncated",
                )?;
                let cutoff = effect.map(|fx| fx.input_end_frame).unwrap_or(end);
                require(
                    v.end_frame <= cutoff || passage.placement == f.b.placement && cutoff == end,
                    "TACTIC_VOCALS",
                    "effect capture cuts an unfinished vocal phrase",
                )?;
                // A newly audible B line may not begin while its gate is still fully closed.
                require(
                    curve_value(&lane.gain, lo) > 1e-4
                        || v.start_frame == start && curve_value(&lane.gain, lo + 1) > 0.,
                    "TACTIC_VOCALS",
                    "vocal would enter after the start of its phrase",
                )?;
                require(
                    checks.iter().all(|&t| curve_value(&lane.gain, t) > 1e-12),
                    "TACTIC_VOCALS",
                    "vocal gate closes before a complete phrase ending",
                )?;
                let tail = effect
                    .map(|fx| fx.end_frame.max(v.tail_end_frame))
                    .unwrap_or(v.tail_end_frame);
                plan.vocal_occupancy.push(VocalOccupancy {
                    placement: passage.placement,
                    start_frame: v.start_frame,
                    end_frame: v.end_frame,
                    tail_end_frame: tail,
                    supported_entry: true,
                    supported_exit: true,
                });
            }
        }
    }
    for a in &plan.vocal_occupancy {
        for b in &plan.vocal_occupancy {
            if a.placement != b.placement && a.start_frame <= b.start_frame {
                require(
                    safe_add(a.tail_end_frame, guard) <= b.start_frame,
                    "TACTIC_VOCALS",
                    "audible vocals or effect tails collide",
                )?;
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)] // All source/output bounds remain explicit at schedule sites.
fn effect(
    plan: &mut TacticPlan,
    placement: usize,
    lane: StemLane,
    start: u64,
    input_end: u64,
    end: u64,
    kind: EffectKind,
    wet: Vec<ScalarPoint>,
) {
    plan.effects.push(EffectSpec {
        placement,
        lane,
        start_frame: start,
        input_end_frame: input_end,
        end_frame: end,
        wet,
        kind,
    });
}
#[allow(clippy::too_many_arguments)] // Explicit timebases prevent accidental source/output mixing.
fn edit(
    p: &PassageFacts,
    lane: StemLane,
    source_start: u64,
    source_end: u64,
    start: u64,
    end: u64,
    rate: u32,
    kind: SourceEditKind,
) -> SourceEditSpec {
    SourceEditSpec {
        placement: p.placement,
        lane,
        source_sample_rate_hz: p.source.sample_rate_hz,
        source_start_frame: source_start,
        source_end_frame: source_end,
        output_start_frame: start,
        output_end_frame: end,
        declick_frames: (rate as u64 / 500)
            .max(1)
            .min(end.saturating_sub(start) / 2),
        transport: SourceTransport::ContinueUnderlying,
        kind,
    }
}

/// Repeated/sampled language occupies its rendered times, not its old source timeline.
fn remap_edited_vocals(plan: &TacticPlan, f: &CandidateFacts) -> Result<CandidateFacts> {
    let mut mapped = f.clone();
    for passage in [&mut mapped.a, &mut mapped.b] {
        let original = passage
            .vocal_phrases
            .clone()
            .ok_or_else(|| Error::new("TACTIC_VOCALS", "source edit has unknown vocal content"))?;
        let mut audible = original.clone();
        for edit in plan.source_edits.iter().filter(|e| {
            e.placement == passage.placement
                && matches!(e.lane, StemLane::Stereo | StemLane::Vocals)
        }) {
            let capture = passage.loop_candidates.iter().find(|l| {
                l.lane == edit.lane
                    && l.source_start_frame == edit.source_start_frame
                    && l.source_end_frame == edit.source_end_frame
            });
            let captured = if let Some(c) = capture {
                original
                    .iter()
                    .filter(|v| {
                        v.end_frame > c.output_start_frame && v.start_frame < c.output_end_frame
                    })
                    .collect::<Vec<_>>()
            } else {
                vec![]
            };
            if let Some(c) = capture {
                require(
                    !c.vocal_free || captured.is_empty(),
                    "TACTIC_VOCALS",
                    "loop vocal-free claim contradicts measured phrase occupancy",
                )?;
            }
            let lane = plan
                .lanes
                .iter()
                .find(|l| l.placement == passage.placement && l.lane == edit.lane)
                .unwrap();
            require(
                !original.iter().any(|v| {
                    v.start_frame < edit.output_start_frame
                        && v.end_frame > edit.output_start_frame
                        && curve_value(&lane.gain, edit.output_start_frame) > 0.
                }),
                "TACTIC_VOCALS",
                "source edit interrupts an already audible vocal phrase",
            )?;
            require(
                !original.iter().any(|v| {
                    v.start_frame < edit.output_end_frame
                        && v.end_frame > edit.output_end_frame
                        && curve_value(&lane.gain, edit.output_end_frame) > 0.
                }),
                "TACTIC_VOCALS",
                "source edit resumes underlying audio mid-phrase",
            )?;
            audible.retain(|v| {
                v.end_frame <= edit.output_start_frame || v.start_frame >= edit.output_end_frame
            });
            if captured.is_empty() {
                require(
                    capture.is_some_and(|c| c.vocal_free) || original.is_empty(),
                    "TACTIC_VOCALS",
                    "capture lacks vocal absence or complete language evidence",
                )?;
                continue;
            }
            let capture = capture.unwrap();
            require(
                capture.contains_complete_language == Some(true)
                    && captured.iter().all(|v| {
                        v.entry_supported
                            && v.exit_supported
                            && v.start_frame >= capture.output_start_frame
                            && v.tail_end_frame <= capture.output_end_frame
                    }),
                "TACTIC_VOCALS",
                "sample capture truncates language or its tail",
            )?;
            let width = (capture.output_end_frame - capture.output_start_frame) as f64;
            let mut add_phrase = |v: &VocalPhrase, at: u64, scale: f64, limit: u64| -> Result<()> {
                let mut out = v.clone();
                out.start_frame = at
                    + ((v.start_frame - capture.output_start_frame) as f64 * scale).round() as u64;
                out.end_frame =
                    at + ((v.end_frame - capture.output_start_frame) as f64 * scale).round() as u64;
                out.tail_end_frame = at
                    + ((v.tail_end_frame - capture.output_start_frame) as f64 * scale).round()
                        as u64;
                require(
                    out.start_frame < out.end_frame && out.tail_end_frame <= limit,
                    "TACTIC_VOCALS",
                    "last sample repetition truncates a phrase or tail",
                )?;
                audible.push(out);
                Ok(())
            };
            match &edit.kind {
                SourceEditKind::Loop { period_frames } => {
                    require(
                        (edit.output_end_frame - edit.output_start_frame)
                            .div_ceil(*period_frames)
                            .saturating_mul(captured.len() as u64)
                            <= 4096,
                        "TACTIC_VOCALS",
                        "too many repeated vocal events",
                    )?;
                    let mut at = edit.output_start_frame;
                    while at < edit.output_end_frame {
                        for v in &captured {
                            add_phrase(
                                v,
                                at,
                                *period_frames as f64 / width,
                                edit.output_end_frame,
                            )?;
                        }
                        at = at.saturating_add(*period_frames);
                    }
                }
                SourceEditKind::PitchSequence { notes } => {
                    for note in notes {
                        for v in &captured {
                            add_phrase(
                                v,
                                note.start_frame,
                                1. / 2f64.powf(note.semitones / 12.),
                                note.end_frame,
                            )?;
                        }
                    }
                }
                _ => {
                    return Err(Error::new(
                        "TACTIC_VOCALS",
                        "vocal capture is not admissible for this motion/edit",
                    ))
                }
            }
        }
        audible.sort_by_key(|v| v.start_frame);
        require(
            audible
                .windows(2)
                .all(|w| w[0].end_frame <= w[1].start_frame),
            "TACTIC_VOCALS",
            "sampled vocal phrases overlap on one lane",
        )?;
        passage.vocal_phrases = Some(audible);
    }
    Ok(mapped)
}

/// Schedule one admitted cue/tactic combination. No decoding or hidden cue search occurs here.
pub fn schedule_tactic(id: TacticId, f: &CandidateFacts, p: &TacticPolicy) -> Result<TacticPlan> {
    p.validate()?;
    let specialized = specialized_facts(id, f);
    let f = &specialized;
    f.validate()?;
    require(
        !id.is_modifier(),
        "TACTIC_MODIFIER",
        "tempo modifiers must attach to an arrangement",
    )?;
    let rejected = eligibility(id, f, p);
    require(
        rejected.is_empty(),
        "TACTIC_INELIGIBLE",
        rejected.join("; "),
    )?;
    candidate_tactic_schedule(id, f, p)
}

/// A processing shape for measuring a candidate's actual selected lanes. This
/// does not admit a tactic or substitute facts; only schedule_tactic may return
/// an eligible decision to callers outside the planner crate.
pub(crate) fn candidate_tactic_schedule(
    id: TacticId,
    f: &CandidateFacts,
    p: &TacticPolicy,
) -> Result<TacticPlan> {
    f.validate()?;
    p.validate()?;
    require(
        !id.is_modifier(),
        "TACTIC_MODIFIER",
        "tempo modifiers need an arrangement",
    )?;
    if let Some(exit) = &f.phrase_exit {
        let end = *f.bar_frames.last().unwrap();
        let last_bar = f.bar_frames[f.bar_frames.len() - 2];
        require(
            sustained_tactic(id)
                && f.timing_basis == TacticTimingBasis::Bars
                && (f.a.phrase_exit.as_ref() == Some(exit)
                    || id == TacticId::M01 && f.pair.rhythmic_exit.as_ref() == Some(exit))
                && exit.start_frame >= last_bar + (end - last_bar) / 2
                && exit.start_frame < exit.end_frame
                && exit.end_frame <= end
                && exit.end_frame - exit.start_frame
                    <= u64::from(f.output_sample_rate_hz) * 120 / 1000
                && f.a
                    .build_release_frame
                    .is_none_or(|at| at <= exit.start_frame)
                && !exit.evidence.is_empty(),
            "TACTIC_PHRASE_EXIT",
            "swift exit lacks measured pause/release geometry or would truncate a build",
        )?;
    }
    let count = f.bar_frames.len() - 1;
    let motif_known = |passage: &PassageFacts| {
        passage.motif.as_ref().is_some_and(|m| {
            m.source_start_frame < m.source_end_frame
                && m.source_start_frame >= passage.source.start_frame
                && m.source_end_frame <= passage.source.end_frame
        })
    };
    let shape_known = match id {
        TacticId::M01 => count >= 8,
        TacticId::S13 => (4..=8).contains(&count),
        TacticId::M06 => f.a.vocal_phrases.is_some() && f.b.vocal_phrases.is_some(),
        TacticId::S04 => count >= 3 && band_exchange_order(f).is_some(),
        TacticId::M04 => motif_known(&f.a),
        TacticId::S12 => {
            steady_bar_clock(f)
                && (loop_candidate(&f.a, p).is_some() || loop_candidate(&f.b, p).is_some())
        }
        TacticId::C05 => steady_bar_clock(f) && loop_candidate(&f.a, p).is_some(),
        TacticId::C06 => steady_bar_clock(f),
        TacticId::C12 => loop_candidate(&f.a, p).is_some(),
        TacticId::C08 => {
            motif_known(&f.a)
                && f.a.motif.as_ref().is_some_and(|m| {
                    m.sample.as_ref().is_some_and(|s| {
                        s.source_start_frame < s.source_end_frame
                            && s.source_start_frame >= m.source_start_frame
                            && s.source_end_frame <= m.source_end_frame
                    })
                })
                && f.pair.shared_motif.as_ref().is_some_and(|n| !n.is_empty())
        }
        TacticId::C11 => motif_known(&f.b),
        TacticId::C13 => {
            loop_candidate_for(&f.b, p, StemLane::Drums).is_some() && polyrhythm_period(f).is_some()
        }
        TacticId::C14 => {
            loop_candidate_for(&f.a, p, StemLane::Drums).is_some()
                && known(f.pair.target_tempo_ratio, |r| r > 0.)
        }
        TacticId::E01 | TacticId::E02 | TacticId::E03 => {
            p.emergency.is_some() && f.a.ready_buffer_frames >= 2
        }
        TacticId::E04 => p.emergency.is_some(),
        _ => true,
    };
    require(
        shape_known,
        "TACTIC_SHAPE",
        "required capture, motif or window geometry is unknown",
    )?;
    use TacticId::*;
    let start = f.bar_frames[0];
    let end = *f.bar_frames.last().unwrap();
    let bars = f.bar_frames.len() - 1;
    let bar = f.bar_frames[1] - start;
    let meter = f.a.bars.first().and_then(|b| b.beats_per_bar).unwrap_or(4);
    require(
        meter > 0 && bar >= u64::from(meter),
        "TACTIC_SHAPE",
        "candidate meter has no representable beat duration",
    )?;
    let beat = bar / u64::from(meter);
    let ramp = (f.output_sample_rate_hz as u64 / 200)
        .max(1)
        .min((end - start) / 8);
    let a = f.a.placement;
    let b = f.b.placement;
    let arrival = if id == C01 {
        phrase_cut_landing(f)
    } else {
        f.b.arrival_frame
    };
    let landing = arrival.unwrap_or(end).clamp(start, end);
    let exit = hold_end(id, f);
    let phrase_exit = f.phrase_exit.clone().filter(|_| sustained_tactic(id));
    let resolve_end = phrase_exit.as_ref().map_or(end, |e| e.end_frame);
    let arrangement = match id {
        M01 | M02 | M03 | M04 | M05 | M06 => TacticArrangement::Mashup,
        S03 | S04 | S05 | S06 => TacticArrangement::Handoff,
        S07 | S09 | S10 | S11 | S12 | C05 | C10 | C14 => TacticArrangement::Bridge,
        C01 | C02 | C03 | C04 | C07 => TacticArrangement::Replacement,
        E01 | E02 | E03 | E04 => TacticArrangement::Reset,
        _ => TacticArrangement::Blend,
    };
    let character = match id {
        E01 | E02 | E03 | E04 => TacticCharacter::Abrupt,
        M05 | M06 | C01 | C02 | C03 | C04 | C05 | C06 | C07 | C08 | C09 | C10 | C11 | C12 | C13
        | C14 => TacticCharacter::Sudden,
        _ => TacticCharacter::Subtle,
    };
    let mut plan=TacticPlan{policy_version:TACTICS_POLICY_VERSION.into(),tactic_id:id,modifiers:vec![],arrangement,character,permission:id.permission(),outgoing_placement:a,incoming_placement:b,start_frame:start,hold_end_frame:exit,exit_start_frame:exit,phrase_exit:phrase_exit.clone(),end_frame:end,bar_frames:f.bar_frames.clone(),timing_basis:f.timing_basis,bar_bpm:None,pulse_ratio:None,lanes:vec![],stem_assets:vec![],effects:vec![],source_edits:vec![],edit_speed_ratio_limits:if id.permission()==TacticPermission::Normal{[1.-p.normal_speed_deviation,1.+p.normal_speed_deviation]}else{p.expressive_edit_speed_limits},vocal_occupancy:vec![],vocal_guard_frames:(f.output_sample_rate_hz as f64*0.300).ceil() as u64,evidence:f.a.evidence.iter().chain(&f.b.evidence).cloned().collect(),selection_reason:format!("{id:?}: measured full-window eligibility, phrase, vocal and renderer constraints passed"),emergency:if id.permission()==TacticPermission::EmergencyOnly{p.emergency.clone()}else{None}};
    if needs_stems(id) {
        plan.stem_assets =
            f.a.stem_assets
                .iter()
                .chain(&f.b.stem_assets)
                .cloned()
                .collect();
        for stem in all_lanes() {
            let (ga, gb) = match stem {
                StemLane::Drums => (constant(start, end, 0.), constant(start, end, 1.)),
                StemLane::Bass => (
                    transfer(
                        start,
                        end,
                        exit + (resolve_end - exit) / 4,
                        exit + 3 * (resolve_end - exit) / 4,
                        false,
                    ),
                    transfer(
                        start,
                        end,
                        exit + (resolve_end - exit) / 4,
                        exit + 3 * (resolve_end - exit) / 4,
                        true,
                    ),
                ),
                _ => (
                    transfer(start, end, exit, resolve_end, false),
                    transfer(start, end, exit, resolve_end, true),
                ),
            };
            plan.lanes.push(lane(a, stem, ga));
            plan.lanes.push(lane(b, stem, gb));
        }
    } else {
        plan.lanes.push(lane(
            a,
            StemLane::Stereo,
            transfer(start, end, start, end, false),
        ));
        plan.lanes.push(lane(
            b,
            StemLane::Stereo,
            transfer(start, end, start, end, true),
        ));
    }
    match id {
        M01 => {
            for l in &mut plan.lanes {
                if matches!(l.lane, StemLane::Bass | StemLane::Other)
                    && !l.gain.iter().any(|g| g.output_frame == exit)
                {
                    l.gain.push(ScalarPoint {
                        output_frame: exit,
                        value: if l.placement == a { 1. } else { 0. },
                    });
                    l.gain.sort_by_key(|g| g.output_frame);
                }
            }
        }
        M02 | S05 | S06 => {}
        M03 => {
            for l in &mut plan.lanes {
                if l.lane != StemLane::Vocals {
                    l.gain = constant(start, end, if l.placement == b { 1. } else { 0. });
                }
            }
        }
        M04 => {
            let motif = f.a.motif.as_ref().unwrap();
            for l in &mut plan.lanes {
                l.gain = if l.placement == a && l.lane == motif.lane {
                    transfer(
                        start,
                        end,
                        motif.phrase_end_frame.saturating_sub(ramp).max(start),
                        motif.phrase_end_frame.min(end),
                        false,
                    )
                } else if l.placement == a {
                    constant(start, end, 0.)
                } else if l.lane == motif.lane {
                    transfer(
                        start,
                        end,
                        motif.phrase_end_frame.saturating_sub(ramp).max(start),
                        motif.phrase_end_frame.min(end),
                        true,
                    )
                } else {
                    constant(start, end, 1.)
                };
            }
        }
        M05 => {
            for l in &mut plan.lanes {
                if l.lane == StemLane::Other {
                    l.gain = if l.placement == a {
                        curve(
                            start,
                            end,
                            &[(start, 0.65), (f.bar_frames[bars - 1], 0.65), (end, 0.)],
                        )
                    } else {
                        curve(
                            start,
                            end,
                            &[(start, 0.65), (f.bar_frames[bars - 1], 0.65), (end, 1.)],
                        )
                    };
                }
            }
        }
        M06 => {
            for l in &mut plan.lanes {
                if l.lane == StemLane::Vocals {
                    let phrases = if l.placement == a {
                        f.a.vocal_phrases.as_ref().unwrap()
                    } else {
                        f.b.vocal_phrases.as_ref().unwrap()
                    };
                    let mut points =
                        vec![(start, 0.), (end, if l.placement == b { 1. } else { 0. })];
                    for v in phrases
                        .iter()
                        .filter(|v| v.start_frame >= start && v.end_frame <= end)
                    {
                        points.extend([
                            (v.start_frame.saturating_sub(ramp).max(start), 0.),
                            (v.start_frame, 1.),
                            (v.end_frame, 1.),
                            ((v.end_frame + ramp).min(end), 0.),
                        ]);
                    }
                    l.gain = curve(start, end, &points);
                } else {
                    l.gain = constant(start, end, if l.placement == b { 1. } else { 0. });
                }
            }
        }
        S01 | S03 | S04 => {
            let mid = start + (end - start) / 2;
            for l in &mut plan.lanes {
                l.gain = constant(start, end, 1.);
                let incoming = l.placement == b;
                let values = if incoming {
                    [
                        [0., 0., 0.],
                        [0., 0.25, 0.8],
                        [0.5, 0.5, 1.],
                        [1., 0.75, 1.],
                        [1., 1., 1.],
                    ]
                } else {
                    [
                        [1., 1., 1.],
                        [1., 0.75, 0.2],
                        [0.5, 0.5, 0.],
                        [0., 0.25, 0.],
                        [0., 0., 0.],
                    ]
                };
                let times = [
                    start,
                    start + (end - start) / 4,
                    mid,
                    start + 3 * (end - start) / 4,
                    end,
                ];
                l.eq = times
                    .into_iter()
                    .zip(values)
                    .map(|(output_frame, gains)| BandGainPoint {
                        output_frame,
                        gains,
                    })
                    .collect();
                if id == S03 {
                    // Upper layers blend smoothly while bass switches over one beat.
                    l.eq = vec![
                        BandGainPoint {
                            output_frame: start,
                            gains: if incoming { [0.; 3] } else { [1.; 3] },
                        },
                        BandGainPoint {
                            output_frame: mid.saturating_sub(beat / 2).max(start + 1),
                            gains: if incoming {
                                [0., 0.45, 0.45]
                            } else {
                                [1., 0.55, 0.55]
                            },
                        },
                        BandGainPoint {
                            output_frame: (mid + beat / 2).min(end - 1),
                            gains: if incoming {
                                [1., 0.55, 0.55]
                            } else {
                                [0., 0.45, 0.45]
                            },
                        },
                        BandGainPoint {
                            output_frame: end,
                            gains: if incoming { [1.; 3] } else { [0.; 3] },
                        },
                    ];
                }
                if id == S04 {
                    let (order, _) = band_exchange_order(f).ok_or_else(|| {
                        Error::new("TACTIC_SHAPE", "three-band occupancy is unknown")
                    })?;
                    let times = [
                        start,
                        f.bar_frames[bars / 3],
                        f.bar_frames[2 * bars / 3],
                        end,
                    ];
                    let mut incoming_gains = [0.; 3];
                    l.eq = times
                        .into_iter()
                        .enumerate()
                        .map(|(step, output_frame)| {
                            if step > 0 {
                                incoming_gains[order[step - 1]] = 1.;
                            }
                            BandGainPoint {
                                output_frame,
                                gains: if incoming {
                                    incoming_gains
                                } else {
                                    incoming_gains.map(|g| 1. - g)
                                },
                            }
                        })
                        .collect();
                }
            }
            if id == S04 {
                let (order, occupancy) = band_exchange_order(f).unwrap();
                let names = ["low", "mid", "high"];
                plan.evidence.push(format!("S04 measured mean band occupancy A={:?}, B={:?}; exchange order {} -> {} -> {}, prioritizing incoming occupancy times outgoing space",occupancy[0],occupancy[1],names[order[0]],names[order[1]],names[order[2]]));
            }
        }
        S02 | S07 | S13 => {}
        S08 => {
            let ny = f.output_sample_rate_hz as f64 * 0.42;
            effect(
                &mut plan,
                a,
                StemLane::Stereo,
                start,
                end,
                end,
                EffectKind::Filter {
                    mode: FilterMode::HighPass,
                    cutoff_hz: curve(start, end, &[(start, 20.), (end, ny.min(9000.))]),
                    resonance: 0.707,
                },
                constant(start, end, 1.),
            );
            effect(
                &mut plan,
                b,
                StemLane::Stereo,
                start,
                end,
                end,
                EffectKind::Filter {
                    mode: FilterMode::LowPass,
                    cutoff_hz: curve(start, end, &[(start, 200.), (end, ny)]),
                    resonance: 0.707,
                },
                constant(start, end, 1.),
            );
        }
        S09 | S10 | S11 => {
            let input_end = if id == S10 {
                (start + bar).min(end - ramp)
            } else if f.timing_basis == TacticTimingBasis::Structural {
                start + (end - start) / 4
            } else {
                f.bar_frames[bars - 1].max(start + ramp)
            };
            plan.exit_start_frame = input_end;
            plan.hold_end_frame = input_end;
            plan.lanes[0].gain = transfer(start, end, end - ramp, end, false);
            plan.lanes[1].gain = transfer(
                start,
                end,
                input_end.saturating_sub(ramp).max(start),
                input_end,
                true,
            );
            let kind = if id == S11 {
                EffectKind::Reverb {
                    decay_seconds: ((end - input_end) as f64 / f.output_sample_rate_hz as f64)
                        .clamp(0.05, 30.),
                    freeze_at: None,
                    high_pass_hz: 250.,
                    low_pass_hz: (f.output_sample_rate_hz as f64 * 0.40).min(10000.),
                }
            } else {
                EffectKind::Delay {
                    delay_frames: constant(start, end, beat.max(1) as f64),
                    feedback: if id == S10 { 0.85 } else { 0.45 },
                    freeze_at: if id == S10 { Some(input_end) } else { None },
                    high_pass_hz: 250.,
                    low_pass_hz: 8000f64.min(f.output_sample_rate_hz as f64 * 0.40),
                }
            };
            effect(
                &mut plan,
                a,
                StemLane::Stereo,
                start,
                input_end,
                end,
                kind,
                curve(start, end, &[(start, 0.25), (input_end, 1.), (end, 0.)]),
            );
            if id == S10 {
                effect(
                    &mut plan,
                    a,
                    StemLane::Stereo,
                    start,
                    end,
                    end,
                    EffectKind::Filter {
                        mode: FilterMode::HighPass,
                        cutoff_hz: curve(
                            start,
                            end,
                            &[
                                (start, 100.),
                                (end, 4000f64.min(f.output_sample_rate_hz as f64 * 0.4)),
                            ],
                        ),
                        resonance: 0.707,
                    },
                    constant(start, end, 1.),
                );
            }
        }
        S12 => {
            let passage = if loop_candidate(&f.a, p).is_some() {
                &f.a
            } else {
                &f.b
            };
            let capture = loop_candidate(passage, p).unwrap();
            plan.source_edits.push(edit(
                passage,
                StemLane::Stereo,
                capture.source_start_frame,
                capture.source_end_frame,
                start,
                end,
                f.output_sample_rate_hz,
                SourceEditKind::Loop {
                    period_frames: capture.output_end_frame - capture.output_start_frame,
                },
            ));
        }
        C01 | C02 | C03 | C04 | C07 => {
            let cut = if id == C07 {
                f.b.arrival_frame.unwrap_or(start)
            } else {
                landing
            };
            plan.hold_end_frame = cut;
            let release = f.a.build_release_frame.filter(|&release| release <= cut);
            let fade_start = cut
                .saturating_sub(ramp)
                .max(start)
                .max(release.unwrap_or(start));
            let fade_end = if fade_start == cut && cut != start {
                (cut + ramp).min(end)
            } else {
                cut
            };
            plan.exit_start_frame = fade_start;
            plan.lanes[0].gain = if cut == start {
                constant(start, end, 0.)
            } else {
                transfer(start, end, fade_start, fade_end, false)
            };
            plan.lanes[1].gain = if cut == start {
                constant(start, end, 1.)
            } else {
                transfer(start, end, fade_start, fade_end, true)
            };
        }
        C05 => {
            let capture = loop_candidate(&f.a, p).unwrap();
            let roll_start = landing.saturating_sub(beat * 4).max(start);
            let stage = (landing - roll_start) / 4;
            require(
                stage >= 16 * ramp,
                "TACTIC_EDIT",
                "roll has insufficient rhythmic runway",
            )?;
            for i in 0..4 {
                let cursor = roll_start + i * stage;
                let finish = if i == 3 { landing } else { cursor + stage };
                let period = stage / (1 << i);
                let fraction =
                    period as f64 / (capture.output_end_frame - capture.output_start_frame) as f64;
                let source_end = (capture.source_start_frame as f64
                    + (capture.source_end_frame - capture.source_start_frame) as f64
                        * fraction.min(1.))
                .round() as u64;
                plan.source_edits.push(edit(
                    &f.a,
                    StemLane::Stereo,
                    capture.source_start_frame,
                    source_end.max(capture.source_start_frame + 1),
                    cursor,
                    finish,
                    f.output_sample_rate_hz,
                    SourceEditKind::Loop {
                        period_frames: period,
                    },
                ));
            }
            plan.lanes[0].gain = transfer(start, end, landing.saturating_sub(ramp), landing, false);
            plan.lanes[1].gain = transfer(start, end, landing.saturating_sub(ramp), landing, true);
        }
        C06 => {
            for placement in [a, b] {
                effect(
                    &mut plan,
                    placement,
                    StemLane::Stereo,
                    start,
                    end,
                    end,
                    EffectKind::Gate {
                        period_frames: beat,
                        phase_frames: if placement == a { 0 } else { beat / 2 },
                        duty_cycle: 0.5,
                        smoothing_frames: ramp.min(beat / 8).max(1),
                    },
                    constant(start, end, 1.),
                );
            }
        }
        C08 => {
            let motif = f.a.motif.as_ref().unwrap();
            let sample = motif.sample.as_ref().unwrap();
            let notes = f.pair.shared_motif.as_ref().unwrap();
            let finish = notes.last().unwrap().end_frame;
            plan.source_edits.push(edit(
                &f.a,
                motif.lane,
                sample.source_start_frame,
                sample.source_end_frame,
                start,
                finish,
                f.output_sample_rate_hz,
                SourceEditKind::PitchSequence {
                    notes: notes.clone(),
                },
            ));
            for l in &mut plan.lanes {
                l.gain = if l.placement == a && l.lane == motif.lane {
                    transfer(start, end, finish.saturating_sub(ramp), finish, false)
                } else if l.placement == a {
                    constant(start, end, 0.)
                } else {
                    transfer(start, end, finish.saturating_sub(ramp), finish, true)
                };
            }
        }
        C09 => {
            effect(
                &mut plan,
                a,
                StemLane::Stereo,
                start,
                landing,
                landing,
                EffectKind::Noise {
                    seed: 0x4d49585441435449,
                    gain: 0.08,
                },
                curve(
                    start,
                    landing,
                    &[
                        (start, 0.),
                        (landing.saturating_sub(beat / 4).max(start + 1), 0.8),
                        (landing, 0.),
                    ],
                ),
            );
            plan.lanes[0].gain = transfer(start, end, landing.saturating_sub(ramp), landing, false);
            plan.lanes[1].gain = transfer(start, end, landing.saturating_sub(ramp), landing, true);
        }
        C10 => {
            let close = (landing.saturating_sub(beat)).max(start + ramp);
            require(
                close < landing,
                "TACTIC_EFFECT",
                "spiral has no capture/tail interval",
            )?;
            effect(
                &mut plan,
                a,
                StemLane::Stereo,
                start,
                close,
                landing,
                EffectKind::Spiral {
                    delay_frames: beat.max((f.output_sample_rate_hz as f64 * 0.15).ceil() as u64)
                        as f64,
                    feedback: 0.45,
                    pitch_semitones: curve(start, landing, &[(start, 0.), (landing, 7.)]),
                },
                curve(start, landing, &[(start, 0.3), (close, 1.), (landing, 0.)]),
            );
            plan.lanes[0].gain = transfer(start, end, landing.saturating_sub(ramp), landing, false);
            plan.lanes[1].gain = transfer(start, end, landing.saturating_sub(ramp), landing, true);
        }
        C11 => {
            let motif = f.b.motif.as_ref().unwrap();
            let tease_end = (start + bar).min(landing);
            plan.source_edits.push(edit(
                &f.b,
                motif.lane,
                motif.source_start_frame,
                motif.source_end_frame,
                start,
                tease_end,
                f.output_sample_rate_hz,
                SourceEditKind::Loop {
                    period_frames: tease_end - start,
                },
            ));
            for l in &mut plan.lanes {
                l.gain = if l.placement == a {
                    transfer(start, end, landing.saturating_sub(ramp), landing, false)
                } else if l.lane == motif.lane {
                    curve(
                        start,
                        end,
                        &[
                            (start, 0.6),
                            (tease_end.saturating_sub(ramp), 0.6),
                            (tease_end, 0.),
                            (landing.saturating_sub(ramp).max(tease_end), 0.),
                            (landing, 1.),
                            (end, 1.),
                        ],
                    )
                } else {
                    transfer(start, end, landing.saturating_sub(ramp), landing, true)
                };
            }
        }
        C12 => {
            let capture = loop_candidate(&f.a, p).unwrap();
            let flourish_start = landing
                .saturating_sub(capture.output_end_frame - capture.output_start_frame)
                .max(start);
            plan.source_edits.push(edit(
                &f.a,
                StemLane::Stereo,
                capture.source_start_frame,
                capture.source_end_frame,
                flourish_start,
                landing,
                f.output_sample_rate_hz,
                SourceEditKind::SlipReverse,
            ));
            plan.lanes[0].gain = transfer(start, end, landing.saturating_sub(ramp), landing, false);
            plan.lanes[1].gain = transfer(start, end, landing.saturating_sub(ramp), landing, true);
        }
        C13 => {
            let capture = loop_candidate_for(&f.b, p, StemLane::Drums).unwrap();
            plan.source_edits.push(edit(
                &f.b,
                StemLane::Drums,
                capture.source_start_frame,
                capture.source_end_frame,
                start,
                end,
                f.output_sample_rate_hz,
                SourceEditKind::Loop {
                    period_frames: polyrhythm_period(f).ok_or_else(|| {
                        Error::new(
                            "TACTIC_PHASE",
                            "polyrhythm has no whole-cycle steady-clock landing",
                        )
                    })?,
                },
            ));
            for l in &mut plan.lanes {
                l.gain = if l.lane == StemLane::Drums && l.placement == b {
                    curve(
                        start,
                        end,
                        &[(start, 0.35), (f.bar_frames[bars - 1], 0.35), (end, 1.)],
                    )
                } else if l.placement == a {
                    transfer(start, end, f.bar_frames[bars - 1], end, false)
                } else {
                    transfer(start, end, f.bar_frames[bars - 1], end, true)
                };
            }
        }
        C14 => {
            let capture = loop_candidate_for(&f.a, p, StemLane::Drums).unwrap();
            let period = (capture.output_end_frame - capture.output_start_frame) as f64;
            let ratio = f.pair.target_tempo_ratio.unwrap();
            plan.source_edits.push(edit(
                &f.a,
                StemLane::Drums,
                capture.source_start_frame,
                capture.source_end_frame,
                start,
                landing,
                f.output_sample_rate_hz,
                SourceEditKind::LoopSequence {
                    periods: curve(
                        start,
                        landing,
                        &[(start, period), (landing, period / ratio)],
                    ),
                },
            ));
            for l in &mut plan.lanes {
                l.gain = if l.placement == a && l.lane == StemLane::Drums {
                    transfer(start, end, landing.saturating_sub(ramp), landing, false)
                } else if l.placement == a {
                    transfer(start, end, start, (start + ramp).min(landing), false)
                } else {
                    transfer(start, end, landing.saturating_sub(ramp), landing, true)
                };
            }
        }
        E01 | E02 | E03 | E04 => {
            let finish = (start + bar.min(f.output_sample_rate_hz as u64)).min(end);
            if id != E04 {
                let available =
                    f.a.ready_buffer_frames
                        .min(f.a.source.end_frame - f.a.source.start_frame);
                let source_start = f.a.source.start_frame;
                let source_end = source_start + available;
                let kind = match id {
                    E01 => SourceEditKind::Brake { initial_speed: 1. },
                    E02 => SourceEditKind::Backspin { initial_speed: 2. },
                    _ => SourceEditKind::Scratch {
                        positions: vec![
                            SourcePositionPoint {
                                output_frame: start,
                                source_frame: source_start as f64,
                            },
                            SourcePositionPoint {
                                output_frame: start + (finish - start) / 3,
                                source_frame: (source_end - 1) as f64,
                            },
                            SourcePositionPoint {
                                output_frame: start + 2 * (finish - start) / 3,
                                source_frame: source_start as f64,
                            },
                            SourcePositionPoint {
                                output_frame: finish,
                                source_frame: (source_end - 1) as f64,
                            },
                        ],
                    },
                };
                plan.source_edits.push(edit(
                    &f.a,
                    StemLane::Stereo,
                    source_start,
                    source_end,
                    start,
                    finish,
                    f.output_sample_rate_hz,
                    kind,
                ));
            }
            plan.lanes[0].gain = transfer(start, end, start, finish, false);
            plan.lanes[1].gain = transfer(start, end, finish.saturating_sub(ramp), finish, true);
        }
        D01 | D02 => unreachable!(),
    }
    for modifier in [D01, D02] {
        if eligibility(modifier, f, p).is_empty() {
            plan.modifiers.push(modifier);
            if modifier == D01 {
                plan.bar_bpm = f.pair.shared_bar_bpm.clone();
            } else {
                plan.pulse_ratio = f.pair.pulse_ratio;
            }
        }
    }
    for fx in &plan.effects {
        fx.validate(f.output_sample_rate_hz)?;
    }
    for e in &plan.source_edits {
        e.validate()?;
    }
    if !crate::vocal_audibility::apply(&mut plan, f)? {
        require(
            f.a.vocal_phrases.is_some() && f.b.vocal_phrases.is_some(),
            "TACTIC_VOCALS",
            "vocal evidence is unknown for this schedule",
        )?;
        let vocal_facts = remap_edited_vocals(&plan, f)?;
        apply_vocal_contract(&mut plan, &vocal_facts, p)?;
    }
    crate::stem_audibility::validate(&mut plan, f)?;
    let method = match id {
        M01 if phrase_exit.as_ref().is_some_and(|e| e.kind == PhraseExitKind::RhythmicHandoff) => "high-quality dense mashup held until an on-grid incoming drum accent near its measured ending; swift accompaniment exchange without requiring silence",
        M01 | M02 | S05 | S06 if phrase_exit.is_some() => "sustained stem combination resolves swiftly in a measured final-half-bar pause or completed crescendo release; incoming drums remain continuous",
        M01 => "dense selected layers remain compatible through the hold; resolve over the final two bars at the selected endpoint",
        M02 => "spacious retained bed and measured incoming arrival support a one-bar resolution",
        M03 => "complete A vocal phrase over isolated B accompaniment",
        M04 => "isolated A motif occupies a complementary B register",
        M05 => "coincident complementary drops with assigned bass and lead ownership",
        M06 => "verified answering phrases alternate without overlapping vocals",
        S01 => "stable sparse intro/outro spans support a long phrase-aligned EQ blend",
        S02 => match preferred_short_fade(f, p) {
            Some(ShortFadeMatch::QuietNearMatch) => "quiet passages have closely matching aligned audio",
            Some(ShortFadeMatch::AllBandsMatch) => "aligned audio closely matches in all three bands",
            Some(ShortFadeMatch::OneBandDifference) => "aligned drums and two matching bands leave one harmonically compatible band to exchange",
            None => "last resort: short window passes vocal, boundary and headroom checks",
        },
        S03 if last_resort(id, f, p) => "last resort: a sub-four-bar upper-band fade with a bass exchange replaces the arrangement quickly without measured band compatibility",
        S03 => "active bass on both tracks requires one bounded bass ownership exchange",
        S04 => "last resort after negative listening feedback: broad stereo-EQ band exchange; measured occupancy orders the bands but does not certify clean musical separation",
        S05 => "quiet A bed leaves room for stable B drums before the accompaniment transfer",
        S06 => "B drums replace A exactly at its supported drum-return boundary",
        S07 => "verified beatless structure permits a tonal bridge without invented pulse alignment",
        S08 => "measured busy frequency occupancy supports complementary cutoff sweeps",
        S09 => "complete ending capture decays into B with a bounded echo tail",
        S10 => "verified nonvocal capture supports a bounded filtered frozen-delay bridge",
        S11 => "isolated A texture and spacious B entry leave room for a bounded reverb tail",
        S12 => "verified loop seam and complete captured language extend the candidate runway",
        S13 => "low-priority baseline: gradual four-to-eight-bar stereo crossfade with measured grid, audible-vocal, harmony and peak checks; complete structural phrase endpoints are not claimed",
        C01|C02|C03|C04|C07 if f.pair.cut_band_matches.contains(&id) => "verified landing and matching mapped PCM in all bands support an ownership switch",
        C01|C02|C03|C04|C07 => "last resort: verified landing permits a cut, but all-band continuity is unproven",
        C05 => "verified rhythmic capture tightens in four bounded stages before the measured landing",
        C06 => "complementary vocal-free rhythmic gaps support alternating phase-offset gates",
        C08 => "isolated sample and verified note sequence introduce B's corresponding motif",
        C09 => "verified build/gap supports a bounded riser ending at B's arrival",
        C10 => "isolated nonvocal capture and tail room support a deliberate pitch-delay departure",
        C11 => "verified B motif is teased in A's available space before its complete entry",
        C12 => "dispensable nonvocal capture reverses briefly while the underlying transport continues",
        C13 => "clean B percussion overlays steady A with an explicit common pulse landing",
        C14 => "isolated percussion connects separately verified pulses within declared edit-speed limits",
        E01|E02|E03|E04 => "verified runtime failure and prepared next source activate the bounded recovery schedule",
        D01|D02 => unreachable!(),
    };
    plan.selection_reason = format!(
        "{id:?}: {method}; {} complete vocal spans, {} effect events, {} source edits",
        plan.vocal_occupancy.len(),
        plan.effects.len(),
        plan.source_edits.len()
    );
    Ok(plan)
}

fn preferred_short_fade(f: &CandidateFacts, p: &TacticPolicy) -> Option<ShortFadeMatch> {
    f.pair
        .short_fade_match
        .filter(|m| *m != ShortFadeMatch::OneBandDifference || tonal(f, p))
}

fn last_resort(id: TacticId, f: &CandidateFacts, p: &TacticPolicy) -> bool {
    match id {
        TacticId::C01 | TacticId::C02 | TacticId::C03 | TacticId::C04 | TacticId::C07 => {
            !f.pair.cut_band_matches.contains(&id)
        }
        // Listening feedback consistently rejects the broad stereo-EQ exchange.
        // Measured occupancy alone does not establish clean musical separation.
        // Keep it executable for explicit auditions, but outside clean-route search.
        TacticId::S04 => true,
        TacticId::S02 => preferred_short_fade(f, p).is_none(),
        TacticId::S03 if f.bar_frames.len() <= 4 => preferred_short_fade(f, p).is_none(),
        _ => false,
    }
}

fn preference(id: TacticId, f: &CandidateFacts, p: &TacticPolicy) -> f64 {
    use TacticId::*;
    // An executable baseline earns finite credit, never a dense/held-tactic
    // reward. Better musical strategies can replace it during set refinement.
    if id == S13 {
        return 5.;
    }
    if last_resort(id, f, p) && !matches!(id, S03 | S04) {
        // A mutually exclusive schedule has no overlapping clashes. That is
        // safety evidence, not evidence of musical continuity across its cut.
        return -1000.;
    }
    let base = match id {
        M01 => 100.,
        M02 => 95.,
        M03 => 88.,
        M04 => 82.,
        M05 => 75.,
        M06 => 82.,
        S05 => 90.,
        S06 => 94.,
        S04 => 72.,
        S03 => 45.,
        S01 => 65.,
        S02 if preferred_short_fade(f, p).is_some() => 50.,
        S02 => -1000.,
        S07 => 63.,
        S08 => 60.,
        S09 => 55.,
        S10 => 53.,
        S11 => 52.,
        S12 => 45.,
        C02 | C03 => 70.,
        C01 | C04 => 48.,
        C07 => 66.,
        E04 => 20.,
        E01 | E02 | E03 => 10.,
        _ => 35.,
    };
    // Tactic preferences break close calls. A strong measured fit can
    // outrank a more elaborate but less compatible schedule.
    let held_bars = f
        .bar_frames
        .windows(2)
        .map(|w| (hold_end(id, f).min(w[1]).saturating_sub(w[0])) as f64 / (w[1] - w[0]) as f64)
        .sum::<f64>();
    // Reward an established musical combination, not just the absence of
    // collisions after most of the outgoing layers have been removed.
    let sustain = 18. * (1. - (-held_bars / 8.).exp());
    let endpoint = if held_bars > 0.
        && (!f.a.endpoint_reasons.is_empty() || !f.b.endpoint_reasons.is_empty())
    {
        4.
    } else {
        0.
    };
    let band_risk = if id == S04 {
        band_exchange_order(f).map_or(0., |(_, [a, b])| {
            if shared_low_band_dominance(a[0], b[0]) {
                5. * ((a[0].min(b[0]) - 0.70) / 0.30).clamp(0., 1.)
            } else {
                0.
            }
        })
    } else {
        0.
    };
    // S04 is an audition-disfavored fallback, ahead of unmatched cuts/fades
    // but below every normal admitted tactic. Preserve fit differences when
    // choosing among fallback exchanges rather than giving them a flat score.
    let listening_penalty = if id == S04 {
        200.
    } else if id == S03 && last_resort(id, f, p) {
        150.
    } else {
        0.
    };
    0.20 * base + sustain + endpoint - band_risk - listening_penalty
        + 40. * f.pair.complementarity.unwrap_or(0.)
        - 40. * f.pair.harmonic_distance.unwrap_or(0.5)
        - 30. * f.pair.lead_conflict.unwrap_or(0.5)
}

fn dense_quality_seconds(id: TacticId, f: &CandidateFacts) -> f64 {
    if id != TacticId::M01
        || f.bar_frames.len() < 9
        || f.a.endpoint_reasons.is_empty() && f.b.endpoint_reasons.is_empty()
    {
        return 0.;
    }
    let (Some(comp), Some(harmony), Some(lead)) = (
        f.pair.complementarity,
        f.pair.harmonic_distance,
        f.pair.lead_conflict,
    ) else {
        return 0.;
    };
    if comp < 0.65 || harmony > 0.30 || lead > 0.20 {
        return 0.;
    }
    sustained_value(id, f, comp, harmony, lead)
}
fn handoff_quality_seconds(id: TacticId, f: &CandidateFacts) -> f64 {
    if !matches!(id, TacticId::S05 | TacticId::S06) || held_bars(id, f) < 4 {
        return 0.;
    }
    let (Some(comp), Some(harmony), Some(lead)) = (
        f.pair.complementarity,
        f.pair.harmonic_distance,
        f.pair.lead_conflict,
    ) else {
        return 0.;
    };
    let n = held_bars(id, f);
    // Reward a substantial retained musical bed with steady drums, not an
    // empty passage or merely a low clash score after removing everything.
    if comp < 0.60
        || harmony > 0.30
        || lead > 0.25
        || !f.a.bars[..n]
            .iter()
            .all(|b| known(b.stability, |v| v >= 0.80))
        || !f.b.bars[..n]
            .iter()
            .all(|b| known(b.stability, |v| v >= 0.85))
        || !f.pair.retained_bed_density.as_ref().is_some_and(|d| {
            d.len() + 1 == f.bar_frames.len() && d[..n].iter().all(|v| v.is_finite() && *v >= 0.15)
        })
    {
        return 0.;
    }
    sustained_value(id, f, comp, harmony, lead)
}
fn sustained_value(id: TacticId, f: &CandidateFacts, comp: f64, harmony: f64, lead: f64) -> f64 {
    let until = hold_end(id, f);
    f.bar_frames
        .windows(2)
        .enumerate()
        .map(|(i, w)| {
            let stability = f.a.bars[i]
                .stability
                .unwrap_or(0.)
                .min(f.b.bars[i].stability.unwrap_or(0.));
            until.min(w[1]).saturating_sub(w[0]) as f64 / f64::from(f.output_sample_rate_hz)
                * comp
                * (1. - harmony)
                * (1. - lead)
                * stability
        })
        .sum()
}

fn specialized_facts(id: TacticId, f: &CandidateFacts) -> CandidateFacts {
    let mut candidate = f.clone();
    if let Some(profile) = f.pair.profiles.get(&id) {
        candidate.pair.harmonic_distance = profile.harmonic_distance;
        candidate.pair.lead_conflict = profile.lead_conflict;
        candidate.pair.complementarity = profile.complementarity;
        candidate.pair.predicted_peak_dbfs = profile.predicted_peak_dbfs;
        candidate.pair.selected_composite_density = profile.selected_composite_density.clone();
        candidate.pair.retained_bed_density = profile.retained_bed_density.clone();
        if matches!(
            id,
            TacticId::M01 | TacticId::M02 | TacticId::S05 | TacticId::S06
        ) {
            for (passage, measured) in [
                (&mut candidate.a, &profile.outgoing_layer_stability),
                (&mut candidate.b, &profile.incoming_layer_stability),
            ] {
                for (i, bar) in passage.bars.iter_mut().enumerate() {
                    bar.stability = measured
                        .as_ref()
                        .filter(|v| v.len() + 1 == f.bar_frames.len())
                        .and_then(|v| v.get(i))
                        .copied();
                }
            }
        }
        candidate
            .a
            .evidence
            .extend(profile.evidence.iter().cloned());
        candidate
            .a
            .evidence
            .push(format!("pair allocation {id:?}: {}", profile.method));
    }
    candidate
}
/// Enumerate all tactic families, retaining reasons for every rejected alternative.
pub fn select_tactics(f: &CandidateFacts, p: &TacticPolicy) -> Result<TacticSelection> {
    p.validate()?;
    f.validate()?;
    let mut candidates = vec![];
    let mut rejected = vec![];
    for id in TacticId::ALL {
        if id.is_modifier() {
            continue;
        }
        let mut variants = vec![f.clone()];
        if sustained_tactic(id)
            && f.a.phrase_exit.is_some()
            && f.pair.swift_profiles.contains_key(&id)
        {
            let mut swift = f.clone();
            swift.phrase_exit = f.a.phrase_exit.clone();
            swift.pair.profiles = f.pair.swift_profiles.clone();
            variants.push(swift);
        }
        if id == TacticId::M01
            && f.pair.rhythmic_exit.is_some()
            && f.pair.rhythmic_profiles.contains_key(&id)
        {
            let mut rhythmic = f.clone();
            rhythmic.phrase_exit = f.pair.rhythmic_exit.clone();
            rhythmic.pair.profiles = f.pair.rhythmic_profiles.clone();
            variants.push(rhythmic);
        }
        for variant in &variants {
            let f = variant;
            let specialized = specialized_facts(id, f);
            let mut reasons = eligibility(id, &specialized, p);
            if f.phrase_exit.is_some() && !reasons.is_empty() {
                reasons = reasons
                    .into_iter()
                    .map(|r| format!("swift exit: {r}"))
                    .collect();
                if let Some(profile) = f.pair.profiles.get(&id) {
                    reasons.extend(
                        profile
                            .evidence
                            .iter()
                            .filter(|r| r.starts_with("swift exit:"))
                            .cloned(),
                    );
                }
            }
            if !reasons.is_empty() {
                rejected.push(TacticRejection {
                    tactic_id: id,
                    reasons,
                });
                continue;
            }
            match schedule_tactic(id, f, p) {
                Ok(plan) => candidates.push(RankedTactic {
                    plan,
                    score: preference(id, &specialized, p),
                    last_resort: last_resort(id, &specialized, p),
                    dense_quality_seconds: dense_quality_seconds(id, &specialized),
                    handoff_quality_seconds: handoff_quality_seconds(id, &specialized),
                }),
                Err(e) => rejected.push(TacticRejection {
                    tactic_id: id,
                    reasons: vec![format!("{}: {}", e.code, e.message)],
                }),
            }
        }
    }
    candidates.sort_by(|a, b| {
        a.last_resort.cmp(&b.last_resort).then_with(|| {
            b.score
                .total_cmp(&a.score)
                .then(a.plan.tactic_id.cmp(&b.plan.tactic_id))
        })
    });
    Ok(TacticSelection {
        candidates,
        rejected,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    const RATE: u32 = 48000;
    const BAR: u64 = 96000;
    pub(crate) fn facts(n: usize) -> CandidateFacts {
        let bar_frames: Vec<_> = (0..=n).map(|n| n as u64 * BAR).collect();
        let end = n as u64 * BAR;
        let passage = |placement| PassageFacts {
            placement,
            source: SourceWindow {
                sample_rate_hz: RATE,
                start_frame: 0,
                end_frame: end + 4 * BAR,
                output_start_frame: 0,
                output_end_frame: end + 4 * BAR,
            },
            bars: bar_frames
                .windows(2)
                .map(|w| BarEvidence {
                    start_frame: w[0],
                    end_frame: w[1],
                    rms: Some(0.12),
                    crest_db: Some(6.),
                    onsets_per_beat: Some(1.5),
                    low_occupancy: Some(0.6),
                    mid_occupancy: Some(0.7),
                    high_occupancy: Some(0.4),
                    active_stems: Some(4),
                    chroma: Some([1. / 12.; 12]),
                    beats_per_bar: Some(4),
                    grid_score: Some(0.9),
                    stability: Some(0.9),
                })
                .collect(),
            roles: BTreeMap::new(),
            phrase_boundaries: bar_frames
                .iter()
                .map(|&output_frame| PhraseBoundary {
                    output_frame,
                    supported_period_bars: None,
                    kind: BoundaryKind::Both,
                    score_margin: 0.5,
                    complete: true,
                })
                .collect(),
            vocal_phrases: Some(vec![]),
            vocal_quiet: None,
            stems: Some(StemEvidence {
                available: all_lanes().into_iter().collect(),
                max_vocal_leakage_db: -45.,
                reconstruction_error: 0.01,
                artifact_score: 0.02,
            }),
            stem_leakage: None,
            phrase_exit: None,
            endpoint_reasons: vec!["fixture measured drum change".into()],
            stem_assets: all_lanes()
                .iter()
                .map(|&lane| StemAsset {
                    placement,
                    lane,
                    audio: FileRef {
                        path: format!("{placement}-{lane:?}.wav"),
                        hash: hash_bytes(b"fixture"),
                    },
                    source_offset_frame: 0,
                    source_end_frame: end + 4 * BAR,
                })
                .collect(),
            loop_candidates: [StemLane::Stereo, StemLane::Drums]
                .into_iter()
                .map(|lane| LoopEvidence {
                    lane,
                    source_start_frame: 0,
                    source_end_frame: BAR,
                    output_start_frame: 0,
                    output_end_frame: BAR,
                    seam_error: 0.001,
                    tonal_cycle_distance: 0.01,
                    vocal_free: true,
                    contains_complete_language: None,
                    confidence_margin: 0.5,
                })
                .collect(),
            motif: Some(MotifEvidence {
                source_start_frame: 0,
                source_end_frame: BAR,
                phrase_end_frame: 4 * BAR.min(end / 4),
                lane: StemLane::Other,
                isolation_score: 0.95,
                transcription_margin: 0.6,
                sample: Some(MotifSample {
                    source_start_frame: 0,
                    source_end_frame: BAR,
                    midi_note: 60.,
                    single_fundamental_score: 0.95,
                }),
            }),
            version_identity: Some("verified-version-family".into()),
            natural_end_frame: Some(end),
            arrival_frame: Some(end),
            cut_attacks: vec![],
            build_release_frame: None,
            ready_buffer_frames: BAR,
            evidence: vec![format!("measured-fixture/{placement}")],
        };
        CandidateFacts {
            phrase_exit: None,
            output_sample_rate_hz: RATE,
            bar_frames: bar_frames.clone(),
            timing_basis: TacticTimingBasis::Bars,
            a: passage(0),
            b: passage(1),
            pair: PairEvidence {
                profiles: BTreeMap::new(),
                swift_profiles: BTreeMap::new(),
                rhythmic_exit: None,
                rhythmic_profiles: BTreeMap::new(),
                beat_skew_ms: Some(1.),
                harmonic_distance: Some(0.05),
                lead_conflict: Some(0.05),
                complementarity: Some(0.9),
                predicted_peak_dbfs: Some(-6.),
                selected_composite_density: Some(vec![0.8; n]),
                retained_bed_density: Some(vec![0.35; n]),
                pulse_ratio: Some(1.),
                target_tempo_ratio: Some(1.5),
                groove_similarity: Some(0.95),
                short_fade_match: None,
                cut_band_matches: BTreeSet::new(),
                semantic_relation: Some(SemanticRelation {
                    relation: "answer".into(),
                    confidence_margin: 0.8,
                    evidence: vec!["verified-lyric-alignment".into()],
                }),
                shared_motif: Some(vec![
                    PitchNote {
                        start_frame: 0,
                        end_frame: BAR,
                        semitones: 0.,
                        gain: 0.7,
                    },
                    PitchNote {
                        start_frame: BAR,
                        end_frame: 2 * BAR,
                        semitones: 3.,
                        gain: 0.7,
                    },
                ]),
                shared_bar_bpm: None,
                tempo_maps_validated: true,
            },
        }
    }
    fn policy() -> TacticPolicy {
        use RenderCapability::*;
        TacticPolicy {
            allow_expressive: true,
            capabilities: [
                Gain,
                Eq,
                Stems,
                Filter,
                Delay,
                Reverb,
                Gate,
                Noise,
                Spiral,
                SourceEdits,
                PitchSampler,
                TempoMap,
                PulseMapping,
                EmergencyRecovery,
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        }
    }
    #[test]
    fn one_sided_filter_requires_measured_crowding_on_at_least_one_side() {
        let mut window = facts(4);
        let mut policy = policy();
        let admitted = |facts: &CandidateFacts, policy: &TacticPolicy| {
            select_tactics(facts, policy)
                .unwrap()
                .candidates
                .iter()
                .any(|candidate| candidate.plan.tactic_id == TacticId::S08)
        };
        assert!(admitted(&window, &policy));
        for bar in &mut window.b.bars {
            bar.active_stems = Some(1);
            bar.mid_occupancy = Some(0.2);
            bar.onsets_per_beat = Some(0.5);
        }
        assert!(!admitted(&window, &policy));
        policy.allow_one_sided_filter = true;
        assert!(admitted(&window, &policy));
        window.b.bars[0].active_stems = None;
        assert!(!admitted(&window, &policy));
    }

    #[test]
    fn baseline_blend_uses_measured_safety_without_sparse_or_complete_phrase_claims() {
        let mut window = facts(4);
        for passage in [&mut window.a, &mut window.b] {
            passage.roles.clear();
            passage.phrase_boundaries.clear();
            passage.endpoint_reasons.clear();
            for bar in &mut passage.bars {
                bar.stability = None;
                bar.active_stems = None;
                bar.mid_occupancy = None;
            }
        }
        let selection = select_tactics(&window, &policy()).unwrap();
        let baseline = selection
            .candidates
            .iter()
            .find(|candidate| candidate.plan.tactic_id == TacticId::S13)
            .unwrap();
        assert_eq!(baseline.score, 5.);
        assert!(!baseline.last_resort);
        assert_eq!(baseline.dense_quality_seconds, 0.);
        assert_eq!(baseline.handoff_quality_seconds, 0.);
        assert_eq!(baseline.plan.lanes.len(), 2);
        for lane in &baseline.plan.lanes {
            assert_eq!(lane.lane, StemLane::Stereo);
            assert!(lane.eq.is_empty());
            assert_eq!(lane.gain.len(), 2);
            assert_eq!(curve_value(&lane.gain, 2 * BAR), 0.5);
        }
        validate_compiled(&baseline.plan, &window);
        assert!(schedule_tactic(TacticId::S01, &window, &policy()).is_err());
    }

    #[test]
    fn baseline_blend_keeps_grid_tonal_peak_build_and_window_guards() {
        let baseline = facts(4);
        let rejected = |window: CandidateFacts| {
            assert!(schedule_tactic(TacticId::S13, &window, &policy()).is_err());
        };
        for bars in [1, 2, 3, 9] {
            rejected(facts(bars));
        }
        let mut window = baseline.clone();
        window.a.bars[0].grid_score = None;
        rejected(window);
        let mut window = baseline.clone();
        window.pair.beat_skew_ms = Some(31.);
        rejected(window);
        let mut window = baseline.clone();
        window.pair.harmonic_distance = Some(0.31);
        rejected(window);
        let mut window = baseline.clone();
        window.pair.lead_conflict = Some(0.26);
        rejected(window);
        let mut window = baseline.clone();
        window.pair.predicted_peak_dbfs = Some(0.);
        rejected(window);
        let mut window = baseline.clone();
        window.a.build_release_frame = Some(BAR);
        rejected(window);
        let mut window = baseline;
        window.a.build_release_frame = Some(0);
        assert!(schedule_tactic(TacticId::S13, &window, &policy()).is_ok());
    }

    #[test]
    fn baseline_blend_requires_complete_audible_lines_without_overlap() {
        let mut window = facts(4);
        window.a.vocal_phrases = Some(vec![vocal(0, BAR)]);
        window.b.vocal_phrases = Some(vec![vocal(2 * BAR, 3 * BAR)]);
        let plan = schedule_tactic(TacticId::S13, &window, &policy()).unwrap();
        assert_eq!(plan.vocal_occupancy.len(), 2);
        window.b.vocal_phrases = Some(vec![vocal(BAR / 2, 2 * BAR)]);
        assert!(schedule_tactic(TacticId::S13, &window, &policy()).is_err());
        window.b.vocal_phrases = Some(vec![]);
        window.a.vocal_phrases = Some(vec![vocal(3 * BAR, 5 * BAR)]);
        assert!(schedule_tactic(TacticId::S13, &window, &policy()).is_err());
        window.a.vocal_phrases = None;
        assert!(schedule_tactic(TacticId::S13, &window, &policy()).is_err());
    }

    fn role_set(p: &mut PassageFacts, roles: &[TacticRole]) {
        p.roles = roles.iter().map(|&r| (r, 0.9)).collect();
    }
    fn vocal(start: u64, end: u64) -> VocalPhrase {
        VocalPhrase {
            start_frame: start,
            end_frame: end,
            tail_end_frame: end,
            entry_supported: true,
            exit_supported: true,
            confidence_margin: 0.5,
        }
    }
    fn validate_compiled(t: &TacticPlan, f: &CandidateFacts) {
        let policy = MixingConfig::default();
        let placements = [&f.a, &f.b]
            .into_iter()
            .map(|p| Placement {
                track_id: format!("track-{}", p.placement),
                audio_hash: hash_bytes(b"fixture"),
                audio_revision: 1,
                analysis: FileRef {
                    path: "analysis.json".into(),
                    hash: hash_bytes(b"analysis"),
                },
                override_revision: 0,
                source_start_frame: p.source.start_frame,
                source_end_frame: p.source.end_frame,
                time_map: TimeMap {
                    header: Header::default(),
                    map_id: format!("map-{}", p.placement),
                    source_sample_rate_hz: p.source.sample_rate_hz,
                    output_sample_rate_hz: f.output_sample_rate_hz,
                    interpolation: "monotone_cubic".into(),
                    anchors: vec![
                        Anchor {
                            source_frame: p.source.start_frame,
                            output_frame: p.source.output_start_frame,
                        },
                        Anchor {
                            source_frame: p.source.end_frame,
                            output_frame: p.source.output_end_frame,
                        },
                    ],
                    speed_ratio_limits: [0.94, 1.06],
                    max_speed_change_per_second: 0.02,
                    preserve_pitch: true,
                },
                automation: vec![],
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
                sample_rate_hz: f.output_sample_rate_hz,
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
                outgoing_region_id: "a".into(),
                incoming_region_id: "b".into(),
                start_output_frame: t.start_frame,
                end_output_frame: t.end_frame,
                template: Template::ShortCrossfade,
                costs: BTreeMap::new(),
                fallback_reason: None,
            }],
            total_output_frames: f.a.source.output_end_frame.max(f.b.source.output_end_frame),
            crossover_hz: [200., 2500.],
            crossover_design: "complementary_one_pole_v1".into(),
            automation_interpolation: "smoothstep_linear_amplitude".into(),
            diagnostics: vec![],
        };
        t.validate(&plan).unwrap_or_else(|e| {
            panic!(
                "{:?} compiled an invalid plan: {} {}",
                t.tactic_id, e.code, e.message
            )
        });
    }
    fn dense() -> CandidateFacts {
        let mut f = facts(8);
        role_set(&mut f.a, &[TacticRole::SustainedTexture]);
        role_set(&mut f.b, &[TacticRole::SteadyGroove]);
        f
    }

    fn with_swift_exit(mut f: CandidateFacts) -> CandidateFacts {
        let end = *f.bar_frames.last().unwrap();
        let exit = PhraseExit {
            start_frame: end - BAR / 4,
            end_frame: end - BAR / 4 + 3840,
            kind: PhraseExitKind::AccompanimentPause,
            evidence: "measured test PCM pause".into(),
        };
        f.a.phrase_exit = Some(exit.clone());
        f.phrase_exit = Some(exit);
        f
    }
    #[test]
    fn rhythmic_handoff_keeps_a_dense_mix_without_quiet_accompaniment() {
        let mut f = with_swift_exit(dense());
        let mut exit = f.phrase_exit.take().unwrap();
        exit.start_frame = 7 * BAR + BAR / 2;
        exit.end_frame = exit.start_frame + 3840;
        exit.kind = PhraseExitKind::RhythmicHandoff;
        exit.evidence = "measured incoming drum accent near phrase end".into();
        f.a.phrase_exit = None;
        f.pair.rhythmic_exit = Some(exit.clone());
        f.phrase_exit = Some(exit);
        let t = schedule_tactic(TacticId::M01, &f, &policy()).unwrap();
        validate_compiled(&t, &f);
        assert_eq!(t.hold_end_frame, 7 * BAR + BAR / 2);
        assert!(schedule_tactic(TacticId::S06, &f, &policy()).is_err());
        f.pair.complementarity = Some(0.64);
        assert!(schedule_tactic(TacticId::M01, &f, &policy()).is_err());
    }
    #[test]
    fn long_stable_handoff_gets_credit_but_empty_or_unstable_beds_do_not() {
        let mut f = facts(12);
        role_set(
            &mut f.a,
            &[TacticRole::DrumReturn, TacticRole::SustainedTexture],
        );
        role_set(&mut f.b, &[TacticRole::SteadyGroove]);
        f.a.arrival_frame = Some(0);
        f.pair.complementarity = Some(0.69);
        f.pair.harmonic_distance = Some(0.235);
        f.pair.lead_conflict = Some(0.148);
        let candidates = select_tactics(&f, &policy()).unwrap().candidates;
        let handoff = candidates
            .iter()
            .find(|c| c.plan.tactic_id == TacticId::S06)
            .unwrap();
        assert!(handoff.handoff_quality_seconds > 0.);
        assert_eq!(handoff.dense_quality_seconds, 0.);
        f.a.bars[3].stability = Some(0.5);
        assert_eq!(handoff_quality_seconds(TacticId::S06, &f), 0.);
        f.a.bars[3].stability = Some(0.9);
        f.pair.retained_bed_density = Some(vec![0.05; 12]);
        assert_eq!(handoff_quality_seconds(TacticId::S06, &f), 0.);
        assert_eq!(handoff_quality_seconds(TacticId::S03, &facts(12)), 0.);
    }
    #[test]
    fn swift_exit_keeps_drums_and_full_hold_then_exchanges_accompaniment() {
        let slow = dense();
        let f = with_swift_exit(slow.clone());
        let t = schedule_tactic(TacticId::M01, &f, &policy()).unwrap();
        validate_compiled(&t, &f);
        let e = t.phrase_exit.as_ref().unwrap();
        assert!(
            dense_quality_seconds(TacticId::M01, &f) > dense_quality_seconds(TacticId::M01, &slow)
        );
        assert!(!last_resort(TacticId::M01, &f, &policy()));
        for lane in &t.lanes {
            if lane.lane == StemLane::Drums {
                assert!(lane
                    .gain
                    .iter()
                    .all(|p| p.value == if lane.placement == 0 { 0. } else { 1. }));
            } else if matches!(lane.lane, StemLane::Bass | StemLane::Other) {
                assert_eq!(
                    curve_value(&lane.gain, e.start_frame),
                    if lane.placement == 0 { 1. } else { 0. }
                );
                assert_eq!(
                    curve_value(&lane.gain, e.end_frame),
                    if lane.placement == 0 { 0. } else { 1. }
                );
            }
        }
        let shifted = t.translated(1000, 3).unwrap();
        assert_eq!(
            shifted.phrase_exit.unwrap().start_frame,
            e.start_frame + 1000
        );
    }
    #[test]
    fn swift_exit_cannot_truncate_a_build_vocal_or_unstable_extended_hold() {
        let f = with_swift_exit(dense());
        let mut build = f.clone();
        build.a.build_release_frame = Some(*f.bar_frames.last().unwrap());
        assert!(schedule_tactic(TacticId::M01, &build, &policy()).is_err());
        build.a.build_release_frame = Some(f.phrase_exit.as_ref().unwrap().start_frame - 960);
        assert!(schedule_tactic(TacticId::M01, &build, &policy()).is_ok());
        let mut invalid = f.clone();
        invalid.phrase_exit.as_mut().unwrap().start_frame = 20 * BAR;
        assert!(schedule_tactic(TacticId::M01, &invalid, &policy()).is_err());
        let mut unstable = f.clone();
        unstable.a.bars[6].stability = Some(0.1);
        assert!(schedule_tactic(TacticId::M01, &unstable, &policy()).is_err());
        let mut vocal = f.clone();
        vocal.a.vocal_phrases = Some(vec![VocalPhrase {
            start_frame: 6 * BAR,
            end_frame: 8 * BAR,
            tail_end_frame: 8 * BAR,
            entry_supported: true,
            exit_supported: true,
            confidence_margin: 0.9,
        }]);
        assert!(schedule_tactic(TacticId::M01, &vocal, &policy()).is_err());
    }
    #[test]
    fn failed_swift_variant_keeps_the_normal_mashup_candidate() {
        let mut f = with_swift_exit(dense());
        f.phrase_exit = None;
        f.pair
            .swift_profiles
            .insert(TacticId::M01, PairProfile::default());
        let result = select_tactics(&f, &policy()).unwrap();
        assert!(result
            .candidates
            .iter()
            .any(|c| c.plan.tactic_id == TacticId::M01 && c.plan.phrase_exit.is_none()));
        assert!(!result
            .candidates
            .iter()
            .any(|c| c.plan.tactic_id == TacticId::M01 && c.plan.phrase_exit.is_some()));
    }
    #[test]
    fn six_plus_two_keeps_accompaniment_fixed_while_vocals_finish() {
        let mut f = dense();
        f.a.vocal_phrases = Some(vec![vocal(0, 6 * BAR + BAR / 2)]);
        f.b.vocal_phrases = Some(vec![vocal(7 * BAR, 9 * BAR)]);
        let t = schedule_tactic(TacticId::M01, &f, &policy()).unwrap();
        assert_eq!(
            (t.hold_end_frame, t.exit_start_frame, t.end_frame),
            (6 * BAR, 6 * BAR, 8 * BAR)
        );
        for placement in [0, 1] {
            let l = t
                .lanes
                .iter()
                .find(|l| l.placement == placement && l.lane == StemLane::Other)
                .unwrap();
            assert_eq!(
                curve_value(&l.gain, 6 * BAR),
                if placement == 0 { 1. } else { 0. }
            );
            assert!((curve_value(&l.gain, 7 * BAR) - 0.5).abs() < 1e-9);
        }
        assert_eq!(t.vocal_occupancy[0].end_frame, 6 * BAR + BAR / 2);
        assert_eq!(t.vocal_occupancy[1].start_frame, 7 * BAR);
        let drums = t
            .lanes
            .iter()
            .find(|l| l.placement == 1 && l.lane == StemLane::Drums)
            .unwrap();
        assert!(drums.gain.iter().all(|g| g.value == 1.));
    }
    #[test]
    fn mashup_cannot_replace_drums_midway_through_the_outgoing_phrase() {
        let mut f = dense();
        f.a.phrase_boundaries
            .retain(|b| b.output_frame != f.bar_frames[0]);
        let error = schedule_tactic(TacticId::M01, &f, &policy()).unwrap_err();
        assert!(error.message.contains("complete outgoing phrase"));
    }

    #[test]
    fn late_line_and_mid_phrase_incoming_gate_are_rejected() {
        let mut f = dense();
        f.a.vocal_phrases = Some(vec![vocal(0, 8 * BAR + 100)]);
        assert_eq!(
            schedule_tactic(TacticId::M01, &f, &policy())
                .unwrap_err()
                .code,
            "TACTIC_VOCALS"
        );
        f.a.vocal_phrases = Some(vec![vocal(0, 7 * BAR)]);
        f.b.vocal_phrases = Some(vec![vocal(6 * BAR, 9 * BAR)]);
        assert!(schedule_tactic(TacticId::M01, &f, &policy())
            .unwrap_err()
            .message
            .contains("mid-phrase"));
    }
    #[test]
    fn raw_track_intensity_does_not_establish_selected_mashup_density() {
        let mut f = dense();
        f.pair.selected_composite_density = None;
        assert!(schedule_tactic(TacticId::M01, &f, &policy())
            .unwrap_err()
            .message
            .contains("selected-layer density"));
        f.pair.selected_composite_density = Some(vec![0.8; 8]);
        f.a.stems = None;
        assert!(schedule_tactic(TacticId::M01, &f, &policy())
            .unwrap_err()
            .message
            .contains("stem isolation"));
    }
    #[test]
    fn unknown_vocals_are_not_treated_as_silence_and_low_scores_are_not_emergencies() {
        let mut f = facts(2);
        f.a.vocal_phrases = None;
        assert!(schedule_tactic(TacticId::S02, &f, &policy())
            .unwrap_err()
            .message
            .contains("vocal evidence is unknown"));
        f.a.vocal_phrases = Some(vec![]);
        for id in [TacticId::E01, TacticId::E02, TacticId::E03, TacticId::E04] {
            assert!(schedule_tactic(id, &f, &policy())
                .unwrap_err()
                .message
                .contains("not an emergency"));
        }
    }
    #[test]
    fn dense_seed_needs_an_endpoint_and_longer_quality_is_accumulated() {
        let mut seed = facts(8);
        seed.a.endpoint_reasons.clear();
        seed.b.endpoint_reasons.clear();
        assert!(select_tactics(&seed, &policy())
            .unwrap()
            .rejected
            .iter()
            .any(|r| r.tactic_id == TacticId::M01
                && r.reasons.iter().any(|s| s.contains("musical endpoint"))));
        assert_eq!(dense_quality_seconds(TacticId::M01, &seed), 0.);
        assert!(
            dense_quality_seconds(TacticId::M01, &facts(16))
                > dense_quality_seconds(TacticId::M01, &facts(8))
        );
        let mut poor = facts(16);
        poor.pair.lead_conflict = Some(0.24);
        assert_eq!(dense_quality_seconds(TacticId::M01, &poor), 0.);
    }

    #[test]
    fn supported_sustained_mix_outranks_isolating_one_collision_free_motif() {
        let mut held = facts(8);
        held.a.endpoint_reasons = vec!["measured rhythm change".into()];
        held.pair.complementarity = Some(0.70);
        held.pair.harmonic_distance = Some(0.25);
        held.pair.lead_conflict = Some(0.15);
        let mut motif = held.clone();
        motif.pair.complementarity = Some(1.);
        motif.pair.lead_conflict = Some(0.);
        assert!(
            preference(TacticId::M01, &held, &policy())
                > preference(TacticId::M04, &motif, &policy())
        );
        held.pair.complementarity = Some(0.50);
        held.pair.harmonic_distance = Some(0.50);
        assert!(
            preference(TacticId::M01, &held, &policy())
                < preference(TacticId::M04, &motif, &policy()),
            "duration must not outweigh a poor musical fit"
        );
    }

    #[test]
    fn long_mashup_keeps_the_supported_endpoint_and_its_vocal_line() {
        let mut f = facts(12);
        role_set(&mut f.a, &[TacticRole::Mixed]);
        role_set(&mut f.b, &[TacticRole::SteadyGroove]);
        f.a.vocal_phrases = Some(vec![vocal(9 * BAR, 11 * BAR)]);
        let t = schedule_tactic(TacticId::M01, &f, &policy()).unwrap();
        assert_eq!(t.hold_end_frame, 10 * BAR);
        assert_eq!(t.exit_start_frame, 10 * BAR);
        assert_eq!(t.end_frame, 12 * BAR);
        validate_compiled(&t, &f);
        assert!(t
            .vocal_occupancy
            .iter()
            .any(|v| v.placement == f.a.placement && v.end_frame == 11 * BAR));
        assert!(
            preference(TacticId::M01, &f, &policy())
                > preference(TacticId::M01, &facts(8), &policy())
        );
    }

    #[test]
    fn brisk_mashup_and_drum_linger_remain_distinct() {
        let mut f = facts(8);
        role_set(
            &mut f.a,
            &[TacticRole::SustainedTexture, TacticRole::LowPercussionBed],
        );
        role_set(&mut f.b, &[TacticRole::SteadyGroove]);
        let t = schedule_tactic(TacticId::M02, &f, &policy()).unwrap();
        assert_eq!(t.exit_start_frame, 7 * BAR);
        let t = schedule_tactic(TacticId::S05, &f, &policy()).unwrap();
        assert_eq!(t.exit_start_frame, 6 * BAR);
        f.b.bars[7].stability = Some(0.1);
        assert!(schedule_tactic(TacticId::M02, &f, &policy()).is_ok());
        f.b.bars[6].stability = Some(0.1);
        assert!(schedule_tactic(TacticId::M02, &f, &policy()).is_err());
    }
    #[test]
    fn echo_tail_counts_as_vocal_occupancy() {
        let mut f = facts(4);
        f.a.vocal_phrases = Some(vec![vocal(BAR, 2 * BAR)]);
        f.b.vocal_phrases = Some(vec![vocal(3 * BAR, 4 * BAR)]);
        assert!(schedule_tactic(TacticId::S09, &f, &policy()).is_err());
        f.b.vocal_phrases = Some(vec![]);
        let t = schedule_tactic(TacticId::S09, &f, &policy()).unwrap();
        assert_eq!(t.vocal_occupancy[0].tail_end_frame, 4 * BAR);
    }
    #[test]
    fn half_time_needs_pulse_hierarchy_and_gradual_tempo_needs_validated_maps() {
        let mut f = facts(2);
        f.pair.pulse_ratio = Some(2.);
        f.pair.groove_similarity = None;
        assert!(!schedule_tactic(TacticId::S02, &f, &policy())
            .unwrap()
            .modifiers
            .contains(&TacticId::D02));
        f.pair.groove_similarity = Some(0.95);
        f.pair.shared_bar_bpm = Some(vec![120., 121., 122.]);
        let t = schedule_tactic(TacticId::S02, &f, &policy()).unwrap();
        assert_eq!(t.modifiers, vec![TacticId::D01, TacticId::D02]);
        f.pair.tempo_maps_validated = false;
        assert!(schedule_tactic(TacticId::S02, &f, &policy())
            .unwrap()
            .modifiers
            .is_empty());
    }
    #[test]
    fn permissions_and_buffer_requirements_are_enforced() {
        let mut f = facts(4);
        role_set(&mut f.a, &[TacticRole::Build]);
        let mut p = policy();
        p.allow_expressive = false;
        assert!(schedule_tactic(TacticId::C09, &f, &p)
            .unwrap_err()
            .message
            .contains("not enabled"));
        p.emergency = Some(EmergencyActivation {
            reason: EmergencyReason::UnexpectedDecodeFailure,
            activated_output_frame: 0,
            failed_condition: "source decoder stopped".into(),
            rejected_safe_recoveries: vec!["no scheduled source available".into()],
            next_source_verified: true,
            healthy_buffer_frames: BAR,
        });
        let t = schedule_tactic(TacticId::E03, &f, &p).unwrap();
        assert!(matches!(
            t.source_edits[0].kind,
            SourceEditKind::Scratch { .. }
        ));
        f.a.ready_buffer_frames = 0;
        assert!(schedule_tactic(TacticId::E03, &f, &p).is_err());
    }
    #[test]
    fn fixed_loop_roll_and_gate_schedules_reject_tempo_drift() {
        for id in [TacticId::S12, TacticId::C05, TacticId::C06] {
            let mut f = facts(4);
            let t = schedule_tactic(id, &f, &policy()).unwrap();
            validate_compiled(&t, &f);
            if id == TacticId::C06 {
                assert_eq!(t.effects.len(), 2);
                assert!(t.effects.iter().all(|fx|matches!(fx.kind,EffectKind::Gate{period_frames,..} if period_frames==BAR/4)));
            }
            f.pair.shared_bar_bpm = Some(vec![120., 121., 122., 123., 124.]);
            assert!(
                schedule_tactic(id, &f, &policy())
                    .unwrap_err()
                    .message
                    .contains("steady output bar clock"),
                "{id:?}"
            );
            assert!(
                candidate_tactic_schedule(id, &f, &policy()).is_err(),
                "{id:?} pair profile must not model unsupported tempo drift"
            );
            f.pair.shared_bar_bpm = None;
            f.bar_frames[2] += 1000;
            for passage in [&mut f.a, &mut f.b] {
                passage.bars[1].end_frame += 1000;
                passage.bars[2].start_frame += 1000;
            }
            assert!(
                schedule_tactic(id, &f, &policy())
                    .unwrap_err()
                    .message
                    .contains("steady output bar clock"),
                "{id:?} missing D01 metadata must not hide a changing clock"
            );
        }
    }
    #[test]
    fn polyrhythm_rejects_changing_clocks_and_partial_output_cycles() {
        let mut f = facts(3);
        role_set(&mut f.a, &[TacticRole::SteadyGroove]);
        let t = schedule_tactic(TacticId::C13, &f, &policy()).unwrap();
        let edit = &t.source_edits[0];
        let SourceEditKind::Loop { period_frames } = edit.kind else {
            panic!("expected fixed percussion loop")
        };
        assert_eq!(period_frames, 3 * BAR / 4);
        assert_eq!(
            (edit.output_end_frame - edit.output_start_frame) % period_frames,
            0
        );
        validate_compiled(&t, &f);
        // Both bar clocks still have twelve beats, but the second bar is longer
        // and the next shorter. Count % 3 previously admitted this drift.
        f.bar_frames[2] += 1000;
        for passage in [&mut f.a, &mut f.b] {
            passage.bars[1].end_frame += 1000;
            passage.bars[2].start_frame += 1000;
        }
        assert!(eligibility(TacticId::C13, &f, &policy())
            .iter()
            .any(|r| r.contains("whole output cycles")));
        assert!(candidate_tactic_schedule(TacticId::C13, &f, &policy()).is_err());
        let mut f = facts(3);
        role_set(&mut f.a, &[TacticRole::SteadyGroove]);
        f.pair.shared_bar_bpm = Some(vec![120., 121., 122., 123.]);
        assert!(schedule_tactic(TacticId::C13, &f, &policy()).is_err());
        // A rounding remainder must not silently become a truncated final loop.
        f.pair.shared_bar_bpm = None;
        *f.bar_frames.last_mut().unwrap() += 1;
        f.a.bars.last_mut().unwrap().end_frame += 1;
        f.b.bars.last_mut().unwrap().end_frame += 1;
        assert!(polyrhythm_period(&f).is_none());
    }
    #[test]
    fn every_family_emits_its_declared_executable_operation() {
        use TacticId::*;
        use TacticRole as R;
        for id in TacticId::ALL
            .into_iter()
            .filter(|id| !id.is_modifier() && id.permission() != TacticPermission::EmergencyOnly)
        {
            let mut f = facts(if id == S02 {
                2
            } else if id == C13 {
                12
            } else {
                8
            });
            role_set(
                &mut f.a,
                &[
                    R::SustainedTexture,
                    R::LowPercussionBed,
                    R::Outro,
                    R::Build,
                    R::Drop,
                    R::DrumReturn,
                    R::SteadyGroove,
                ],
            );
            role_set(
                &mut f.b,
                &[R::SteadyGroove, R::Intro, R::Drop, R::Breakdown],
            );
            if matches!(id, M05 | S06) {
                f.a.arrival_frame = Some(0);
            }
            if id == M05 {
                f.b.arrival_frame = Some(0);
            }
            if id == C03 {
                f.a.build_release_frame = Some(0);
            }
            if id == M03 {
                f.a.vocal_phrases = Some(vec![vocal(0, 3 * BAR)]);
            }
            if id == M06 {
                f.a.vocal_phrases = Some(vec![vocal(0, BAR)]);
                f.b.vocal_phrases = Some(vec![vocal(2 * BAR, 3 * BAR)]);
            }
            let t = schedule_tactic(id, &f, &policy())
                .unwrap_or_else(|e| panic!("{id:?}: {} {}", e.code, e.message));
            validate_compiled(&t, &f);
            assert!(!t.lanes.is_empty(), "{id:?}");
            match id {
                S08 | S09 | S10 | S11 | C06 | C09 | C10 => assert!(!t.effects.is_empty(), "{id:?}"),
                S12 | C05 | C08 | C11 | C12 | C13 | C14 => {
                    assert!(!t.source_edits.is_empty(), "{id:?}")
                }
                S01 | S03 | S04 => assert!(t.lanes.iter().any(|l| !l.eq.is_empty()), "{id:?}"),
                _ => {}
            }
        }
    }
    #[test]
    fn selector_accounts_for_all_families_deterministically() {
        let f = dense();
        let a = select_tactics(&f, &policy()).unwrap();
        let b = select_tactics(&f, &policy()).unwrap();
        assert_eq!(a.candidates.len() + a.rejected.len(), 37);
        assert_eq!(a.candidates[0].plan.tactic_id, TacticId::M01);
        assert_eq!(
            serde_json::to_value(a).unwrap(),
            serde_json::to_value(b).unwrap()
        );
    }

    #[test]
    fn repeated_vocals_use_rendered_occupancy_and_guard_every_repetition() {
        let mut f = facts(8);
        f.a.vocal_phrases = Some(vec![vocal(BAR / 4, BAR / 2)]);
        f.a.loop_candidates[0].vocal_free = false;
        f.a.loop_candidates[0].contains_complete_language = Some(true);
        let t = schedule_tactic(TacticId::S12, &f, &policy()).unwrap();
        assert_eq!(t.vocal_occupancy.len(), 8);
        assert_eq!(t.vocal_occupancy[7].start_frame, 7 * BAR + BAR / 4);
        f.b.vocal_phrases = Some(vec![vocal(7 * BAR + BAR / 3, 7 * BAR + 3 * BAR / 4)]);
        assert_eq!(
            schedule_tactic(TacticId::S12, &f, &policy())
                .unwrap_err()
                .code,
            "TACTIC_VOCALS"
        );
    }

    #[test]
    fn vocal_tease_uses_complete_capture_at_its_new_time() {
        let mut f = facts(8);
        let motif = f.b.motif.as_mut().unwrap();
        motif.lane = StemLane::Vocals;
        motif.source_start_frame = 2 * BAR;
        motif.source_end_frame = 3 * BAR;
        f.b.vocal_phrases = Some(vec![vocal(2 * BAR + BAR / 4, 2 * BAR + BAR / 2)]);
        let c = &mut f.b.loop_candidates[0];
        c.lane = StemLane::Vocals;
        c.source_start_frame = 2 * BAR;
        c.source_end_frame = 3 * BAR;
        c.output_start_frame = 2 * BAR;
        c.output_end_frame = 3 * BAR;
        c.vocal_free = false;
        c.contains_complete_language = Some(true);
        let t = schedule_tactic(TacticId::C11, &f, &policy()).unwrap();
        assert_eq!(t.vocal_occupancy.len(), 1);
        assert_eq!(t.vocal_occupancy[0].start_frame, BAR / 4);
        f.a.vocal_phrases = Some(vec![vocal(BAR / 3, 3 * BAR / 4)]);
        assert!(schedule_tactic(TacticId::C11, &f, &policy()).is_err());
    }

    #[test]
    fn ambient_bridge_does_not_invent_a_meter_and_loop_bridge_uses_separate_pulses() {
        let mut f = facts(8);
        f.timing_basis = TacticTimingBasis::Structural;
        f.bar_frames = vec![0, 8 * BAR];
        f.a.bars.clear();
        f.b.bars.clear();
        f.pair.beat_skew_ms = None;
        role_set(
            &mut f.a,
            &[TacticRole::SustainedTexture, TacticRole::LowPercussionBed],
        );
        role_set(&mut f.b, &[TacticRole::Intro]);
        let t = schedule_tactic(TacticId::S07, &f, &policy()).unwrap();
        assert_eq!(t.timing_basis, TacticTimingBasis::Structural);
        assert!(t.modifiers.is_empty());
        assert!(schedule_tactic(TacticId::S02, &f, &policy()).is_err());
        let mut f = facts(8);
        f.pair.beat_skew_ms = None;
        assert!(schedule_tactic(TacticId::C14, &f, &policy()).is_ok());
    }

    #[test]
    fn three_band_order_uses_measured_space_and_retains_complementary_ownership() {
        let mut f = facts(8);
        for (a, b) in f.a.bars.iter_mut().zip(&mut f.b.bars) {
            a.low_occupancy = Some(0.9);
            a.mid_occupancy = Some(0.6);
            a.high_occupancy = Some(0.05);
            b.low_occupancy = Some(0.2);
            b.mid_occupancy = Some(0.2);
            b.high_occupancy = Some(0.8);
        }
        let high_first = schedule_tactic(TacticId::S04, &f, &policy()).unwrap();
        assert_eq!(high_first.lanes[1].eq[1].gains, [0., 0., 1.]);
        assert_eq!(high_first.lanes[1].eq[2].gains, [0., 1., 1.]);
        for (a, b) in high_first.lanes[0].eq.iter().zip(&high_first.lanes[1].eq) {
            assert_eq!(a.output_frame, b.output_frame);
            for band in 0..3 {
                assert_eq!(a.gains[band] + b.gains[band], 1.);
            }
        }
        for (a, b) in f.a.bars.iter_mut().zip(&mut f.b.bars) {
            a.low_occupancy = Some(0.05);
            a.high_occupancy = Some(0.9);
            b.low_occupancy = Some(0.8);
            b.high_occupancy = Some(0.2);
        }
        let low_first = schedule_tactic(TacticId::S04, &f, &policy()).unwrap();
        assert_eq!(low_first.lanes[1].eq[1].gains, [1., 0., 0.]);
        validate_compiled(&low_first, &f);
        f.a.bars[3].high_occupancy = None;
        assert!(schedule_tactic(TacticId::S04, &f, &policy()).is_err());
    }

    #[test]
    fn stereo_band_exchange_is_a_fallback_without_demoting_successful_stem_tactics() {
        let mut f = facts(8);
        role_set(&mut f.a, &[TacticRole::Mixed, TacticRole::DrumReturn]);
        role_set(&mut f.b, &[TacticRole::SteadyGroove]);
        f.a.arrival_frame = Some(f.bar_frames[0]);
        let selected = select_tactics(&f, &policy()).unwrap();
        let band = selected
            .candidates
            .iter()
            .find(|c| c.plan.tactic_id == TacticId::S04)
            .unwrap();
        assert!(band.last_resort);
        assert!(band
            .plan
            .selection_reason
            .contains("negative listening feedback"));
        for id in [TacticId::M01, TacticId::S06, TacticId::S03] {
            let normal = selected
                .candidates
                .iter()
                .find(|c| c.plan.tactic_id == id)
                .unwrap_or_else(|| panic!("{id:?} missing: {:?}", selected.rejected));
            assert!(!normal.last_resort, "{id:?} remains a normal tactic");
            assert!(normal.score > band.score);
        }
        assert!(
            band.score > preference(TacticId::C01, &f, &policy()),
            "an unmatched cut remains below an admitted band exchange"
        );
        f.pair.short_fade_match = Some(ShortFadeMatch::AllBandsMatch);
        assert!(
            last_resort(TacticId::S04, &f, &policy()),
            "a matching statistic cannot erase the listening policy"
        );
    }

    #[test]
    fn low_band_energy_penalizes_but_does_not_veto_a_compatible_exchange() {
        let mut f = facts(4);
        let baseline = preference(TacticId::S04, &f, &policy());
        for bar in f.a.bars.iter_mut().chain(&mut f.b.bars) {
            bar.low_occupancy = Some(0.82);
            bar.mid_occupancy = Some(0.14);
            bar.high_occupancy = Some(0.04);
        }
        schedule_tactic(TacticId::S04, &f, &policy()).unwrap();
        assert!(preference(TacticId::S04, &f, &policy()) < baseline);
        f.pair.lead_conflict = Some(0.8);
        assert!(schedule_tactic(TacticId::S04, &f, &policy()).is_err());
        f.pair.lead_conflict = Some(0.1);
        f.pair.complementarity = None;
        assert!(schedule_tactic(TacticId::S04, &f, &policy()).is_err());
    }

    #[test]
    fn one_bar_bass_swap_cannot_bypass_the_short_fade_policy() {
        let mut f = facts(1);
        let p = policy();
        assert!(last_resort(TacticId::S03, &f, &p));
        assert!(preference(TacticId::S03, &f, &p) < preference(TacticId::S03, &facts(4), &p));
        f.pair.short_fade_match = Some(ShortFadeMatch::OneBandDifference);
        assert!(!last_resort(TacticId::S03, &f, &p));
        assert!(last_resort(TacticId::S03, &facts(2), &p));
        assert!(last_resort(TacticId::S03, &facts(3), &p));
        assert!(!last_resort(TacticId::S03, &facts(6), &p));
    }

    #[test]
    fn zero_overlap_clash_does_not_promote_unmatched_cuts() {
        let mut cut = facts(2);
        cut.pair.complementarity = Some(1.);
        cut.pair.harmonic_distance = Some(0.);
        cut.pair.lead_conflict = Some(0.);
        let p = policy();
        let blend = facts(4);
        for id in [
            TacticId::C01,
            TacticId::C02,
            TacticId::C03,
            TacticId::C04,
            TacticId::C07,
        ] {
            assert!(last_resort(id, &cut, &p));
            assert!(preference(id, &cut, &p) < preference(TacticId::S04, &blend, &p));
        }
        // The one-band fade exception cannot promote a full-band cut.
        cut.pair.short_fade_match = Some(ShortFadeMatch::OneBandDifference);
        assert!(last_resort(TacticId::C01, &cut, &p));
        cut.pair.cut_band_matches.insert(TacticId::C01);
        assert!(last_resort(TacticId::C03, &cut, &p));
        assert!(!last_resort(TacticId::C01, &cut, &p));
        let selected = select_tactics(&cut, &p).unwrap();
        assert!(selected
            .candidates
            .iter()
            .filter(|c| c.plan.tactic_id == TacticId::C01)
            .all(|c| !c.last_resort));
    }

    #[test]
    fn a_clean_short_fade_can_outrank_a_marginal_band_exchange() {
        let mut crowded = facts(4);
        crowded.pair.complementarity = Some(0.66);
        crowded.pair.harmonic_distance = Some(0.28);
        crowded.pair.lead_conflict = Some(0.22);
        let mut clean = facts(1);
        clean.pair.complementarity = Some(0.91);
        clean.pair.harmonic_distance = Some(0.09);
        clean.pair.lead_conflict = Some(0.04);
        let policy = policy();
        let band = preference(TacticId::S04, &crowded, &policy);
        assert!(preference(TacticId::S02, &clean, &policy) < band);
        clean.pair.short_fade_match = Some(ShortFadeMatch::QuietNearMatch);
        assert!(preference(TacticId::S02, &clean, &policy) > band);
        clean.pair.short_fade_match = Some(ShortFadeMatch::OneBandDifference);
        assert!(preference(TacticId::S02, &clean, &policy) > band);
        clean.pair.harmonic_distance = Some(0.9);
        assert!(preference(TacticId::S02, &clean, &policy) < band);
    }

    #[test]
    fn build_to_drop_keeps_the_crescendo_until_the_release() {
        let mut f = facts(2);
        role_set(&mut f.a, &[TacticRole::Build]);
        role_set(&mut f.b, &[TacticRole::Drop]);
        f.a.build_release_frame = Some(BAR);
        f.b.arrival_frame = Some(BAR / 2);
        assert!(schedule_tactic(TacticId::S02, &f, &policy())
            .unwrap_err()
            .message
            .contains("buildup"));
        assert!(schedule_tactic(TacticId::C03, &f, &policy())
            .unwrap_err()
            .message
            .contains("buildup"));
        f.b.arrival_frame = Some(BAR);
        let cut = schedule_tactic(TacticId::C03, &f, &policy()).unwrap();
        assert_eq!(cut.exit_start_frame, BAR);
        assert_eq!(cut.hold_end_frame, BAR);
        assert_eq!(curve_value(&cut.lanes[0].gain, BAR), 1.);
        assert_eq!(curve_value(&cut.lanes[1].gain, BAR), 0.);
    }

    #[test]
    fn measured_cut_ignores_muted_lines_and_requires_a_gap_on_both_decks() {
        let mut f = facts(2);
        f.a.phrase_boundaries.clear();
        f.b.phrase_boundaries.clear();
        f.b.arrival_frame = None;
        f.b.cut_attacks = vec![BAR];
        f.a.vocal_phrases = None;
        f.b.vocal_phrases = None;
        let gap = [BAR - u64::from(RATE) / 4, BAR + u64::from(RATE) / 4];
        f.a.vocal_quiet = Some(vec![gap]);
        f.b.vocal_quiet = Some(vec![gap]);
        let cut = schedule_tactic(TacticId::C01, &f, &policy()).unwrap();
        assert_eq!(cut.hold_end_frame, BAR);
        assert_eq!(cut.vocal_occupancy.len(), 2);
        assert_eq!(cut.vocal_occupancy[0].end_frame, gap[0]);
        assert_eq!(cut.vocal_occupancy[1].start_frame, gap[1]);
        f.b.arrival_frame = Some(BAR);
        f.b.vocal_quiet = Some(vec![]);
        f.a.phrase_boundaries = facts(2).a.phrase_boundaries;
        f.b.phrase_boundaries = facts(2).b.phrase_boundaries;
        assert!(schedule_tactic(TacticId::C01, &f, &policy())
            .unwrap_err()
            .message
            .contains("between-lines gap"));
    }

    #[test]
    fn dry_fade_rejects_possible_vocals_on_both_audible_decks() {
        let mut f = facts(2);
        f.a.vocal_phrases = None;
        f.b.vocal_phrases = None;
        f.a.vocal_quiet = Some(vec![[2 * BAR - u64::from(RATE), 2 * BAR + u64::from(RATE)]]);
        f.b.vocal_quiet = Some(vec![[0, u64::from(RATE)]]);
        assert!(schedule_tactic(TacticId::S02, &f, &policy())
            .unwrap_err()
            .message
            .contains("vocals overlap"));
        f.a.vocal_quiet = Some(vec![[0, 2 * BAR + u64::from(RATE)]]);
        assert!(schedule_tactic(TacticId::S02, &f, &policy()).is_ok());
    }
}
