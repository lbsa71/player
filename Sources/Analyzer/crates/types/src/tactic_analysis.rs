//! Immutable source-aligned analysis and binary stems for tactic selection.
use crate::*;

pub const TACTIC_ANALYSIS_METHOD: &str = "full-track-stems-phrases-signals/1";

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TacticTrackAnalysis {
    #[serde(flatten)]
    pub header: Header,
    pub tactic_analysis_id: String,
    pub track_id: String,
    pub audio_hash: ContentHash,
    pub audio_revision: u64,
    pub source_sample_rate_hz: u32,
    pub source_frames: u64,
    /// Original generation used for analysis, before this supplementary record.
    pub base_analysis: FileRef,
    pub beat_artifact: FileRef,
    pub override_revision: u64,
    pub method: String,
    pub implementation_hash: ContentHash,
    /// Script and pipeline hashes allow dependency-specific reuse. Missing on
    /// the first implementation; legacy reuse requires its exact aggregate hash.
    #[serde(default)]
    pub component_hashes: BTreeMap<String, ContentHash>,
    pub cache_key: ContentHash,
    pub models: BTreeMap<String, ModelAsset>,
    /// All stems retain original frame zero, full length and source sample rate.
    pub stems: BTreeMap<StemLane, FileRef>,
    pub vocal_evidence: Value,
    pub signal_evidence: Value,
    pub limitations: Vec<String>,
    pub created_at: String,
}

impl Record for TacticTrackAnalysis {
    const MAX_JSON_BYTES: u64 = MAX_TACTIC_JSON;
    fn validate(&self) -> Result<()> {
        valid_id(&self.tactic_analysis_id)?;
        valid_id(&self.track_id)?;
        require(
            self.source_sample_rate_hz > 0
                && self.source_frames > 0
                && self.source_frames <= MAX_FRAME
                && self.method == TACTIC_ANALYSIS_METHOD,
            "TACTIC_ANALYSIS_FORMAT",
            "invalid tactic analysis identity or source timebase",
        )?;
        require(
            self.stems.len() == 4
                && [
                    StemLane::Drums,
                    StemLane::Bass,
                    StemLane::Other,
                    StemLane::Vocals,
                ]
                .iter()
                .all(|lane| self.stems.contains_key(lane)),
            "TACTIC_ANALYSIS_STEMS",
            "full-track analysis requires four explicit source-aligned stems",
        )?;
        require(
            self.models.contains_key("stem_htdemucs")
                && self.models.contains_key("vocal_phrase_whisper"),
            "TACTIC_ANALYSIS_MODELS",
            "separator and phrase model provenance are required",
        )?;
        require(
            self.signal_evidence.is_object()
                && self.vocal_evidence.is_object()
                && self
                    .signal_evidence
                    .get("method")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty())
                && self
                    .vocal_evidence
                    .get("method")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty()),
            "TACTIC_ANALYSIS_EVIDENCE",
            "versioned signal and vocal evidence are required",
        )?;
        require(
            self.signal_evidence
                .get("sample_rate_hz")
                .and_then(Value::as_u64)
                == Some(u64::from(self.source_sample_rate_hz))
                && self
                    .signal_evidence
                    .get("source_start_frame")
                    .and_then(Value::as_u64)
                    == Some(0)
                && self
                    .signal_evidence
                    .get("source_end_frame")
                    .and_then(Value::as_u64)
                    == Some(self.source_frames),
            "TACTIC_ANALYSIS_TIMEBASE",
            "signal evidence must retain exact source frame coordinates",
        )?;
        Ok(())
    }
}
