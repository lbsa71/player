//! Source-time segment measurements, independent of any proposed mix.
use crate::*;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SegmentAnalysis {
    #[serde(flatten)]
    pub header: Header,
    pub segments_id: String,
    pub source_sample_rate_hz: u32,
    pub source_frames: u64,
    pub method: String,
    pub score_semantics: String,
    pub tiles: Vec<SegmentTile>,
    pub boundaries: Vec<StructuralBoundary>,
    pub unavailable: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SegmentTile {
    pub start_frame: u64,
    pub end_frame: u64,
    pub bars: u32,
    pub pulses_per_bar: u32,
    pub pulse_score_min: f64,
    pub local_bpm: f64,
    pub rms: f64,
    pub peak: f64,
    pub crest_factor_db: f64,
    pub band_occupancy: [f64; 3],
    pub onset_rate_hz: f64,
    pub chroma: [f64; 12],
    pub level_variation: f64,
    pub harmonic_variation: f64,
    /// Acoustic role evidence, not calibrated probabilities or vocal labels.
    pub roles: BTreeMap<String, f64>,
    /// These must remain unknown until a suitable analysis provider runs.
    pub vocal_activity: Option<f64>,
    pub separation_quality: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct StructuralBoundary {
    pub source_frame: u64,
    pub novelty: f64,
    pub supported_period_bars: Option<u32>,
    pub repeated_support: u32,
    pub evidence: String,
}

impl Record for SegmentAnalysis {
    fn validate(&self) -> Result<()> {
        valid_id(&self.segments_id)?;
        require(
            self.source_sample_rate_hz > 0
                && self.source_frames > 0
                && self.source_frames <= MAX_FRAME
                && !self.method.is_empty(),
            "SEGMENT_FORMAT",
            "invalid segment timebase or method",
        )?;
        let mut previous = None;
        for tile in &self.tiles {
            bounds(tile.start_frame, tile.end_frame, self.source_frames)?;
            require(
                (1..=4).contains(&tile.bars)
                    && (2..=12).contains(&tile.pulses_per_bar)
                    && previous.is_none_or(|p| p < (tile.start_frame, tile.bars)),
                "SEGMENT_ORDER",
                "segment tiles must be unique and ordered by start and span",
            )?;
            previous = Some((tile.start_frame, tile.bars));
            for value in [
                tile.rms,
                tile.peak,
                tile.onset_rate_hz,
                tile.level_variation,
                tile.harmonic_variation,
                tile.local_bpm,
            ] {
                require(
                    value.is_finite() && value >= 0.,
                    "SEGMENT_VALUE",
                    "invalid segment measurement",
                )?;
            }
            require(
                tile.crest_factor_db.is_finite() && tile.local_bpm > 0.,
                "SEGMENT_VALUE",
                "invalid crest or tempo",
            )?;
            for value in tile
                .band_occupancy
                .iter()
                .chain(&tile.chroma)
                .chain(tile.roles.values())
                .chain([&tile.pulse_score_min])
                .chain(tile.vocal_activity.iter())
                .chain(tile.separation_quality.iter())
            {
                require(
                    value.is_finite() && (0.0..=1.).contains(value),
                    "SEGMENT_SCORE",
                    "segment evidence score is outside 0..1",
                )?;
            }
        }
        let mut previous = None;
        for boundary in &self.boundaries {
            require(
                boundary.source_frame < self.source_frames
                    && previous.is_none_or(|p| p < boundary.source_frame)
                    && boundary.novelty.is_finite()
                    && (0.0..=1.).contains(&boundary.novelty)
                    && boundary
                        .supported_period_bars
                        .is_none_or(|n| [4, 8, 16, 32].contains(&n))
                    && (boundary.supported_period_bars.is_none() || boundary.repeated_support >= 2)
                    && !boundary.evidence.is_empty(),
                "SEGMENT_BOUNDARY",
                "invalid structural boundary evidence",
            )?;
            previous = Some(boundary.source_frame);
        }
        Ok(())
    }
}
