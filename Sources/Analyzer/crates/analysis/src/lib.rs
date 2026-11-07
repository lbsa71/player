//! Versioned analysis stages. Missing rhythm evidence stays unknown; it never becomes a fixed grid.
use mixlib_audio::WavReader;
use mixlib_catalog::{fingerprint, rebuild_catalog, shard, validate_analysis, Store};
use mixlib_types::*;
use rustfft::{num_complex::Complex, FftPlanner};
use std::{
    collections::{BTreeMap, VecDeque},
    path::Path,
    sync::Arc,
};

pub const RHYTHM: &str = "beat-this-1.1.0-rten-peak-picker/1";
pub const FEATURES: &str = "fft-chroma-band-rms/1";
pub const STRUCTURE: &str = "novelty-regions/1";
pub trait RhythmAnalyzer {
    fn identity(&self) -> &str;
    fn analyze(
        &mut self,
        path: &Path,
        format: &AudioFormat,
        cancel: &Cancellation,
    ) -> Result<Beats>;
}
pub struct UnknownRhythm;
impl RhythmAnalyzer for UnknownRhythm {
    fn identity(&self) -> &str {
        "unavailable/1"
    }
    fn analyze(&mut self, _: &Path, f: &AudioFormat, _: &Cancellation) -> Result<Beats> {
        Ok(Beats {
            header: Header::default(),
            beats_id: id(),
            source_sample_rate_hz: f.sample_rate_hz,
            source_frames: f.frames,
            events: vec![],
            downbeat_ids: vec![],
            meter_segments: vec![],
            unreliable_intervals: vec![Interval {
                start_frame: 0,
                end_frame: f.frames,
                reason: "beat backend explicitly disabled; pulse and meter unknown".into(),
            }],
            score_semantics: "no beat evidence".into(),
        })
    }
}
struct NeuralRhythm {
    tracker: beat_this::BeatThis<<beat_this::RtenRuntime as beat_this::Runtime>::Model>,
}
impl RhythmAnalyzer for NeuralRhythm {
    fn identity(&self) -> &str {
        RHYTHM
    }
    fn analyze(&mut self, path: &Path, f: &AudioFormat, cancel: &Cancellation) -> Result<Beats> {
        let mut wav = WavReader::open(path)?;
        let mut stream = self
            .tracker
            .stream(f.sample_rate_hz)
            .map_err(|e| Error::new("BEAT_MODEL", e.to_string()))?;
        loop {
            cancel.check()?;
            let frames = wav.read_frames(8192)?;
            if frames.is_empty() {
                break;
            }
            let mono: Vec<f32> = frames.iter().map(|f| (f[0] + f[1]) * 0.5).collect();
            stream
                .push(&mono)
                .map_err(|e| Error::new("BEAT_MODEL", e.to_string()))?;
        }
        cancel.check()?;
        let result = stream
            .finish()
            .map_err(|e| Error::new("BEAT_MODEL", e.to_string()))?;
        cancel.check()?;
        let mut events = vec![];
        for time in result.beats {
            let frame = (time as f64 * f.sample_rate_hz as f64).round() as u64;
            if frame >= f.frames
                || events
                    .last()
                    .is_some_and(|e: &Beat| e.source_frame >= frame)
            {
                continue;
            }
            let logit = result
                .beat_logits
                .get((time * 50.).round() as usize)
                .copied()
                .unwrap_or(0.);
            let score = 1. / (1. + (-(logit as f64)).exp());
            events.push(Beat {
                event_id: format!("beat-{frame}"),
                source_frame: frame,
                score,
                local_bpm: None,
            });
        }
        derive_tempi(&mut events, f.sample_rate_hz);
        let mut downbeat_ids = vec![];
        for time in result.downbeats {
            let frame = (time as f64 * f.sample_rate_hz as f64).round() as u64;
            if let Some(b) = events.iter().min_by_key(|b| b.source_frame.abs_diff(frame)) {
                if b.source_frame.abs_diff(frame) <= f.sample_rate_hz as u64 / 20
                    && !downbeat_ids.contains(&b.event_id)
                {
                    downbeat_ids.push(b.event_id.clone());
                }
            }
        }
        let mut meters = vec![];
        let indices: Vec<usize> = downbeat_ids
            .iter()
            .filter_map(|id| events.iter().position(|b| &b.event_id == id))
            .collect();
        for w in indices.windows(3) {
            let pulses = w[1] - w[0];
            if (2..=12).contains(&pulses) && w[2] - w[1] == pulses {
                meters.push(Meter {
                    start_frame: events[w[0]].source_frame,
                    end_frame: events[w[2]].source_frame,
                    pulses_per_bar: pulses as u32,
                    pulse_unit: "model_beat_pulse".into(),
                });
            }
        }
        let mut unreliable = vec![];
        for w in events.windows(2) {
            if w[0].score < 0.5
                || w[1].score < 0.5
                || w[1].source_frame - w[0].source_frame > f.sample_rate_hz as u64 * 2
            {
                unreliable.push(Interval {
                    start_frame: w[0].source_frame,
                    end_frame: w[1].source_frame,
                    reason: "weak activation or missing pulse".into(),
                });
            }
        }
        if events.len() < 4 {
            unreliable.push(Interval {
                start_frame: 0,
                end_frame: f.frames,
                reason: "insufficient beats".into(),
            });
        }
        let b = Beats {
            header: Header::default(),
            beats_id: id(),
            source_sample_rate_hz: f.sample_rate_hz,
            source_frames: f.frames,
            events,
            downbeat_ids,
            meter_segments: meters,
            unreliable_intervals: unreliable,
            score_semantics: "sigmoid of raw model beat logit at nearest 50 Hz frame; uncalibrated"
                .into(),
        };
        b.validate()?;
        Ok(b)
    }
}
pub fn derive_tempi(events: &mut [Beat], rate: u32) {
    for i in 0..events.len().saturating_sub(1) {
        let d = events[i + 1].source_frame - events[i].source_frame;
        events[i].local_bpm = Some(60. * rate as f64 / d as f64);
    }
    if events.len() > 1 {
        let n = events.len();
        events[n - 1].local_bpm = events[n - 2].local_bpm;
    }
}
fn models(
    store: &Store,
    config: &AnalysisConfig,
) -> Result<(Box<dyn RhythmAnalyzer>, serde_json::Value)> {
    if matches!(config.beat_backend, BeatBackend::Unavailable) {
        return Ok((Box::new(UnknownRhythm), serde_json::json!("unavailable/1")));
    }
    let m: ModelManifest = store.read("models/manifest.json")?;
    let mut paths = vec![];
    let mut hashes = vec![];
    for name in ["mel_spectrogram", "beat_this"] {
        let asset=m.models.iter().find(|m|m.model_id==name).ok_or_else(||Error::new("MODEL_MISSING",format!("install the pinned {name} model using scripts/install-models.py, or explicitly select --dsp-only for fallback-only analysis")))?;
        let path = store.path(&asset.file.path)?;
        let metadata = std::fs::metadata(&path)?;
        require(
            metadata.len() <= 512 * 1024 * 1024,
            "MODEL_SIZE",
            "model exceeds 512 MiB",
        )?;
        require(
            hash_bytes(&std::fs::read(&path)?) == asset.file.hash,
            "MODEL_CHECKSUM",
            name,
        )?;
        hashes.push(asset.file.hash.clone());
        paths.push(path);
    }
    let tracker = beat_this::BeatThis::new(&beat_this::RtenRuntime, &paths[0], &paths[1])
        .map_err(|e| Error::new("BEAT_MODEL", e.to_string()))?;
    Ok((
        Box::new(NeuralRhythm { tracker }),
        serde_json::json!({"backend":RHYTHM,"models":hashes,"preprocessing":"beat-this-1.1.0-stream-mono-mean-rubato-22050-mel50hz"}),
    ))
}

