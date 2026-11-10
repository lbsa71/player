//! Executes saved lane schedules over the same source/time maps as legacy mixes.
use crate::tactics::scalar_value;
use crate::{render::Voice, PreparedSourceEdit, StereoEffectChain, WavReader};
use mixlib_types::*;
use std::{collections::BTreeMap, path::PathBuf};

struct Lane {
    schedule: LaneSchedule,
    decoder: Option<Voice>,
    edits: Vec<PreparedSourceEdit>,
    effects: StereoEffectChain,
    low: [f64; 2],
    upper: [f64; 2],
}
struct Window {
    start: u64,
    end: u64,
    tail_end: u64,
    base_level: f64,
    lanes: Vec<Lane>,
}
pub(crate) struct TacticVoice {
    legacy: Voice,
    source_end_output: u64,
    end: u64,
    windows: Vec<Window>,
    alpha: [f64; 2],
}

impl TacticVoice {
    pub(crate) fn new(
        legacy: Voice,
        index: usize,
        plan: &MixPlan,
        tactics: &[TacticPlan],
        sources: &BTreeMap<String, PathBuf>,
        cancel: &Cancellation,
    ) -> Result<Self> {
        let p = &plan.placements[index];
        let rate = plan.output.sample_rate_hz;
        let mut windows = Vec::new();
        for tactic in tactics
            .iter()
            .filter(|t| [t.outgoing_placement, t.incoming_placement].contains(&index))
        {
            let schedules: Vec<_> = tactic
                .lanes
                .iter()
                .filter(|l| l.placement == index)
                .collect();
            require(
                !schedules.is_empty(),
                "TACTIC_LANE",
                "each participating placement needs an explicit lane schedule",
            )?;
            let mut lanes = Vec::new();
            let base_level = p
                .automation
                .iter()
                .filter(|a| !a.mute)
                .map(|a| a.gain_linear)
                .fold(0., f64::max);
            for schedule in schedules {
                cancel.check()?;
                let asset = tactic
                    .stem_assets
                    .iter()
                    .find(|a| a.placement == index && a.lane == schedule.lane);
                let (path, offset, capture_end) = if schedule.lane == StemLane::Stereo {
                    (&sources[&p.track_id], 0, p.source_end_frame)
                } else {
                    let asset = asset.ok_or_else(|| {
                        Error::new("TACTIC_STEM", "isolated lane has no verified audio asset")
                    })?;
                    let path = sources
                        .get(&asset.audio.path)
                        .ok_or_else(|| Error::new("TACTIC_STEM", &asset.audio.path))?;
                    require(
                        asset.source_offset_frame as f64
                            <= p.time_map.source_at(tactic.start_frame as f64)
                            && asset.source_end_frame as f64
                                >= p.time_map.source_at(tactic.end_frame as f64),
                        "TACTIC_STEM",
                        "stem capture does not cover the lane schedule",
                    )?;
                    let wav = WavReader::open(path)?;
                    require(
                        wav.format.sample_rate_hz == p.time_map.source_sample_rate_hz
                            && wav.format.frames
                                >= asset.source_end_frame - asset.source_offset_frame,
                        "TACTIC_STEM",
                        "stem WAV does not cover its declared source timebase and capture",
                    )?;
                    (path, asset.source_offset_frame, asset.source_end_frame)
                };
                let decoder = if schedule.lane == StemLane::Stereo {
                    None
                } else {
                    Some(Voice::new_at(
                        p,
                        path,
                        rate,
                        plan.crossover_hz,
                        vec![],
                        offset,
                        tactic.start_frame,
                        true,
                    )?)
                };
                let mut edits = Vec::new();
                for edit in tactic
                    .source_edits
                    .iter()
                    .filter(|e| e.placement == index && e.lane == schedule.lane)
                {
                    require(
                        edit.source_start_frame >= offset && edit.source_end_frame <= capture_end,
                        "TACTIC_EDIT",
                        "edit source lies outside its lane capture",
                    )?;
                    if let SourceTransport::ResumeAt { source_frame } = edit.transport {
                        require(
                            (p.time_map.source_at(edit.output_end_frame as f64)
                                - source_frame as f64)
                                .abs()
                                <= 1.,
                            "TACTIC_TRANSPORT",
                            "source edit resume point does not match the saved subsequent time map",
                        )?;
                    }
                    let mut wav = WavReader::open(path)?;
                    require(
                        wav.format.sample_rate_hz == edit.source_sample_rate_hz,
                        "TACTIC_EDIT",
                        "edit capture sample rate changed",
                    )?;
                    wav.seek_frame(edit.source_start_frame - offset)?;
                    let count = edit.source_end_frame - edit.source_start_frame;
                    require(
                        count <= u64::from(edit.source_sample_rate_hz) * 60,
                        "TACTIC_EDIT",
                        "edit capture exceeds sixty seconds",
                    )?;
                    let mut captured = Vec::with_capacity(count as usize);
                    while captured.len() < count as usize {
                        cancel.check()?;
                        let block =
                            wav.read_frames((count as usize - captured.len()).min(65536))?;
                        if block.is_empty() {
                            break;
                        }
                        captured.extend(block);
                    }
                    require(
                        captured.len() as u64 == count,
                        "TACTIC_EDIT",
                        "truncated source edit capture",
                    )?;
                    edits.push(PreparedSourceEdit::new(edit, &captured, rate, cancel)?);
                }
                let effects: Vec<_> = tactic
                    .effects
                    .iter()
                    .filter(|e| e.placement == index && e.lane == schedule.lane)
                    .cloned()
                    .collect();
                lanes.push(Lane {
                    schedule: schedule.clone(),
                    decoder,
                    edits,
                    effects: StereoEffectChain::new(&effects, rate)?,
                    low: [0.; 2],
                    upper: [0.; 2],
                });
            }
            let tail_end = tactic
                .effects
                .iter()
                .filter(|e| e.placement == index)
                .map(|e| e.end_frame)
                .max()
                .unwrap_or(tactic.end_frame)
                .max(tactic.end_frame);
            windows.push(Window {
                start: tactic.start_frame,
                end: tactic.end_frame,
                tail_end,
                base_level,
                lanes,
            });
        }
        windows.sort_by_key(|w| w.start);
        require(
            windows.windows(2).all(|w| w[0].end <= w[1].start),
            "TACTIC_OVERLAP",
            "multiple ownership schedules overlap on one placement",
        )?;
        let source_end_output = p.time_map.anchors.last().unwrap().output_frame;
        let end = windows
            .iter()
            .map(|w| w.tail_end)
            .max()
            .unwrap_or(source_end_output)
            .max(source_end_output);
        Ok(Self {
            legacy,
            source_end_output,
            end,
            windows,
            alpha: plan
                .crossover_hz
                .map(|f| 1. - (-2. * std::f64::consts::PI * f / f64::from(rate)).exp()),
        })
    }
    pub(crate) fn end_frame(&self) -> u64 {
        self.end
    }
    pub(crate) fn next(&mut self, frame: u64) -> Result<[f32; 2]> {
        let raw = if frame < self.source_end_output {
            self.legacy.next_raw()?
        } else {
            [0.; 2]
        };
        let baseline = if frame < self.source_end_output {
            self.legacy.automated(frame, raw)
        } else {
            [0.; 2]
        };
        let controlled = self
            .windows
            .iter()
            .any(|w| frame >= w.start && frame < w.end);
        let mut output = if controlled { [0.; 2] } else { baseline };
        for window in self
            .windows
            .iter_mut()
            .filter(|w| frame >= w.start && frame < w.tail_end)
        {
            for lane in &mut window.lanes {
                let active = frame < window.end;
                let mut sample = if active {
                    if let Some(decoder) = &mut lane.decoder {
                        decoder.next_raw()?
                    } else {
                        raw
                    }
                } else {
                    [0.; 2]
                };
                if active {
                    for edit in &lane.edits {
                        let spec = edit.spec();
                        if frame >= spec.output_start_frame && frame < spec.output_end_frame {
                            let edited = edit.sample(frame).ok_or_else(|| {
                                Error::new("TACTIC_EDIT", "missing prepared edit sample")
                            })?;
                            // Prepared edit audio already has a de-click envelope.
                            // Retain the underlying signal through its complementary edge.
                            let edge = (frame - spec.output_start_frame)
                                .min(spec.output_end_frame - 1 - frame)
                                as f64
                                / spec.declick_frames as f64;
                            let t = edge.clamp(0., 1.);
                            let blend = t * t * (3. - 2. * t);
                            for c in 0..2 {
                                sample[c] = edited[c] + sample[c] * (1. - blend) as f32;
                            }
                        }
                    }
                    let gain = scalar_value(&lane.schedule.gain, frame);
                    let bands = band_value(&lane.schedule.eq, frame);
                    for (c, value) in sample.iter_mut().enumerate() {
                        let x = f64::from(*value);
                        lane.low[c] += self.alpha[0] * (x - lane.low[c]);
                        lane.upper[c] += self.alpha[1] * (x - lane.upper[c]);
                        *value = ((lane.low[c] * bands[0]
                            + (lane.upper[c] - lane.low[c]) * bands[1]
                            + (x - lane.upper[c]) * bands[2])
                            * gain) as f32;
                    }
                }
                // Feed ownership/gain before the effect. Captured wet audio can
                // continue after its input lane closes, within the declared tail.
                let processed = lane.effects.process(frame, sample)?;
                for c in 0..2 {
                    output[c] += processed[c] * window.base_level as f32;
                }
            }
        }
        Ok(output)
    }
}

fn band_value(points: &[BandGainPoint], frame: u64) -> [f64; 3] {
    if points.is_empty() {
        return [1.; 3];
    }
    let i = points
        .partition_point(|p| p.output_frame <= frame)
        .saturating_sub(1);
    let a = &points[i];
    let b = &points[(i + 1).min(points.len() - 1)];
    let t = if a.output_frame == b.output_frame {
        0.
    } else {
        (frame.saturating_sub(a.output_frame) as f64 / (b.output_frame - a.output_frame) as f64)
            .clamp(0., 1.)
    };
    let t = t * t * (3. - 2. * t);
    std::array::from_fn(|c| a.gains[c] + (b.gains[c] - a.gains[c]) * t)
}
