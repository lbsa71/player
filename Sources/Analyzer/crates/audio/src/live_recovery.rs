//! Bounded live fault recovery from PCM verified before a runtime failure.
//!
//! This API is deliberately separate from offline plan rendering. Preparation
//! allocates and may run source-edit DSP; run it on the playback control thread.
//! `render_into` only copies prepared samples, checks cancellation and updates
//! the audit. It never opens, decodes or captures a failed source.
use crate::{tactics::smooth_unit, PreparedSourceEdit};
use mixlib_types::{
    require, Cancellation, EmergencyActivation, EmergencyReason, Error, Result, SourceEditKind,
    SourceEditSpec, SourcePositionPoint, SourceTransport, StemLane, TacticId, MAX_FRAME,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

const MAX_CACHE_SECONDS: u64 = 10;
const MAX_BRIDGE_SECONDS: u64 = 2;

/// Digest domain includes the original rate, source-frame origin and exact
/// interleaved little-endian f32 samples. A decoder/cache owner records it while
/// the material is healthy, then supplies that expected digest at activation.
pub fn recovery_pcm_digest(rate: u32, source_start: u64, pcm: &[[f32; 2]]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"mixlib/live-recovery-pcm/1\0");
    hash.update(rate.to_le_bytes());
    hash.update(source_start.to_le_bytes());
    for frame in pcm {
        for sample in frame {
            hash.update(sample.to_le_bytes());
        }
    }
    format!("{:x}", hash.finalize())
}

/// Immutable, bounded decoded PCM. There is no constructor accepting a mere
/// `healthy=true` flag, and callers cannot mutate bytes after verification.
pub struct VerifiedRecoveryPcm {
    source_id: String,
    rate: u32,
    source_start: u64,
    pcm: Vec<[f32; 2]>,
    digest: String,
}
impl VerifiedRecoveryPcm {
    pub fn verify(
        source_id: String,
        rate: u32,
        source_start: u64,
        pcm: Vec<[f32; 2]>,
        expected_digest: &str,
        cancel: &Cancellation,
    ) -> Result<Self> {
        cancel.check()?;
        require(
            !source_id.trim().is_empty() && source_id.len() <= 1024,
            "RECOVERY_SOURCE",
            "cached PCM needs a bounded source identifier",
        )?;
        require(
            (8000..=192000).contains(&rate)
                && pcm.len() >= 2
                && pcm.len() as u64 <= MAX_CACHE_SECONDS * rate as u64
                && source_start <= MAX_FRAME.saturating_sub(pcm.len() as u64),
            "RECOVERY_BOUNDS",
            "cached PCM rate, source span or duration is invalid",
        )?;
        for chunk in pcm.chunks(65536) {
            cancel.check()?;
            require(
                chunk
                    .iter()
                    .flatten()
                    .all(|v| v.is_finite() && v.abs() <= 8.),
                "RECOVERY_PCM",
                "cached PCM contains nonfinite or unreasonable samples",
            )?;
        }
        let digest = recovery_pcm_digest(rate, source_start, &pcm);
        require(
            digest == expected_digest,
            "RECOVERY_HASH",
            "cached PCM differs from its healthy digest",
        )?;
        Ok(Self {
            source_id,
            rate,
            source_start,
            pcm,
            digest,
        })
    }
    fn end(&self) -> u64 {
        self.source_start + self.pcm.len() as u64
    }
}

/// Emitted by the live transport/decoder at the exact failed output cursor.
/// There is intentionally no compatibility-score or offline-planning reason.
#[derive(Clone, Debug, Serialize)]
pub struct LiveSourceFailure {
    pub reason: EmergencyReason,
    pub sample_rate_hz: u32,
    pub source_id: String,
    pub source_frame: u64,
    pub output_frame: u64,
    pub failed_condition: String,
}

/// PCM is already verified at a prevalidated entry. Its landing frame is the
/// earliest safe transport slot, not a delay invented to justify an effect.
pub struct ReadyRecoverySource {
    pub pcm: VerifiedRecoveryPcm,
    pub landing_output_frame: u64,
}