struct Features {
    frames: Vec<FeatureFrame>,
    peak: f64,
    rms: f64,
    tuning: Option<f64>,
    flags: Vec<String>,
}
fn extract(path: &Path, c: &AnalysisConfig, cancel: &Cancellation) -> Result<Features> {
    let mut wav = WavReader::open(path)?;
    let rate = wav.format.sample_rate_hz;
    let n = (rate as f64 * 0.093).round() as usize;
    let n = n.next_power_of_two();
    let hop = (rate as u64 * c.feature_hop_ms as u64 / 1000).max(1) as usize;
    let fft = FftPlanner::<f64>::new().plan_fft_forward(n);
    let mut window = VecDeque::<[f32; 2]>::with_capacity(n);
    let mut cursor = 0u64;
    let mut frames = vec![];
    let mut peak = 0f64;
    let mut sum = 0.;
    let mut mono_sum = 0.;
    let mut last_rms = 0.;
    let mut tuning_x = 0.;
    let mut tuning_y = 0.;
    loop {
        cancel.check()?;
        let block = wav.read_frames(8192)?;
        if block.is_empty() {
            break;
        }
        for pair in block {
            for x in pair {
                peak = peak.max(x.abs() as f64);
                sum += (x as f64).powi(2) * 0.5;
            }
            mono_sum += ((pair[0] as f64 + pair[1] as f64) * 0.5).powi(2);
            window.push_back(pair);
            if window.len() > n {
                window.pop_front();
            }
            cursor += 1;
            if cursor as usize % hop != 0 {
                continue;
            }
            let feature = feature(
                &window,
                n,
                &fft,
                rate,
                c,
                cursor.saturating_sub(hop as u64),
                last_rms,
                &mut tuning_x,
                &mut tuning_y,
            );
            last_rms = feature.rms;
            frames.push(feature);
        }
    }
    if frames.is_empty() {
        frames.push(feature(
            &window,
            n,
            &fft,
            rate,
            c,
            0,
            0.,
            &mut tuning_x,
            &mut tuning_y,
        ));
    }
    let mut flags = vec![
        "loudness_is_uncalibrated_rms_not_lufs".into(),
        "lead_activity_unknown".into(),
        "key_estimator_baseline_unvalidated".into(),
    ];
    if sum > 1e-12 && mono_sum / sum < 0.05 {
        flags.push("destructive_stereo_cancellation".into());
    }
    let rms = (sum / wav.format.frames as f64).sqrt();
    if peak < 1e-5 {
        flags.push("silent_audio".into());
    }
    let tuning = if tuning_x.hypot(tuning_y) > 1e-9 {
        Some(tuning_y.atan2(tuning_x) * 100. / (2. * std::f64::consts::PI))
    } else {
        None
    };
    Ok(Features {
        frames,
        peak,
        rms,
        tuning,
        flags,
    })
}
#[allow(clippy::too_many_arguments)]
fn feature(
    window: &VecDeque<[f32; 2]>,
    n: usize,
    fft: &Arc<dyn rustfft::Fft<f64>>,
    rate: u32,
    c: &AnalysisConfig,
    frame: u64,
    last_rms: f64,
    tx: &mut f64,
    ty: &mut f64,
) -> FeatureFrame {
    let mut data = vec![Complex::new(0., 0.); n];
    let mut sum = 0.;
    let mut peak = 0f64;
    let offset = n - window.len();
    for (i, p) in window.iter().enumerate() {
        let mono = (p[0] as f64 + p[1] as f64) * 0.5;
        let w = 0.5 - 0.5 * (2. * std::f64::consts::PI * (i + offset) as f64 / n as f64).cos();
        data[i + offset].re = mono * w;
        sum += (p[0] as f64).powi(2) * 0.5 + (p[1] as f64).powi(2) * 0.5;
        peak = peak.max(p[0].abs().max(p[1].abs()) as f64);
    }
    fft.process(&mut data);
    let mut chroma = [0.; 12];
    let mut bands = [0.; 3];
    for k in 1..n / 2 {
        let freq = k as f64 * rate as f64 / n as f64;
        let energy = data[k].norm_sqr() / (n * n) as f64;
        let band = if freq < c.low_crossover_hz {
            0
        } else if freq < c.high_crossover_hz {
            1
        } else {
            2
        };
        bands[band] += energy;
        if (45.0..=5000.).contains(&freq) {
            let midi = 69. + 12. * (freq / 440.).log2();
            let pitch = midi.round() as i64;
            chroma[pitch.rem_euclid(12) as usize] += energy.sqrt();
            if k > 1
                && data[k].norm_sqr() > data[k - 1].norm_sqr()
                && data[k].norm_sqr() > data[k + 1].norm_sqr()
            {
                let angle = (midi - midi.round()) * 2. * std::f64::consts::PI;
                *tx += energy * angle.cos();
                *ty += energy * angle.sin();
            }
        }
    }
    let norm = chroma.iter().sum::<f64>();
    if norm > 0. {
        for x in &mut chroma {
            *x /= norm;
        }
    }
    let rms = (sum / window.len().max(1) as f64).sqrt();
    FeatureFrame {
        source_frame: frame,
        chroma,
        band_energy: bands,
        rms,
        peak,
        onset: (rms - last_rms).max(0.),
    }
}

