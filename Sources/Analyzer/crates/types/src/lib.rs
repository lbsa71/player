//! Portable contracts. JSON authority, schema validation and semantic invariants live here.
mod tactics;
pub use tactics::*;
mod segments;
pub use segments::*;
mod tactic_analysis;
pub use tactic_analysis::*;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    any::TypeId,
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
};

pub const MAX_FRAME: u64 = (1u64 << 53) - 1;
pub const MAX_JSON: u64 = 8 * 1024 * 1024;
/// Full-track evidence is bounded separately from small catalog/control records.
pub const MAX_TACTIC_JSON: u64 = 32 * 1024 * 1024;
pub fn logical_json_limit(path: &str) -> u64 {
    if path.rsplit('/').next() == Some("tactic-analysis.json") {
        MAX_TACTIC_JSON
    } else {
        MAX_JSON
    }
}
pub const ENGINE: &str = "mixlib/0.1.0";
pub type Extensions = BTreeMap<String, Value>;
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, Serialize)]
pub struct Error {
    pub code: String,
    pub message: String,
}
impl Error {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::new("IO", e.to_string())
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::new("JSON", e.to_string())
    }
}
pub fn require(ok: bool, code: &str, message: impl Into<String>) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(Error::new(code, message))
    }
}
pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
#[derive(Clone, Default)]
pub struct Cancellation(pub Arc<AtomicBool>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn check(&self) -> Result<()> {
        require(
            !self.0.load(Ordering::Relaxed),
            "CANCELLED",
            "operation cancelled at a safe boundary",
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ContentHash {
    pub algorithm: HashAlgorithm,
    pub digest: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HashAlgorithm {
    Sha256,
}
pub fn hash_bytes(bytes: &[u8]) -> ContentHash {
    ContentHash {
        algorithm: HashAlgorithm::Sha256,
        digest: format!("{:x}", Sha256::digest(bytes)),
    }
}
/// Canonical mixlib-json-v1: UTF-8, recursively sorted object keys (serde_json BTreeMap),
/// no whitespace, serde_json shortest-roundtrip finite number encoding, array order retained.
pub fn canonical_hash<T: Serialize>(value: &T) -> Result<ContentHash> {
    reject_non_finite(value)?;
    Ok(hash_bytes(&serde_json::to_vec(&serde_json::to_value(
        value,
    )?)?))
}
fn reject_non_finite<T: Serialize>(value: &T) -> Result<()> {
    fn visit(value: &serde_value::Value) -> Result<()> {
        use serde_value::Value;
        match value {
            Value::F32(x) => finite(*x as f64)?,
            Value::F64(x) => finite(*x)?,
            Value::Seq(values) => {
                for v in values {
                    visit(v)?;
                }
            }
            Value::Map(values) => {
                for (key, value) in values {
                    visit(key)?;
                    visit(value)?;
                }
            }
            Value::Option(Some(v)) | Value::Newtype(v) => visit(v)?,
            _ => {}
        }
        Ok(())
    }
    visit(&serde_value::to_value(value).map_err(|e| Error::new("JSON", e.to_string()))?)
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct FileRef {
    pub path: String,
    pub hash: ContentHash,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Header {
    #[schemars(range(min = 1, max = 1))]
    pub schema_version: u32,
    #[serde(default)]
    pub extensions: Extensions,
}
impl Default for Header {
    fn default() -> Self {
        Self {
            schema_version: 1,
            extensions: Extensions::new(),
        }
    }
}
pub trait Record: Serialize + for<'de> Deserialize<'de> + JsonSchema + 'static {
    const MAX_JSON_BYTES: u64 = MAX_JSON;
    fn validate(&self) -> Result<()> {
        Ok(())
    }
}
pub fn schema<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("schema serializes")
}
pub fn validate_value<T: Record>(value: &Value) -> Result<T> {
    require(
        value.get("schema_version").and_then(Value::as_u64) == Some(1),
        "SCHEMA_VERSION",
        "only schema major version 1 is supported",
    )?;
    static VALIDATORS: OnceLock<
        Mutex<std::collections::HashMap<TypeId, Arc<jsonschema::Validator>>>,
    > = OnceLock::new();
    let validator = {
        let mut cache = VALIDATORS
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| Error::new("SCHEMA", "schema cache lock poisoned"))?;
        if let Some(validator) = cache.get(&TypeId::of::<T>()) {
            validator.clone()
        } else {
            let validator = Arc::new(
                jsonschema::validator_for(&schema::<T>())
                    .map_err(|e| Error::new("SCHEMA", e.to_string()))?,
            );
            cache.insert(TypeId::of::<T>(), validator.clone());
            validator
        }
    };
    validator
        .validate(value)
        .map_err(|e| Error::new("SCHEMA", e.to_string()))?;
    let result: T = serde_json::from_value(value.clone())?;
    result.validate()?;
    Ok(result)
}
pub fn encode<T: Record>(record: &T) -> Result<Vec<u8>> {
    reject_non_finite(record)?;
    record.validate()?;
    let value = serde_json::to_value(record)?;
    validate_value::<T>(&value)?;
    let bytes = serde_json::to_vec_pretty(&value)?;
    require(
        bytes.len() as u64 <= T::MAX_JSON_BYTES,
        "RECORD_TOO_LARGE",
        format!(
            "JSON record exceeds {} MiB",
            T::MAX_JSON_BYTES / (1024 * 1024)
        ),
    )?;
    Ok(bytes)
}
pub fn valid_id(id: &str) -> Result<()> {
    require(
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "INVALID_ID",
        "IDs must contain only ASCII letters, numbers, '-' or '_'",
    )
}
pub fn finite(x: f64) -> Result<()> {
    require(x.is_finite(), "NON_FINITE", "non-finite number")
}
pub fn bounds(start: u64, end: u64, limit: u64) -> Result<()> {
    require(
        start < end && end <= limit && limit <= MAX_FRAME,
        "FRAME_RANGE",
        "invalid or unsafe frame range",
    )
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SourceRoot {
    pub root_id: String,
    pub display_name: String,
    pub location_kind: String,
    pub path: String,
    pub available: bool,
    #[serde(default)]
    pub require_finalized_sidecar: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Library {
    #[serde(flatten)]
    pub header: Header,
    pub library_id: String,
    #[serde(default)]
    pub metadata_layout: MetadataLayout,
    pub source_roots: Vec<SourceRoot>,
    pub configuration_refs: BTreeMap<String, String>,
    pub excluded_directories: Vec<String>,
    pub created_at: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MetadataLayout {
    #[default]
    SeparateV1,
    SidecarV1,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SidecarLocation {
    pub root_id: String,
    pub relative_wav_path: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SidecarIndex {
    #[serde(flatten)]
    pub header: Header,
    pub index_id: String,
    pub tracks: BTreeMap<String, SidecarLocation>,
}
impl Record for SidecarIndex {}
/// Stored under the reserved `analyzer` field of the recorder's existing JSON.
/// Record keys retain the logical paths used by immutable FileRefs; they are not files.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SidecarBundle {
    #[serde(flatten)]
    pub header: Header,
    pub bundle_id: String,
    pub library_id: String,
    pub track_id: String,
    pub shared_from: Option<SidecarLocation>,
    pub records: BTreeMap<String, Value>,
}
impl Record for SidecarBundle {
    fn validate(&self) -> Result<()> {
        valid_id(&self.track_id)?;
        valid_id(&self.library_id)?;
        valid_id(&self.bundle_id)?;
        require(
            self.shared_from.is_none() || self.records.is_empty(),
            "SIDECAR_ALIAS",
            "shared sidecars cannot own competing records",
        )
    }
}
impl Record for Library {
    fn validate(&self) -> Result<()> {
        valid_id(&self.library_id)?;
        let mut roots = BTreeSet::new();
        for r in &self.source_roots {
            valid_id(&r.root_id)?;
            require(roots.insert(&r.root_id), "DUPLICATE_ROOT", &r.root_id)?;
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    PcmInteger,
    IeeeFloat,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AudioFormat {
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub encoding: Encoding,
    pub container_bits: u16,
    pub valid_bits: u16,
    pub channel_mask: u32,
    pub frames: u64,
}
impl AudioFormat {
    pub fn validate(&self) -> Result<()> {
        require(
            (8000..=192000).contains(&self.sample_rate_hz) && (1..=2).contains(&self.channels),
            "WAV_FORMAT",
            "supported: mono/stereo, 8–192 kHz",
        )?;
        require(
            self.frames > 0
                && self.frames <= MAX_FRAME
                && self.valid_bits > 0
                && self.valid_bits <= self.container_bits,
            "WAV_FORMAT",
            "invalid frame count or bit depth",
        )
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct Fingerprint {
    pub size: u64,
    pub modified_ns: String,
    pub file_identity: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SourceLocation {
    pub root_id: String,
    pub relative_path: String,
    pub fingerprint: Fingerprint,
    pub file_hash: ContentHash,
    pub available: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Available,
    Unavailable,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Track {
    #[serde(flatten)]
    pub header: Header,
    pub track_id: String,
    pub audio_revision: u64,
    pub audio_hash: ContentHash,
    pub format: AudioFormat,
    pub source_locations: Vec<SourceLocation>,
    pub active_analysis: Option<FileRef>,
    #[serde(default)]
    pub active_overrides: Option<FileRef>,
    pub override_revision: u64,
    pub lifecycle: Lifecycle,
}
impl Record for Track {
    fn validate(&self) -> Result<()> {
        valid_id(&self.track_id)?;
        self.format.validate()?;
        require(
            !self.source_locations.is_empty(),
            "SOURCE_LOCATION",
            "track has no source locations",
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
    Deferred,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Job {
    #[serde(flatten)]
    pub header: Header,
    pub job_id: String,
    pub track_id: String,
    pub audio_hash: ContentHash,
    pub state: JobState,
    pub stage: String,
    pub retry_count: u32,
    pub diagnostic: Option<String>,
    pub updated_at: String,
}
impl Record for Job {}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Diagnostic {
    pub code: String,
    pub subject: String,
    pub message: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Report {
    #[serde(flatten)]
    pub header: Header,
    pub report_id: String,
    pub kind: String,
    pub counts: BTreeMap<String, u64>,
    pub diagnostics: Vec<Diagnostic>,
    pub created_at: String,
}
impl Report {
    pub fn new(kind: &str) -> Self {
        Self {
            header: Header::default(),
            report_id: id(),
            kind: kind.into(),
            counts: BTreeMap::new(),
            diagnostics: vec![],
            created_at: now(),
        }
    }
    pub fn count(&mut self, k: &str) {
        *self.counts.entry(k.into()).or_default() += 1;
    }
    pub fn error(&mut self, subject: &str, e: &Error) {
        self.diagnostics.push(Diagnostic {
            code: e.code.clone(),
            subject: subject.into(),
            message: e.message.clone(),
        });
    }
}
impl Record for Report {}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BeatBackend {
    BeatThis,
    Unavailable,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ModelAsset {
    pub model_id: String,
    pub file: FileRef,
    pub upstream_url: String,
    pub revision: String,
    pub code_license: String,
    pub weight_license: String,
    pub attribution: String,
    pub redistribution: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ModelManifest {
    #[serde(flatten)]
    pub header: Header,
    pub manifest_id: String,
    pub models: Vec<ModelAsset>,
}
impl Record for ModelManifest {}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct AnalysisConfig {
    #[serde(flatten)]
    pub header: Header,
    pub config_id: String,
    pub beat_backend: BeatBackend,
    pub settling_ms: u64,
    pub max_duration_seconds: u32,
    pub feature_hop_ms: u32,
    pub low_crossover_hz: f64,
    pub high_crossover_hz: f64,
}
impl Default for AnalysisConfig {
    fn default() -> Self {
        Self {
            header: Header::default(),
            config_id: "analysis".into(),
            beat_backend: BeatBackend::BeatThis,
            settling_ms: 500,
            max_duration_seconds: 7200,
            feature_hop_ms: 100,
            low_crossover_hz: 200.,
            high_crossover_hz: 2500.,
        }
    }
}
impl Record for AnalysisConfig {
    fn validate(&self) -> Result<()> {
        require(
            (20..=1000).contains(&self.feature_hop_ms)
                && self.max_duration_seconds > 0
                && self.max_duration_seconds <= 7200
                && self.settling_ms <= 60000,
            "CONFIG",
            "invalid analysis limits",
        )?;
        require(
            self.low_crossover_hz.is_finite()
                && self.high_crossover_hz.is_finite()
                && self.low_crossover_hz > 0.
                && self.low_crossover_hz < self.high_crossover_hz
                && self.high_crossover_hz < 4000.,
            "CONFIG",
            "invalid band crossovers",
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Beat {
    pub event_id: String,
    pub source_frame: u64,
    pub score: f64,
    pub local_bpm: Option<f64>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Interval {
    pub start_frame: u64,
    pub end_frame: u64,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Meter {
    pub start_frame: u64,
    pub end_frame: u64,
    pub pulses_per_bar: u32,
    pub pulse_unit: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Beats {
    #[serde(flatten)]
    pub header: Header,
    pub beats_id: String,
    pub source_sample_rate_hz: u32,
    pub source_frames: u64,
    pub events: Vec<Beat>,
    pub downbeat_ids: Vec<String>,
    pub meter_segments: Vec<Meter>,
    pub unreliable_intervals: Vec<Interval>,
    pub score_semantics: String,
}
impl Record for Beats {
    fn validate(&self) -> Result<()> {
        require(
            self.source_sample_rate_hz > 0 && self.source_frames <= MAX_FRAME,
            "BEATS",
            "invalid source timebase",
        )?;
        let mut prev = None;
        let mut ids = BTreeSet::new();
        for b in &self.events {
            require(
                b.source_frame < self.source_frames
                    && prev.is_none_or(|p| p < b.source_frame)
                    && ids.insert(&b.event_id),
                "BEAT_ORDER",
                "duplicate, backward or out-of-range beat",
            )?;
            finite(b.score)?;
            require(
                (0.0..=1.0).contains(&b.score),
                "BEAT_SCORE",
                "score out of range",
            )?;
            if let Some(bpm) = b.local_bpm {
                require(bpm.is_finite() && bpm > 0., "TEMPO", "invalid local tempo")?;
            }
            prev = Some(b.source_frame);
        }
        let mut last = None;
        for id in &self.downbeat_ids {
            let i = self
                .events
                .iter()
                .position(|b| &b.event_id == id)
                .ok_or_else(|| Error::new("DOWNBEAT", "downbeat does not reference a beat"))?;
            require(
                last.is_none_or(|p| p < i),
                "DOWNBEAT",
                "downbeat references must be strictly increasing",
            )?;
            last = Some(i);
        }
        for i in &self.unreliable_intervals {
            bounds(i.start_frame, i.end_frame, self.source_frames)?;
        }
        for m in &self.meter_segments {
            bounds(m.start_frame, m.end_frame, self.source_frames)?;
            require(
                (2..=16).contains(&m.pulses_per_bar),
                "METER",
                "invalid meter",
            )?;
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct FeatureFrame {
    pub source_frame: u64,
    pub chroma: [f64; 12],
    pub band_energy: [f64; 3],
    pub rms: f64,
    pub peak: f64,
    pub onset: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct FeatureChunk {
    #[serde(flatten)]
    pub header: Header,
    pub chunk_id: String,
    pub source_sample_rate_hz: u32,
    pub hop_frames: u64,
    pub dimensions: u32,
    pub pitch_class_order: String,
    pub method: String,
    pub normalization: String,
    pub tuning_reference_hz: f64,
    pub frames: Vec<FeatureFrame>,
}
impl Record for FeatureChunk {
    fn validate(&self) -> Result<()> {
        require(
            self.dimensions == 12 && self.hop_frames > 0 && self.source_sample_rate_hz > 0,
            "FEATURE_SHAPE",
            "invalid feature dimensions/timebase",
        )?;
        let mut prev = None;
        for f in &self.frames {
            require(
                f.source_frame <= MAX_FRAME && prev.is_none_or(|p| p < f.source_frame),
                "FEATURE_ORDER",
                "non-increasing feature frames",
            )?;
            for v in f
                .chroma
                .iter()
                .chain(f.band_energy.iter())
                .chain([&f.rms, &f.peak, &f.onset])
            {
                require(
                    v.is_finite() && *v >= 0.,
                    "FEATURE_VALUE",
                    "invalid feature value",
                )?;
            }
            prev = Some(f.source_frame);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct KeyCandidate {
    pub tonic: u8,
    pub mode: String,
    pub score: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TonalWindow {
    pub start_frame: u64,
    pub end_frame: u64,
    pub context_seconds: f64,
    pub candidates: Vec<KeyCandidate>,
    pub ambiguous: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Harmony {
    #[serde(flatten)]
    pub header: Header,
    pub harmony_id: String,
    pub source_sample_rate_hz: u32,
    pub tonic_encoding: String,
    pub mode_vocabulary: Vec<String>,
    pub score_semantics: String,
    pub tuning_cents: Option<f64>,
    pub tuning_reason: Option<String>,
    pub tuning_reference_hz: f64,
    pub feature_chunks: Vec<FileRef>,
    pub windows: Vec<TonalWindow>,
}
impl Record for Harmony {
    fn validate(&self) -> Result<()> {
        require(
            self.source_sample_rate_hz > 0
                && self.tuning_reference_hz.is_finite()
                && self.tuning_reference_hz > 0.,
            "HARMONY_TIMEBASE",
            "invalid harmonic timebase/tuning",
        )?;
        if let Some(cents) = self.tuning_cents {
            finite(cents)?;
        }
        for w in &self.windows {
            bounds(w.start_frame, w.end_frame, MAX_FRAME)?;
            require(
                w.context_seconds.is_finite() && w.context_seconds > 0.,
                "HARMONY_WINDOW",
                "invalid context",
            )?;
            for c in &w.candidates {
                require(
                    c.tonic < 12 && self.mode_vocabulary.contains(&c.mode),
                    "HARMONY_KEY",
                    "unsupported tonic or mode",
                )?;
                finite(c.score)?;
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RegionRole {
    Entry,
    Exit,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Region {
    pub region_id: String,
    pub role: RegionRole,
    pub start_frame: u64,
    pub end_frame: u64,
    pub beat_ids: Vec<String>,
    pub tempo_range_bpm: Option<[f64; 2]>,
    pub chroma: [f64; 12],
    pub energy: f64,
    pub available_overlap_frames: u64,
    pub reliable: bool,
    pub exclusion_reasons: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Regions {
    #[serde(flatten)]
    pub header: Header,
    pub regions_id: String,
    pub source_frames: u64,
    pub regions: Vec<Region>,
}
impl Record for Regions {
    fn validate(&self) -> Result<()> {
        let mut ids = BTreeSet::new();
        for r in &self.regions {
            valid_id(&r.region_id)?;
            require(ids.insert(&r.region_id), "REGION_ID", "duplicate region")?;
            bounds(r.start_frame, r.end_frame, self.source_frames)?;
            finite(r.energy)?;
            for v in r.chroma {
                finite(v)?;
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Stage {
    pub implementation: String,
    pub input_hash: ContentHash,
    pub artifacts: BTreeMap<String, FileRef>,
    pub dependencies: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct AnalysisManifest {
    #[serde(flatten)]
    pub header: Header,
    pub analysis_id: String,
    pub audio_hash: ContentHash,
    pub effective_key: ContentHash,
    pub stages: BTreeMap<String, Stage>,
    pub artifacts: BTreeMap<String, FileRef>,
    pub quality_flags: Vec<String>,
    pub peak: f64,
    pub rms: f64,
    pub created_at: String,
}
impl Record for AnalysisManifest {
    fn validate(&self) -> Result<()> {
        valid_id(&self.analysis_id)?;
        require(
            self.peak.is_finite() && self.rms.is_finite() && self.peak >= 0. && self.rms >= 0.,
            "ANALYSIS_LEVEL",
            "invalid level",
        )
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum Correction {
    InsertBeat {
        source_frame: u64,
    },
    DeleteBeat {
        source_frame: u64,
    },
    Downbeat {
        source_frame: u64,
        enabled: bool,
    },
    Meter {
        start_frame: u64,
        end_frame: u64,
        pulses_per_bar: u32,
    },
    Unsuitable {
        start_frame: u64,
        end_frame: u64,
    },
    Cue {
        region_id: String,
        start_frame: u64,
        end_frame: u64,
    },
    ExcludeTransition {
        to_track_id: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Overrides {
    #[serde(flatten)]
    pub header: Header,
    pub overrides_id: String,
    pub revision: u64,
    pub base_audio_hash: ContentHash,
    pub base_analysis: Option<FileRef>,
    pub operations: Vec<Correction>,
    pub user_preferences: Extensions,
    pub migration_status: String,
}
impl Record for Overrides {
    fn validate(&self) -> Result<()> {
        valid_id(&self.overrides_id)?;
        for op in &self.operations {
            match op {
                Correction::InsertBeat { source_frame }
                | Correction::DeleteBeat { source_frame }
                | Correction::Downbeat { source_frame, .. } => require(
                    *source_frame < MAX_FRAME,
                    "OVERRIDE_RANGE",
                    "invalid correction frame",
                )?,
                Correction::Meter {
                    start_frame,
                    end_frame,
                    pulses_per_bar,
                } => {
                    bounds(*start_frame, *end_frame, MAX_FRAME)?;
                    require(
                        (2..=16).contains(pulses_per_bar),
                        "OVERRIDE_METER",
                        "invalid pulse count",
                    )?;
                }
                Correction::Unsuitable {
                    start_frame,
                    end_frame,
                }
                | Correction::Cue {
                    start_frame,
                    end_frame,
                    ..
                } => bounds(*start_frame, *end_frame, MAX_FRAME)?,
                Correction::ExcludeTransition { to_track_id } => valid_id(to_track_id)?,
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CatalogSummary {
    pub track_id: String,
    pub audio_hash: ContentHash,
    pub duration_seconds: f64,
    pub available: bool,
    pub analysis: Option<FileRef>,
    pub override_revision: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CatalogShard {
    #[serde(flatten)]
    pub header: Header,
    pub shard_id: String,
    pub tracks: Vec<CatalogSummary>,
}
impl Record for CatalogShard {}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CatalogManifest {
    #[serde(flatten)]
    pub header: Header,
    pub generation_id: String,
    pub shards: Vec<FileRef>,
}
impl Record for CatalogManifest {}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Template {
    ShortCrossfade,
    BassHandoff,
    ThreeBandExchange,
    Boundary,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct MixingConfig {
    #[serde(flatten)]
    pub header: Header,
    pub config_id: String,
    pub beam_width: usize,
    pub max_expansions: usize,
    pub max_speed_deviation: f64,
    pub max_speed_change_per_second: f64,
    pub overlap_seconds: f64,
    pub min_played_seconds: f64,
    pub allow_fallback: bool,
    pub template: Template,
    pub output_sample_rate_hz: u32,
}
impl Default for MixingConfig {
    fn default() -> Self {
        Self {
            header: Header::default(),
            config_id: "mixing".into(),
            beam_width: 32,
            max_expansions: 100000,
            max_speed_deviation: 0.06,
            max_speed_change_per_second: 0.02,
            overlap_seconds: 8.,
            min_played_seconds: 5.,
            allow_fallback: true,
            template: Template::BassHandoff,
            output_sample_rate_hz: 48000,
        }
    }
}
impl Record for MixingConfig {
    fn validate(&self) -> Result<()> {
        require(
            (1..=128).contains(&self.beam_width)
                && (1..=1000000).contains(&self.max_expansions)
                && [44100, 48000].contains(&self.output_sample_rate_hz),
            "CONFIG",
            "invalid planner budget or output rate",
        )?;
        require(
            self.max_speed_deviation.is_finite()
                && (0.0..=0.2).contains(&self.max_speed_deviation)
                && self.max_speed_change_per_second.is_finite()
                && (0.001..=0.2).contains(&self.max_speed_change_per_second)
                && self.overlap_seconds.is_finite()
                && (0.01..=120.).contains(&self.overlap_seconds)
                && self.min_played_seconds.is_finite()
                && self.min_played_seconds > 0.,
            "CONFIG",
            "invalid transition limits",
        )
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Anchor {
    pub source_frame: u64,
    pub output_frame: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TimeMap {
    #[serde(flatten)]
    pub header: Header,
    pub map_id: String,
    pub source_sample_rate_hz: u32,
    pub output_sample_rate_hz: u32,
    pub interpolation: String,
    pub anchors: Vec<Anchor>,
    pub speed_ratio_limits: [f64; 2],
    pub max_speed_change_per_second: f64,
    pub preserve_pitch: bool,
}
impl TimeMap {
    /// Monotone cubic Hermite, derivatives in source seconds/output seconds.
    pub fn slopes(&self) -> Vec<f64> {
        let d: Vec<f64> = self
            .anchors
            .windows(2)
            .map(|w| {
                (w[1].source_frame - w[0].source_frame) as f64
                    / self.source_sample_rate_hz as f64
                    / ((w[1].output_frame - w[0].output_frame) as f64
                        / self.output_sample_rate_hz as f64)
            })
            .collect();
        if d.is_empty() {
            return vec![];
        }
        let mut m = vec![d[0]];
        for w in d.windows(2) {
            m.push(2. * w[0] * w[1] / (w[0] + w[1]));
        }
        m.push(*d.last().unwrap());
        m
    }
    pub fn source_at(&self, output: f64) -> f64 {
        let a = &self.anchors;
        let i = a
            .partition_point(|a| a.output_frame as f64 <= output)
            .saturating_sub(1)
            .min(a.len() - 2);
        let x = &a[i];
        let y = &a[i + 1];
        let h = (y.output_frame - x.output_frame) as f64 / self.output_sample_rate_hz as f64;
        let t = ((output - x.output_frame as f64) / self.output_sample_rate_hz as f64 / h)
            .clamp(0., 1.);
        let m = self.slopes();
        let s0 = x.source_frame as f64 / self.source_sample_rate_hz as f64;
        let s1 = y.source_frame as f64 / self.source_sample_rate_hz as f64;
        ((2. * t.powi(3) - 3. * t * t + 1.) * s0
            + (t.powi(3) - 2. * t * t + t) * h * m[i]
            + (-2. * t.powi(3) + 3. * t * t) * s1
            + (t.powi(3) - t * t) * h * m[i + 1])
            * self.source_sample_rate_hz as f64
    }
}
impl Record for TimeMap {
    fn validate(&self) -> Result<()> {
        require(
            self.anchors.len() >= 2
                && self.source_sample_rate_hz > 0
                && self.output_sample_rate_hz > 0
                && self.interpolation == "monotone_cubic"
                && self.preserve_pitch,
            "TIME_MAP",
            "invalid time map contract",
        )?;
        let [lo, hi] = self.speed_ratio_limits;
        require(
            lo.is_finite()
                && hi.is_finite()
                && lo > 0.
                && lo <= hi
                && self.max_speed_change_per_second.is_finite()
                && self.max_speed_change_per_second > 0.,
            "TIME_MAP",
            "invalid speed limits",
        )?;
        for w in self.anchors.windows(2) {
            require(
                w[0].source_frame < w[1].source_frame
                    && w[0].output_frame < w[1].output_frame
                    && w[1].source_frame <= MAX_FRAME
                    && w[1].output_frame <= MAX_FRAME,
                "TIME_MAP_ORDER",
                "time map must be strictly increasing",
            )?;
        }
        let m = self.slopes();
        for (i, w) in self.anchors.windows(2).enumerate() {
            let h =
                (w[1].output_frame - w[0].output_frame) as f64 / self.output_sample_rate_hz as f64;
            let d = (w[1].source_frame - w[0].source_frame) as f64
                / self.source_sample_rate_hz as f64
                / h;
            let a = 3. * (m[i] + m[i + 1] - 2. * d);
            let b = 6. * d - 4. * m[i] - 2. * m[i + 1];
            let c = m[i];
            let mut ts = vec![0., 1.];
            if a.abs() > 1e-12 {
                let t = -b / (2. * a);
                if t > 0. && t < 1. {
                    ts.push(t);
                }
            }
            for t in ts {
                let speed = a * t * t + b * t + c;
                require(
                    speed >= lo - 1e-7 && speed <= hi + 1e-7,
                    "TIME_MAP_SPEED",
                    "interpolated derivative exceeds speed limits",
                )?;
            }
            require(
                b.abs().max((2. * a + b).abs()) / h <= self.max_speed_change_per_second + 1e-7,
                "TIME_MAP_ACCELERATION",
                "stretch changes too rapidly",
            )?;
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct AutomationPoint {
    pub output_frame: u64,
    pub gain_linear: f64,
    pub bands_linear: [f64; 3],
    pub mute: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Placement {
    pub track_id: String,
    pub audio_hash: ContentHash,
    pub audio_revision: u64,
    pub analysis: FileRef,
    pub override_revision: u64,
    pub source_start_frame: u64,
    pub source_end_frame: u64,
    pub time_map: TimeMap,
    pub automation: Vec<AutomationPoint>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Cost {
    pub value: f64,
    pub weight: f64,
    pub normalization: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Transition {
    pub transition_id: String,
    pub from_track_id: String,
    pub to_track_id: String,
    pub outgoing_region_id: String,
    pub incoming_region_id: String,
    pub start_output_frame: u64,
    pub end_output_frame: u64,
    pub template: Template,
    pub costs: BTreeMap<String, Cost>,
    pub fallback_reason: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct OutputFormat {
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub encoding: String,
    pub dither: String,
}
/// Prepared incoming excerpt with a drum-only lead and a band handoff.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct DrumIntro {
    pub audio: FileRef,
    pub source_offset: u64,
    pub accompaniment_start_frame: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outgoing: Option<DrumRemoval>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct DrumRemoval {
    pub audio: FileRef,
    pub source_offset: u64,
}

impl DrumIntro {
    pub fn assets(&self) -> impl Iterator<Item = (&'static str, usize, &FileRef, u64)> {
        std::iter::once((DRUM_INTRO_SOURCE, 1, &self.audio, self.source_offset)).chain(
            self.outgoing
                .iter()
                .map(|clip| (DRUM_REMOVAL_SOURCE, 0, &clip.audio, clip.source_offset)),
        )
    }
}

pub const DRUM_INTRO_SOURCE: &str = "@drum-intro";
pub const DRUM_REMOVAL_SOURCE: &str = "@drum-removal";

/// A prepared excerpt that replaces a bounded range of a placement's source frames.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PreparedPatch {
    pub placement: usize,
    pub audio: FileRef,
    pub source_offset: u64,
    pub source_end: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct MixPlan {
    #[serde(flatten)]
    pub header: Header,
    pub plan_id: String,
    pub planner_version: String,
    pub renderer_version: String,
    pub policy_hash: ContentHash,
    pub policy: MixingConfig,
    pub seed: u64,
    pub objective: String,
    pub requested_duration_seconds: Option<f64>,
    pub complete: bool,
    pub output: OutputFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drum_intro: Option<DrumIntro>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prepared_patches: Vec<PreparedPatch>,
    pub placements: Vec<Placement>,
    pub transitions: Vec<Transition>,
    pub total_output_frames: u64,
    pub crossover_hz: [f64; 2],
    pub crossover_design: String,
    pub automation_interpolation: String,
    pub diagnostics: Vec<String>,
}
impl MixPlan {
    pub fn prepared_assets(&self) -> impl Iterator<Item = (&str, &FileRef)> {
        self.drum_intro
            .iter()
            .flat_map(DrumIntro::assets)
            .map(|(key, _, audio, _)| (key, audio))
            .chain(
                self.prepared_patches
                    .iter()
                    .map(|p| (p.audio.path.as_str(), &p.audio)),
            )
    }
}

impl Record for MixPlan {
    fn validate(&self) -> Result<()> {
        read_tactics(self)?;
        valid_id(&self.plan_id)?;
        self.policy.validate()?;
        require(
            self.policy_hash == canonical_hash(&self.policy)?,
            "POLICY_HASH",
            "policy hash mismatch",
        )?;
        require(
            !self.placements.is_empty()
                && self.transitions.len() + 1 == self.placements.len()
                && self.complete
                && self.total_output_frames <= MAX_FRAME,
            "PLAN",
            "incomplete plan",
        )?;
        require(
            self.output.channels == 2
                && [44100, 48000].contains(&self.output.sample_rate_hz)
                && ["float32", "pcm24"].contains(&self.output.encoding.as_str())
                && self.output.dither == "seeded_tpdf_1_lsb"
                && self.crossover_design == "complementary_one_pole_v1"
                && self.automation_interpolation == "smoothstep_linear_amplitude",
            "PLAN_FORMAT",
            "unsupported output/DSP contract",
        )?;
        require(
            self.crossover_hz[0] > 0.
                && self.crossover_hz[1] > self.crossover_hz[0]
                && self.crossover_hz[1] < self.output.sample_rate_hz as f64 / 2.,
            "CROSSOVER",
            "invalid crossover",
        )?;
        if let Some(intro) = &self.drum_intro {
            require(
                self.placements.len() == 2
                    && self.transitions[0].start_output_frame < intro.accompaniment_start_frame
                    && match &intro.outgoing {
                        Some(clip) => {
                            intro.accompaniment_start_frame <= self.transitions[0].end_output_frame
                                && clip.source_offset <= self.placements[0].source_start_frame
                                && clip.audio.path == format!("stems/{}-outgoing.wav", self.plan_id)
                        }
                        None => {
                            intro.accompaniment_start_frame < self.transitions[0].end_output_frame
                        }
                    }
                    && intro.source_offset <= self.placements[1].source_start_frame
                    && intro.audio.path == format!("stems/{}.wav", self.plan_id),
                "DRUM_INTRO",
                "invalid prepared incoming excerpt or handoff bounds",
            )?;
        }
        let mut patch_ends = BTreeMap::new();
        for patch in &self.prepared_patches {
            let placement = self.placements.get(patch.placement).ok_or_else(|| {
                Error::new(
                    "PLAN_PATCH",
                    "prepared patch references an unknown placement",
                )
            })?;
            require(
                self.drum_intro.is_none()
                    && patch.audio.path.starts_with("stems/")
                    && patch.source_offset < patch.source_end
                    && patch.source_offset < placement.source_end_frame
                    && patch.source_end > placement.source_start_frame
                    && patch_ends
                        .get(&patch.placement)
                        .is_none_or(|end| patch.source_offset >= *end),
                "PLAN_PATCH",
                "invalid or overlapping prepared source patch",
            )?;
            patch_ends.insert(patch.placement, patch.source_end);
        }
        let mut ids = BTreeSet::new();
        let mut hashes = BTreeSet::new();
        let mut end = 0;
        for (i, p) in self.placements.iter().enumerate() {
            require(
                ids.insert(&p.track_id) && hashes.insert(&p.audio_hash.digest),
                "PLAN_REPEAT",
                "duplicate audio in no-repeat plan",
            )?;
            bounds(p.source_start_frame, p.source_end_frame, p.source_end_frame)?;
            p.time_map.validate()?;
            require(
                p.time_map.speed_ratio_limits[0] >= 1. - self.policy.max_speed_deviation
                    && p.time_map.speed_ratio_limits[1] <= 1. + self.policy.max_speed_deviation
                    && p.time_map.max_speed_change_per_second
                        <= self.policy.max_speed_change_per_second,
                "PLAN_STRETCH_POLICY",
                "time map exceeds saved policy limits",
            )?;
            let a = &p.time_map.anchors;
            require(
                a[0].source_frame == p.source_start_frame
                    && a.last().unwrap().source_frame == p.source_end_frame
                    && p.time_map.output_sample_rate_hz == self.output.sample_rate_hz,
                "PLAN_MAP",
                "placement/map bounds mismatch",
            )?;
            require(
                if i == 0 {
                    a[0].output_frame == 0
                } else {
                    a[0].output_frame <= end && a.last().unwrap().output_frame > end
                },
                "PLAN_PLACEMENT",
                "unintended gap or contained placement",
            )?;
            let mut prev = None;
            require(
                p.automation.iter().any(|v| {
                    !v.mute && v.gain_linear > 0. && v.bands_linear.iter().any(|g| *g > 0.)
                }),
                "PLAN_AUDIBILITY",
                "placement is entirely muted",
            )?;
            for v in &p.automation {
                require(
                    v.output_frame >= a[0].output_frame
                        && v.output_frame <= a.last().unwrap().output_frame
                        && prev.is_none_or(|p| p < v.output_frame),
                    "AUTOMATION_ORDER",
                    "invalid automation time",
                )?;
                for g in [v.gain_linear].into_iter().chain(v.bands_linear) {
                    require(
                        g.is_finite() && (0.0..=8.).contains(&g),
                        "AUTOMATION_GAIN",
                        "invalid gain",
                    )?;
                }
                prev = Some(v.output_frame);
            }
            require(
                p.automation
                    .first()
                    .is_some_and(|v| v.output_frame == a[0].output_frame)
                    && p.automation
                        .last()
                        .is_some_and(|v| v.output_frame == a.last().unwrap().output_frame),
                "AUTOMATION_BOUNDS",
                "automation must cover placement",
            )?;
            if i > 0 {
                let t = &self.transitions[i - 1];
                require(
                    t.from_track_id == self.placements[i - 1].track_id
                        && t.to_track_id == p.track_id
                        && t.start_output_frame == a[0].output_frame
                        && t.end_output_frame == end,
                    "TRANSITION_BOUNDS",
                    "transition does not match placements",
                )?;
                if i > 1 {
                    require(
                        t.start_output_frame >= self.transitions[i - 2].end_output_frame,
                        "TRANSITION_OVERLAP",
                        "overlapping transitions",
                    )?;
                }
                for c in t.costs.values() {
                    finite(c.value)?;
                    finite(c.weight)?;
                }
            }
            end = a.last().unwrap().output_frame;
        }
        require(
            end == self.total_output_frames,
            "PLAN_DURATION",
            "incorrect total duration",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_order() {
        assert!(canonical_hash(&vec![Some(f64::NAN)]).is_err());
        assert_eq!(
            canonical_hash(&serde_json::json!({"a":1,"b":2})).unwrap(),
            canonical_hash(&serde_json::from_str::<Value>("{\"b\":2,\"a\":1}").unwrap()).unwrap()
        );
    }
    #[test]
    fn map_derivative_checks_interior() {
        let map = TimeMap {
            header: Header::default(),
            map_id: "m".into(),
            source_sample_rate_hz: 100,
            output_sample_rate_hz: 100,
            interpolation: "monotone_cubic".into(),
            anchors: vec![
                Anchor {
                    source_frame: 0,
                    output_frame: 0,
                },
                Anchor {
                    source_frame: 94,
                    output_frame: 100,
                },
                Anchor {
                    source_frame: 200,
                    output_frame: 200,
                },
            ],
            speed_ratio_limits: [0.94, 1.06],
            max_speed_change_per_second: 1.,
            preserve_pitch: true,
        };
        assert_eq!(map.validate().unwrap_err().code, "TIME_MAP_SPEED");
    }
    #[test]
    fn unknown_version_rejected() {
        let mut v = serde_json::to_value(AnalysisConfig::default()).unwrap();
        v["schema_version"] = 2.into();
        assert_eq!(
            validate_value::<AnalysisConfig>(&v).unwrap_err().code,
            "SCHEMA_VERSION"
        );
    }
    #[test]
    fn nonfinite_and_bad_shape_rejected() {
        let c = MixingConfig {
            overlap_seconds: f64::NAN,
            ..MixingConfig::default()
        };
        assert!(encode(&c).is_err());
        let mut v = serde_json::to_value(c).unwrap();
        v["overlap_seconds"] = 1.into();
        v["beam_width"] = (-1).into();
        assert!(validate_value::<MixingConfig>(&v).is_err());
    }
}