#[derive(Clone, Debug)]
pub struct LiveRecoveryPolicy {
    /// Empty by default. Only E01, E02 and E03 are accepted, in preference order.
    pub permitted_motion: Vec<TacticId>,
    /// Maximum silence accepted before the already verified next entry.
    pub maximum_silence_frames: u64,
    /// A short de-click release, between 1 ms and 50 ms.
    pub fade_frames: u64,
}
impl LiveRecoveryPolicy {
    pub fn conservative(rate: u32) -> Self {
        Self {
            permitted_motion: vec![],
            maximum_silence_frames: rate as u64 / 20,
            fade_frames: rate as u64 / 100,
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LiveRecoveryRoute {
    PreparedContinuation,
    MinimalFade,
    Motion(TacticId),
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LiveRecoveryState {
    Ready,
    Playing,
    Completed,
    Cancelled,
    CachedNextExhausted,
}

#[derive(Clone, Debug, Serialize)]
pub struct LiveRecoveryAudit {
    pub method: String,
    pub route: LiveRecoveryRoute,
    pub failure: LiveSourceFailure,
    pub activation: Option<EmergencyActivation>,
    pub rejected_safe_recoveries: Vec<String>,
    pub healthy_capture_digest: Option<String>,
    pub next_source_id: String,
    pub next_pcm_digest: String,
    pub next_landing_output_frame: u64,
    pub output_end_frame: u64,
    pub next_source_resume_frame: u64,
    pub motion: Option<SourceEditSpec>,
    pub vocal_guard_frames: u64,
    pub applied_gain: f64,
    pub peak: f64,
    pub state: LiveRecoveryState,
    pub rendered_frames: u64,
    pub next_source_loss: Option<String>,
}

pub struct LiveRecoveryRenderer {
    samples: Vec<[f32; 2]>,
    cursor: usize,
    fade_frames: usize,
    release_start: Option<usize>,
    audit: LiveRecoveryAudit,
}
impl LiveRecoveryRenderer {
    /// Select and prepare a fallback solely from verified resident PCM. A live
    /// caller must pass the actual failure, not synthesize one during planning.
    pub fn activate(
        failure: LiveSourceFailure,
        healthy: Option<&VerifiedRecoveryPcm>,
        next: ReadyRecoverySource,
        policy: &LiveRecoveryPolicy,
        cancel: &Cancellation,
    ) -> Result<Self> {
        cancel.check()?;
        let rate = next.pcm.rate;
        require(
            failure.sample_rate_hz == rate
                && !failure.source_id.trim().is_empty()
                && !failure.failed_condition.trim().is_empty()
                && failure.failed_condition.len() <= 4096
                && failure.source_frame <= MAX_FRAME
                && failure.output_frame <= MAX_FRAME,
            "RECOVERY_FAILURE",
            "missing concrete live failure or incompatible output timebase",
        )?;
        require(
            policy.fade_frames >= (rate as u64 / 1000).max(1)
                && policy.fade_frames <= rate as u64 / 20
                && policy.maximum_silence_frames <= rate as u64 / 4
                && policy.permitted_motion.len() <= 3
                && policy
                    .permitted_motion
                    .iter()
                    .all(|id| matches!(id, TacticId::E01 | TacticId::E02 | TacticId::E03)),
            "RECOVERY_POLICY",
            "invalid live recovery bounds or nonemergency motion",
        )?;
        let landing = next.landing_output_frame;
        require(
            landing >= failure.output_frame
                && landing - failure.output_frame <= MAX_BRIDGE_SECONDS * rate as u64
                && landing <= MAX_FRAME.saturating_sub(next.pcm.pcm.len() as u64),
            "RECOVERY_LANDING",
            "next source has no bounded future landing",
        )?;
        if let Some(cache) = healthy {
            require(
                cache.source_id == failure.source_id && cache.rate == rate,
                "RECOVERY_SOURCE",
                "healthy cache belongs to another source or timebase",
            )?;
        }
        let bridge = (landing - failure.output_frame) as usize;
        let forward = healthy
            .and_then(|h| {
                (h.source_start <= failure.source_frame && failure.source_frame < h.end())
                    .then(|| &h.pcm[(failure.source_frame - h.source_start) as usize..])
            })
            .unwrap_or(&[]);
        let history = healthy
            .and_then(|h| {
                (h.source_start < failure.source_frame && failure.source_frame <= h.end())
                    .then(|| &h.pcm[..(failure.source_frame - h.source_start) as usize])
            })
            .unwrap_or(&[]);
        let mut rejected = vec![];
        let mut motion = None;
        let guard = (rate as u64 * 3 / 10) as usize;
        let route = if bridge > 0 && forward.len() >= bridge {
            LiveRecoveryRoute::PreparedContinuation
        } else {
            rejected.push(if bridge == 0 {
                "prepared continuation is unnecessary: the verified fallback can take ownership now"
                    .into()
            } else {
                format!(
                    "prepared continuation has {} verified forward frames; {} required",
                    forward.len(),
                    bridge
                )
            });
            if bridge.saturating_sub(forward.len()) as u64 <= policy.maximum_silence_frames {
                LiveRecoveryRoute::MinimalFade
            } else {
                rejected.push(format!("minimal fade would leave {} silent frames before the verified landing; policy permits {}",
                    bridge.saturating_sub(forward.len()), policy.maximum_silence_frames));
                require(
                    !history.is_empty()
                        && !policy.permitted_motion.is_empty()
                        && bridge >= guard + (rate as usize / 20),
                    "RECOVERY_UNAVAILABLE",
                    "no safe continuation/minimal fade and no permitted healthy motion capture",
                )?;
                let duration = bridge - guard;
                let (id, needed) = policy.permitted_motion.iter().find_map(|id| {
                    let needed = match id {
                        TacticId::E01 | TacticId::E03 => duration / 2 + 2,
                        TacticId::E02 => duration + 2,
                        _ => unreachable!(),
                    };
                    (history.len() >= needed).then_some((*id, needed))
                }).ok_or_else(|| Error::new("RECOVERY_CAPTURE", "healthy pre-failure capture cannot cover any permitted emergency motion"))?;
                let source_end = failure.source_frame;
                let source_start = source_end - needed as u64;
                let end = failure.output_frame + duration as u64;
                let kind = match id {
                    TacticId::E01 => SourceEditKind::Brake { initial_speed: 1. },
                    TacticId::E02 => SourceEditKind::Backspin { initial_speed: 2. },
                    TacticId::E03 => {
                        let a = source_start as f64;
                        let b = (source_end - 1) as f64;
                        SourceEditKind::Scratch {
                            positions: vec![
                                SourcePositionPoint {
                                    output_frame: failure.output_frame,
                                    source_frame: b,
                                },
                                SourcePositionPoint {
                                    output_frame: failure.output_frame + duration as u64 / 3,
                                    source_frame: a,
                                },
                                SourcePositionPoint {
                                    output_frame: failure.output_frame + duration as u64 * 2 / 3,
                                    source_frame: b,
                                },
                                SourcePositionPoint {
                                    output_frame: end,
                                    source_frame: a + (b - a) * 0.5,
                                },
                            ],
                        }
                    }
                    _ => unreachable!(),
                };
                let spec = SourceEditSpec {
                    placement: 0,
                    lane: StemLane::Stereo,
                    source_sample_rate_hz: rate,
                    source_start_frame: source_start,
                    source_end_frame: source_end,
                    output_start_frame: failure.output_frame,
                    output_end_frame: end,
                    declick_frames: policy.fade_frames.min(duration as u64 / 2),
                    transport: SourceTransport::ContinueUnderlying,
                    kind,
                };
                let edit = PreparedSourceEdit::new(
                    &spec,
                    &history[history.len() - needed..],
                    rate,
                    cancel,
                )?;
                motion = Some((spec, edit));
                LiveRecoveryRoute::Motion(id)
            }
        };
        let mut samples = vec![[0.; 2]; bridge + next.pcm.pcm.len()];
        if let Some((_, edit)) = &motion {
            for (i, out) in samples[..bridge].iter_mut().enumerate() {
                if i % 65536 == 0 {
                    cancel.check()?;
                }
                *out = edit
                    .sample(failure.output_frame + i as u64)
                    .unwrap_or([0.; 2]);
            }
        } else {
            let take = bridge.min(forward.len());
            samples[..take].copy_from_slice(&forward[..take]);
            let fade = take.min(policy.fade_frames as usize);
            for i in 0..fade {
                let gain = (1. - smooth_unit((i + 1) as f64 / fade as f64)) as f32;
                samples[take - fade + i] = samples[take - fade + i].map(|s| s * gain);
            }
        }
        samples[bridge..].copy_from_slice(&next.pcm.pcm);
        let fade = next.pcm.pcm.len().min(policy.fade_frames as usize);
        for i in 0..fade {
            let gain = smooth_unit(i as f64 / fade as f64) as f32;
            samples[bridge + i] = samples[bridge + i].map(|s| s * gain);
        }
        let peak = samples.iter().flatten().fold(0f32, |p, v| p.max(v.abs())) as f64;
        let gain = if peak > 10f64.powf(-1. / 20.) {
            10f64.powf(-1. / 20.) / peak
        } else {
            1.
        };
        if gain < 1. {
            for chunk in samples.chunks_mut(65536) {
                cancel.check()?;
                for sample in chunk.iter_mut().flatten() {
                    *sample *= gain as f32;
                }
            }
        }
        let activation =
            (route != LiveRecoveryRoute::PreparedContinuation).then(|| EmergencyActivation {
                reason: failure.reason,
                activated_output_frame: failure.output_frame,
                failed_condition: failure.failed_condition.clone(),
                rejected_safe_recoveries: rejected.clone(),
                next_source_verified: true,
                healthy_buffer_frames: history.len() as u64,
            });
        let audit = LiveRecoveryAudit {
            method: "bounded_live_pcm_recovery/1".into(),
            route,
            output_end_frame: landing + next.pcm.pcm.len() as u64,
            next_source_resume_frame: next.pcm.end(),
            failure,
            activation,
            rejected_safe_recoveries: rejected,
            healthy_capture_digest: healthy.map(|h| h.digest.clone()),
            next_source_id: next.pcm.source_id,
            next_pcm_digest: next.pcm.digest,
            next_landing_output_frame: landing,
            vocal_guard_frames: if motion.is_some() { guard as u64 } else { 0 },
            motion: motion.map(|(spec, _)| spec),
            applied_gain: gain,
            peak: peak * gain,
            state: LiveRecoveryState::Ready,
            rendered_frames: 0,
            next_source_loss: None,
        };
        Ok(Self {
            samples,
            cursor: 0,
            fade_frames: policy.fade_frames as usize,
            release_start: None,
            audit,
        })
    }

    pub fn audit(&self) -> &LiveRecoveryAudit {
        &self.audit
    }

    /// The cached bytes remain healthy even if their original file disappears.
    /// Drain that verified cache, then report exhaustion instead of recapturing
    /// the lost file or pretending the normal transport can resume there.
    pub fn notify_next_source_loss(&mut self, failed_condition: String) -> Result<()> {
        require(
            !failed_condition.trim().is_empty() && failed_condition.len() <= 4096,
            "RECOVERY_FAILURE",
            "next-source loss needs a concrete failure",
        )?;
        require(
            matches!(
                self.audit.state,
                LiveRecoveryState::Ready | LiveRecoveryState::Playing
            ),
            "RECOVERY_FINISHED",
            "recovery no longer owns the live transport",
        )?;
        self.audit.next_source_loss = Some(failed_condition);
        self.release_start.get_or_insert(
            self.samples
                .len()
                .saturating_sub(self.fade_frames)
                .max(self.cursor),
        );
        Ok(())
    }

    /// Returns the populated frame count; the remaining output is cleared.
    /// Successful completion hands back the exact `next_source_resume_frame`.
    /// No arbitrary seeks, new decoder reads or unbounded wet tails are possible.
    pub fn render_into(&mut self, output: &mut [[f32; 2]], cancel: &Cancellation) -> Result<usize> {
        output.fill([0.; 2]);
        if let Err(error) = cancel.check() {
            self.audit.state = LiveRecoveryState::Cancelled;
            return Err(error);
        }
        if self.audit.state == LiveRecoveryState::Cancelled {
            return Err(Error::new("CANCELLED", "live recovery was cancelled"));
        }
        let count = output.len().min(self.samples.len() - self.cursor);
        output[..count].copy_from_slice(&self.samples[self.cursor..self.cursor + count]);
        if let Some(fade_start) = self.release_start {
            // A disconnected upstream cannot supply the ordinary next sample
            // after this cache. Release the known final frames to silence.
            let fade = self.samples.len() - fade_start;
            for (i, frame) in output[..count].iter_mut().enumerate() {
                if self.cursor + i >= fade_start {
                    let gain = (1.
                        - smooth_unit((self.cursor + i - fade_start + 1) as f64 / fade as f64))
                        as f32;
                    *frame = frame.map(|v| v * gain);
                }
            }
        }
        self.cursor += count;
        self.audit.rendered_frames = self.cursor as u64;
        self.audit.state = if self.cursor == self.samples.len() {
            if self.audit.next_source_loss.is_some() {
                LiveRecoveryState::CachedNextExhausted
            } else {
                LiveRecoveryState::Completed
            }
        } else {
            LiveRecoveryState::Playing
        };
        Ok(count)
    }
}