pub trait HarmonicEstimator {
    fn identity(&self) -> &str;
    fn windows(&self, frames: &[FeatureFrame], f: &AudioFormat) -> Vec<TonalWindow>;
}
pub struct ProfileHarmony;
impl HarmonicEstimator for ProfileHarmony {
    fn identity(&self) -> &str {
        "krumhansl-multiscale/1"
    }
    fn windows(&self, frames: &[FeatureFrame], f: &AudioFormat) -> Vec<TonalWindow> {
        let profiles = [
            [
                6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
            ],
            [
                6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
            ],
        ];
        let mut windows = vec![];
        for seconds in [8., 16., 32.] {
            let len = (seconds * f.sample_rate_hz as f64) as u64;
            let mut start = 0;
            let mut previous = None;
            while start < f.frames {
                let end = (start + len).min(f.frames);
                let mut chroma = [0.; 12];
                for frame in frames
                    .iter()
                    .filter(|x| x.source_frame >= start && x.source_frame < end)
                {
                    for (c, x) in chroma.iter_mut().zip(frame.chroma) {
                        *c += x;
                    }
                }
                let total = chroma.iter().sum::<f64>();
                let mut candidates = vec![];
                if total > 1e-6 {
                    for tonic in 0..12 {
                        for (mode, p) in profiles.iter().enumerate() {
                            let score = correlation(
                                &chroma,
                                &std::array::from_fn(|i| p[(i + 12 - tonic) % 12]),
                            );
                            let bonus = if previous == Some((tonic, mode)) {
                                0.02
                            } else {
                                0.
                            };
                            candidates.push(KeyCandidate {
                                tonic: tonic as u8,
                                mode: if mode == 0 { "major" } else { "minor" }.into(),
                                score: score + bonus,
                            });
                        }
                    }
                    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
                    candidates.truncate(3);
                    previous = candidates
                        .first()
                        .map(|x| (x.tonic as usize, usize::from(x.mode == "minor")));
                }
                let ambiguous = candidates.len() < 2
                    || candidates[0].score < 0.5
                    || candidates[0].score - candidates[1].score < 0.08;
                windows.push(TonalWindow {
                    start_frame: start,
                    end_frame: end,
                    context_seconds: seconds,
                    candidates,
                    ambiguous,
                });
                start += len / 2;
            }
        }
        windows
    }
}
fn correlation(a: &[f64; 12], b: &[f64; 12]) -> f64 {
    let am = a.iter().sum::<f64>() / 12.;
    let bm = b.iter().sum::<f64>() / 12.;
    let numerator = a
        .iter()
        .zip(b)
        .map(|(a, b)| (a - am) * (b - bm))
        .sum::<f64>();
    let den = (a.iter().map(|x| (x - am).powi(2)).sum::<f64>()
        * b.iter().map(|x| (x - bm).powi(2)).sum::<f64>())
    .sqrt();
    if den < 1e-12 {
        0.
    } else {
        numerator / den
    }
}

pub trait StructureAnalyzer {
    fn identity(&self) -> &str;
    fn regions(&self, beats: &Beats, features: &[FeatureFrame], f: &AudioFormat) -> Regions;
}
pub struct NoveltyRegions;
impl StructureAnalyzer for NoveltyRegions {
    fn identity(&self) -> &str {
        STRUCTURE
    }
    fn regions(&self, b: &Beats, features: &[FeatureFrame], f: &AudioFormat) -> Regions {
        let length = (16 * f.sample_rate_hz as u64).min(f.frames);
        let mut boundaries = vec![0, f.frames.saturating_sub(length)];
        let min_gap = 8 * f.sample_rate_hz as u64;
        for pair in features.windows(2) {
            let harmonic: f64 = pair[0]
                .chroma
                .iter()
                .zip(pair[1].chroma)
                .map(|(a, b)| (a - b).abs())
                .sum();
            let energy = (pair[1].rms - pair[0].rms).abs();
            if (harmonic > 0.8 || energy > 0.12)
                && pair[1].source_frame > min_gap
                && pair[1].source_frame + length < f.frames
                && boundaries
                    .iter()
                    .all(|x| x.abs_diff(pair[1].source_frame) > min_gap)
            {
                boundaries.push(pair[1].source_frame);
            }
        }
        boundaries.sort_unstable();
        boundaries.dedup();
        let mut regions = vec![];
        for (i, start) in boundaries.into_iter().enumerate() {
            let end = (start + length).min(f.frames);
            let beats: Vec<&Beat> = b
                .events
                .iter()
                .filter(|x| x.source_frame >= start && x.source_frame < end)
                .collect();
            let tempi: Vec<f64> = beats.iter().filter_map(|b| b.local_bpm).collect();
            let reliable = beats.len() >= 8
                && beats.iter().all(|b| b.score >= 0.5)
                && !b
                    .unreliable_intervals
                    .iter()
                    .any(|x| x.start_frame < end && x.end_frame > start)
                && b.meter_segments.iter().any(|m| {
                    m.start_frame <= start.saturating_add(f.sample_rate_hz as u64 * 2)
                        && m.end_frame >= end.saturating_sub(f.sample_rate_hz as u64 * 2)
                });
            let fs: Vec<_> = features
                .iter()
                .filter(|f| f.source_frame >= start && f.source_frame < end)
                .collect();
            let mut chroma = [0.; 12];
            let mut energy = 0.;
            for f in &fs {
                for (c, x) in chroma.iter_mut().zip(f.chroma) {
                    *c += x / fs.len() as f64;
                }
                energy += f.rms / fs.len() as f64;
            }
            for role in [RegionRole::Entry, RegionRole::Exit] {
                regions.push(Region {
                    region_id: format!(
                        "region-{i}-{}",
                        if role == RegionRole::Entry {
                            "entry"
                        } else {
                            "exit"
                        }
                    ),
                    role,
                    start_frame: start,
                    end_frame: end,
                    beat_ids: beats.iter().map(|b| b.event_id.clone()).collect(),
                    tempo_range_bpm: if tempi.is_empty() {
                        None
                    } else {
                        Some([
                            tempi.iter().copied().fold(f64::INFINITY, f64::min),
                            tempi.iter().copied().fold(0., f64::max),
                        ])
                    },
                    chroma,
                    energy,
                    available_overlap_frames: end - start,
                    reliable,
                    exclusion_reasons: if reliable {
                        vec![]
                    } else {
                        vec!["pulse_or_meter_unreliable".into()]
                    },
                });
            }
        }
        Regions {
            header: Header::default(),
            regions_id: id(),
            source_frames: f.frames,
            regions,
        }
    }
}

pub fn analyze(
    store: &Store,
    resume: bool,
    dsp_only: bool,
    cancel: &Cancellation,
) -> Result<Report> {
    analyze_selected(store, resume, dsp_only, &[], cancel)
}

/// Empty selection analyzes all tracks. Validate every requested identity before
/// publishing anything, so a typo cannot silently broaden or partially run a batch.
pub fn analyze_selected(
    store: &Store,
    resume: bool,
    dsp_only: bool,
    selected: &[String],
    cancel: &Cancellation,
) -> Result<Report> {
    let mut tracks = store.tracks()?;
    for id in selected {
        require(
            tracks.iter().any(|t| &t.track_id == id),
            "TRACK_NOT_FOUND",
            id,
        )?;
    }
    if !selected.is_empty() {
        tracks.retain(|t| selected.contains(&t.track_id));
    }
    let mut config: AnalysisConfig = store.read("config/analysis.json")?;
    if dsp_only {
        config.beat_backend = BeatBackend::Unavailable;
    }
    // Model construction is lazy: unchanged libraries do not load neural weights.
    let manifest: ModelManifest = store.read("models/manifest.json")?;
    let model_identity = if matches!(config.beat_backend, BeatBackend::Unavailable) {
        serde_json::json!("unavailable/1")
    } else {
        serde_json::json!({"backend":RHYTHM,"models":manifest.models.iter().map(|m|(&m.model_id,&m.file.hash)).collect::<Vec<_>>()})
    };
    let mut backend: Option<Box<dyn RhythmAnalyzer>> = None;
    let mut report = Report::new("analyze");
    for mut track in tracks {
        cancel.check()?;
        if !matches!(track.lifecycle, Lifecycle::Available) {
            report.count("unavailable");
            continue;
        }
        let rhythm_key = canonical_hash(
            &serde_json::json!({"audio":track.audio_hash,"rhythm":model_identity,"version":RHYTHM}),
        )?;
        let features_key = canonical_hash(
            &serde_json::json!({"audio":track.audio_hash,"version":FEATURES,"hop_ms":config.feature_hop_ms,"crossovers":[config.low_crossover_hz,config.high_crossover_hz],"harmony":"krumhansl-multiscale/1"}),
        )?;
        let effective = canonical_hash(&(&rhythm_key, &features_key, STRUCTURE))?;
        let current = track
            .active_analysis
            .as_ref()
            .and_then(|r| store.read_ref::<AnalysisManifest>(r).ok());
        if current
            .as_ref()
            .is_some_and(|a| a.effective_key == effective && validate_analysis(store, a).is_ok())
        {
            report.count("unchanged");
            continue;
        }
        store.job(&track)?;
        let mut job: Job = store.read(&format!("jobs/{}.json", track.track_id))?;
        if matches!(
            job.state,
            JobState::Running | JobState::Cancelled | JobState::Failed | JobState::Deferred
        ) && !resume
        {
            report.count("awaiting_resume");
            continue;
        }
        job.state = JobState::Running;
        job.updated_at = now();
        job.retry_count += 1;
        job.diagnostic = None;
        store.save_job(&job)?;
        let result = (|| -> Result<()> {
            let path = store.source(&track)?;
            let before = fingerprint(&path)?;
            let mut wav = WavReader::open(&path)?;
            require(
                wav.format.frames as f64 / wav.format.sample_rate_hz as f64
                    <= config.max_duration_seconds as f64,
                "DURATION_LIMIT",
                "track exceeds configured analysis duration limit",
            )?;
            require(
                wav.hashes(cancel)?.1 == track.audio_hash,
                "SOURCE_CHANGED",
                "source differs from scan; rescan",
            )?;
            let checkpoint_path = format!("staging/{}/checkpoint.json", job.job_id);
            let checkpoint = if resume {
                store
                    .read::<AnalysisManifest>(&checkpoint_path)
                    .ok()
                    .filter(|a| a.audio_hash == track.audio_hash)
            } else {
                None
            };
            let old = checkpoint.as_ref().or(current.as_ref());
            let analysis_id = id();
            let prefix = format!(
                "tracks/{}/{}/analysis/{analysis_id}",
                shard(&track.track_id),
                track.track_id
            );
            let mut analysis = AnalysisManifest {
                header: Header::default(),
                analysis_id,
                audio_hash: track.audio_hash.clone(),
                effective_key: effective.clone(),
                stages: BTreeMap::new(),
                artifacts: BTreeMap::new(),
                quality_flags: vec![],
                peak: 0.,
                rms: 0.,
                created_at: now(),
            };
            let reusable = |name: &str, key: &ContentHash| -> Option<Stage> {
                old.and_then(|a| a.stages.get(name))
                    .filter(|s| {
                        &s.input_hash == key
                            && s.artifacts.values().all(|r| store.verify_ref(r).is_ok())
                    })
                    .cloned()
            };
            if let Some(stage) = reusable("features", &features_key) {
                analysis.peak = old.unwrap().peak;
                analysis.rms = old.unwrap().rms;
                analysis.quality_flags = old.unwrap().quality_flags.clone();
                analysis.stages.insert("features".into(), stage);
                report.count("reused_feature_stages");
            } else {
                job.stage = "features".into();
                store.save_job(&job)?;
                let features = extract(&path, &config, cancel)?;
                analysis.peak = features.peak;
                analysis.rms = features.rms;
                analysis.quality_flags = features.flags;
                let mut chunks = vec![];
                let mut artifacts = BTreeMap::new();
                for (i, frames) in features.frames.chunks(1024).enumerate() {
                    cancel.check()?;
                    let chunk = FeatureChunk {
                        header: Header::default(),
                        chunk_id: format!("chunk-{i:06}"),
                        source_sample_rate_hz: track.format.sample_rate_hz,
                        hop_frames: track.format.sample_rate_hz as u64
                            * config.feature_hop_ms as u64
                            / 1000,
                        dimensions: 12,
                        pitch_class_order: "C,C#,D,D#,E,F,F#,G,G#,A,A#,B".into(),
                        method: FEATURES.into(),
                        normalization: "chroma_l1; band_squared_magnitude_over_fft_size_squared"
                            .into(),
                        tuning_reference_hz: 440.,
                        frames: frames.to_vec(),
                    };
                    let r = store.write(&format!("{prefix}/features/{i:06}.json"), &chunk)?;
                    artifacts.insert(format!("feature-{i:06}"), r.clone());
                    chunks.push(r);
                }
                let harmony = Harmony {
                    header: Header::default(),
                    harmony_id: id(),
                    source_sample_rate_hz: track.format.sample_rate_hz,
                    tonic_encoding: "0=C, chromatic semitones".into(),
                    mode_vocabulary: vec!["major".into(), "minor".into(), "unknown".into()],
                    score_semantics:
                        "profile Pearson correlation plus 0.02 continuity preference; uncalibrated"
                            .into(),
                    tuning_cents: features.tuning,
                    tuning_reason: features
                        .tuning
                        .is_none()
                        .then(|| "no pitched spectral evidence".into()),
                    tuning_reference_hz: 440.,
                    feature_chunks: chunks,
                    windows: ProfileHarmony.windows(&features.frames, &track.format),
                };
                artifacts.insert(
                    "harmony".into(),
                    store.write(&format!("{prefix}/harmony.json"), &harmony)?,
                );
                analysis.stages.insert(
                    "features".into(),
                    Stage {
                        implementation: FEATURES.into(),
                        input_hash: features_key.clone(),
                        artifacts,
                        dependencies: vec![],
                    },
                );
            }
            store.write(&checkpoint_path, &analysis)?;
            cancel.check()?;
            if let Some(stage) = reusable("rhythm", &rhythm_key) {
                analysis.stages.insert("rhythm".into(), stage);
                report.count("reused_rhythm_stages");
            } else {
                job.stage = "rhythm".into();
                store.save_job(&job)?;
                if backend.is_none() {
                    backend = Some(models(store, &config)?.0);
                }
                let b = backend
                    .as_mut()
                    .unwrap()
                    .analyze(&path, &track.format, cancel)?;
                let r = store.write(&format!("{prefix}/beats.json"), &b)?;
                analysis.stages.insert(
                    "rhythm".into(),
                    Stage {
                        implementation: backend.as_ref().unwrap().identity().into(),
                        input_hash: rhythm_key.clone(),
                        artifacts: BTreeMap::from([("beats".into(), r)]),
                        dependencies: vec![],
                    },
                );
            }
            store.write(&checkpoint_path, &analysis)?;
            cancel.check()?;
            let harmony_ref = analysis.stages["features"].artifacts["harmony"].clone();
            let harmony: Harmony = store.read_ref(&harmony_ref)?;
            let mut features = vec![];
            for r in &harmony.feature_chunks {
                features.extend(store.read_ref::<FeatureChunk>(r)?.frames);
            }
            let beats_ref = analysis.stages["rhythm"].artifacts["beats"].clone();
            let beats: Beats = store.read_ref(&beats_ref)?;
            if beats.events.is_empty() {
                analysis
                    .quality_flags
                    .push("rhythm_unknown_fallback_required".into());
            }
            let region_key = canonical_hash(&(&beats_ref.hash, &features_key, STRUCTURE))?;
            if let Some(stage) = reusable("regions", &region_key) {
                analysis.stages.insert("regions".into(), stage);
            } else {
                let regions = NoveltyRegions.regions(&beats, &features, &track.format);
                let r = store.write(&format!("{prefix}/regions.json"), &regions)?;
                analysis.stages.insert(
                    "regions".into(),
                    Stage {
                        implementation: STRUCTURE.into(),
                        input_hash: region_key,
                        artifacts: BTreeMap::from([("regions".into(), r)]),
                        dependencies: vec!["rhythm".into(), "features".into()],
                    },
                );
            }
            analysis.artifacts = BTreeMap::from([
                ("beats".into(), beats_ref),
                ("harmony".into(), harmony_ref),
                (
                    "regions".into(),
                    analysis.stages["regions"].artifacts["regions"].clone(),
                ),
            ]);
            analysis.quality_flags.sort();
            analysis.quality_flags.dedup();
            store.write(&checkpoint_path, &analysis)?;
            validate_analysis(store, &analysis)?;
            require(
                fingerprint(&path)? == before,
                "SOURCE_SETTLING",
                "source changed during analysis",
            )?;
            require(
                WavReader::open(&path)?.hashes(cancel)?.1 == track.audio_hash,
                "SOURCE_CHANGED",
                "source content changed during analysis",
            )?;
            cancel.check()?;
            let reference = store.write(&format!("{prefix}/manifest.json"), &analysis)?;
            track.active_analysis = Some(reference);
            store.save_track(&track)?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                job.state = JobState::Completed;
                job.stage = "complete".into();
                report.count("completed");
            }
            Err(e) => {
                job.state = match e.code.as_str() {
                    "CANCELLED" => JobState::Cancelled,
                    "SOURCE_CHANGED" | "SOURCE_SETTLING" => JobState::Deferred,
                    _ => JobState::Failed,
                };
                job.diagnostic = Some(e.to_string());
                report.count(if job.state == JobState::Deferred {
                    "deferred"
                } else {
                    "failed"
                });
                report.error(&track.track_id, &e);
                if e.code == "CANCELLED" {
                    store.save_job(&job)?;
                    store.report(&report)?;
                    return Err(e);
                }
            }
        }
        job.updated_at = now();
        store.save_job(&job)?;
    }
    rebuild_catalog(store)?;
    store.report(&report)?;
    Ok(report)
}

/// Exact-frame reconciliation only. A changed neural grid never silently retargets a correction.
pub fn apply_overrides(
    track: &Track,
    analysis_ref: &FileRef,
    beats: &mut Beats,
    regions: &mut Regions,
    o: &Overrides,
) -> Result<()> {
    if o.operations.is_empty() {
        return Ok(());
    }
    require(
        o.base_audio_hash == track.audio_hash,
        "OVERRIDE_UNRESOLVED",
        "audio changed; review anchored corrections",
    )?;
    let changed = o
        .base_analysis
        .as_ref()
        .is_none_or(|r| r.hash != analysis_ref.hash);
    for op in &o.operations {
        match op {
            Correction::InsertBeat { source_frame } => {
                require(
                    *source_frame < track.format.frames,
                    "OVERRIDE_RANGE",
                    "beat outside audio",
                )?;
                require(
                    !beats.events.iter().any(|b| b.source_frame == *source_frame),
                    "OVERRIDE_UNRESOLVED",
                    "inserted beat already exists",
                )?;
                beats.events.push(Beat {
                    event_id: format!("manual-{source_frame}"),
                    source_frame: *source_frame,
                    score: 1.,
                    local_bpm: None,
                });
            }
            Correction::DeleteBeat { source_frame } => {
                let i = beats
                    .events
                    .iter()
                    .position(|b| b.source_frame == *source_frame)
                    .ok_or_else(|| {
                        Error::new(
                            "OVERRIDE_UNRESOLVED",
                            "deleted beat no longer exists at original frame",
                        )
                    })?;
                let b = beats.events.remove(i);
                beats.downbeat_ids.retain(|x| x != &b.event_id);
            }
            Correction::Downbeat {
                source_frame,
                enabled,
            } => {
                let b = beats
                    .events
                    .iter()
                    .find(|b| b.source_frame == *source_frame)
                    .ok_or_else(|| {
                        Error::new(
                            "OVERRIDE_UNRESOLVED",
                            "downbeat target no longer exists at original frame",
                        )
                    })?;
                beats.downbeat_ids.retain(|x| x != &b.event_id);
                if *enabled {
                    beats.downbeat_ids.push(b.event_id.clone());
                }
            }
            Correction::Meter {
                start_frame,
                end_frame,
                pulses_per_bar,
            } => {
                bounds(*start_frame, *end_frame, track.format.frames)?;
                beats
                    .meter_segments
                    .retain(|m| m.end_frame <= *start_frame || m.start_frame >= *end_frame);
                beats.meter_segments.push(Meter {
                    start_frame: *start_frame,
                    end_frame: *end_frame,
                    pulses_per_bar: *pulses_per_bar,
                    pulse_unit: "user_annotated_pulse".into(),
                });
            }
            Correction::Unsuitable {
                start_frame,
                end_frame,
            } => {
                bounds(*start_frame, *end_frame, track.format.frames)?;
                beats.unreliable_intervals.push(Interval {
                    start_frame: *start_frame,
                    end_frame: *end_frame,
                    reason: "user_unsuitable".into(),
                });
                for r in &mut regions.regions {
                    if r.start_frame < *end_frame && r.end_frame > *start_frame {
                        r.reliable = false;
                        r.exclusion_reasons.push("user_unsuitable".into());
                    }
                }
            }
            Correction::Cue {
                region_id,
                start_frame,
                end_frame,
            } => {
                require(
                    !changed,
                    "OVERRIDE_UNRESOLVED",
                    "cue region identity must be reviewed after reanalysis",
                )?;
                bounds(*start_frame, *end_frame, track.format.frames)?;
                let r = regions
                    .regions
                    .iter_mut()
                    .find(|r| &r.region_id == region_id)
                    .ok_or_else(|| Error::new("OVERRIDE_UNRESOLVED", "cue region missing"))?;
                r.start_frame = *start_frame;
                r.end_frame = *end_frame;
                r.available_overlap_frames = end_frame - start_frame;
            }
            Correction::ExcludeTransition { .. } => {}
        }
    }
    beats.events.sort_by_key(|b| b.source_frame);
    derive_tempi(&mut beats.events, beats.source_sample_rate_hz);
    beats.downbeat_ids.sort_by_key(|id| {
        beats
            .events
            .iter()
            .position(|b| &b.event_id == id)
            .unwrap_or(usize::MAX)
    });
    beats.validate()?;
    regions.validate()?;
    Ok(())
}
