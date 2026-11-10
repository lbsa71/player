//! Explicit, bounded source traversal for loops and performance gestures.
//!
//! A prepared edit substitutes samples over its declared output interval. It
//! does not mutate the placement's monotonic time map. The caller is responsible
//! for validating the declared transport resume point against subsequent audio.

use crate::tactics::{scalar_value, smooth_unit};
use mixlib_types::{
    require, Cancellation, Result, SourceEditKind, SourceEditSpec, SourceTransport,
};
use signalsmith_stretch::Stretch;
use std::{collections::BTreeMap, f64::consts::PI};

const MAX_EDIT_SECONDS: u64 = 180;
const MAX_CAPTURE_SECONDS: u64 = 60;
const MAX_CACHED_PERIODS: usize = 256;
const MAX_BUFFER_FRAMES: usize = 8_388_608;

pub struct PreparedSourceEdit {
    spec: SourceEditSpec,
    samples: Vec<[f32; 2]>,
}

impl PreparedSourceEdit {
    /// `captured[0]` is exactly `source_start_frame` at source_sample_rate_hz.
    /// Hash and availability checks belong to the renderer before capture.
    pub fn new(
        spec: &SourceEditSpec,
        captured: &[[f32; 2]],
        output_rate: u32,
        cancel: &Cancellation,
    ) -> Result<Self> {
        spec.validate()?;
        require(
            (8000..=192000).contains(&output_rate)
                && (8000..=192000).contains(&spec.source_sample_rate_hz),
            "EDIT_RATE",
            "unsupported edit sample rate",
        )?;
        require(
            spec.source_start_frame < spec.source_end_frame
                && captured.len() as u64 == spec.source_end_frame - spec.source_start_frame
                && captured.len() as u64 <= MAX_CAPTURE_SECONDS * spec.source_sample_rate_hz as u64
                && spec.output_start_frame < spec.output_end_frame
                && spec.output_end_frame - spec.output_start_frame
                    <= MAX_EDIT_SECONDS * output_rate as u64,
            "EDIT_BOUNDS",
            "invalid or oversized source capture or output edit",
        )?;
        require(
            captured.iter().flatten().all(|x| x.is_finite()),
            "EDIT_NONFINITE",
            "captured edit audio is nonfinite",
        )?;
        let count = (spec.output_end_frame - spec.output_start_frame) as usize;
        require(
            count <= MAX_BUFFER_FRAMES && captured.len() <= MAX_BUFFER_FRAMES,
            "EDIT_MEMORY",
            "source edit exceeds the bounded preparation buffer",
        )?;
        require(
            spec.declick_frames > 0 && spec.declick_frames <= count as u64 / 2,
            "EDIT_DECLICK",
            "edit needs a bounded nonzero de-click interval",
        )?;
        if matches!(spec.kind, SourceEditKind::SlipReverse) {
            require(
                matches!(spec.transport, SourceTransport::ContinueUnderlying),
                "EDIT_TRANSPORT",
                "slip must continue underlying transport",
            )?;
        }
        let natural_frames = (captured.len() as f64 * output_rate as f64
            / spec.source_sample_rate_hz as f64)
            .round() as usize;
        let mut samples = vec![[0.; 2]; count];
        match &spec.kind {
            SourceEditKind::Loop { period_frames } => {
                validate_period(*period_frames as f64, natural_frames, output_rate)?;
                let cycle = stretch_cycle(
                    captured,
                    spec.source_sample_rate_hz,
                    output_rate,
                    *period_frames as usize,
                    cancel,
                )?;
                for (i, out) in samples.iter_mut().enumerate() {
                    if i % 65536 == 0 {
                        cancel.check()?;
                    }
                    *out = cycle[i % cycle.len()];
                }
                soften_seams(
                    &mut samples,
                    &(*period_frames as usize..count)
                        .step_by(*period_frames as usize)
                        .collect::<Vec<_>>(),
                    spec.declick_frames as usize,
                );
            }
            SourceEditKind::LoopSequence { periods } => {
                require(
                    periods.len() >= 2
                        && periods[0].output_frame == spec.output_start_frame
                        && periods.last().unwrap().output_frame == spec.output_end_frame
                        && periods
                            .windows(2)
                            .all(|p| p[0].output_frame < p[1].output_frame),
                    "EDIT_PERIODS",
                    "loop periods must cover the edit in output time",
                )?;
                for p in periods {
                    validate_period(p.value, natural_frames, output_rate)?;
                }
                let mut cycles = BTreeMap::<usize, Vec<[f32; 2]>>::new();
                let mut seams = vec![];
                let mut cursor = 0;
                let mut cached_frames = 0;
                while cursor < count {
                    cancel.check()?;
                    let period = scalar_value(periods, spec.output_start_frame + cursor as u64)
                        .round() as usize;
                    if !cycles.contains_key(&period) {
                        require(
                            cycles.len() < MAX_CACHED_PERIODS,
                            "EDIT_PERIOD_LIMIT",
                            "loop automation needs too many unique prepared periods",
                        )?;
                        require(
                            cached_frames + period <= MAX_BUFFER_FRAMES,
                            "EDIT_MEMORY",
                            "loop automation exceeds bounded cycle cache",
                        )?;
                        cycles.insert(
                            period,
                            stretch_cycle(
                                captured,
                                spec.source_sample_rate_hz,
                                output_rate,
                                period,
                                cancel,
                            )?,
                        );
                        cached_frames += period;
                    }
                    let cycle = &cycles[&period];
                    let take = period.min(count - cursor);
                    samples[cursor..cursor + take].copy_from_slice(&cycle[..take]);
                    cursor += take;
                    if cursor < count {
                        seams.push(cursor);
                    }
                }
                soften_seams(&mut samples, &seams, spec.declick_frames as usize);
            }
            SourceEditKind::Reverse | SourceEditKind::SlipReverse => {
                // A reverse flourish runs at natural speed. It may not silently
                // wrap or turn into an extreme time stretch to fill a long span.
                require(
                    count <= natural_frames,
                    "EDIT_REVERSE_BOUNDS",
                    "reverse edit exceeds captured duration",
                )?;
                for (i, out) in samples.iter_mut().enumerate() {
                    if i % 65536 == 0 {
                        cancel.check()?;
                    }
                    let position = captured.len() as f64
                        - 1.
                        - i as f64 * spec.source_sample_rate_hz as f64 / output_rate as f64;
                    *out = sinc_sample(captured, position, 1.);
                }
            }
            SourceEditKind::Brake { initial_speed }
            | SourceEditKind::Backspin { initial_speed } => {
                require(
                    initial_speed.is_finite() && (0.1..=8.).contains(initial_speed),
                    "EDIT_SPEED",
                    "invalid performance speed",
                )?;
                let reverse = matches!(spec.kind, SourceEditKind::Backspin { .. });
                let ratio = spec.source_sample_rate_hz as f64 / output_rate as f64;
                // Integral of linearly descending speed, reaching zero exactly
                // at the edit end. This deliberately changes pitch.
                let distance = initial_speed * count as f64 * ratio * 0.5;
                require(
                    distance <= (captured.len() - 1) as f64,
                    "EDIT_CAPTURE",
                    "brake/backspin requires a longer healthy capture",
                )?;
                for (i, out) in samples.iter_mut().enumerate() {
                    if i % 65536 == 0 {
                        cancel.check()?;
                    }
                    let t = i as f64 / count as f64;
                    let moved = initial_speed * count as f64 * ratio * (t - 0.5 * t * t);
                    let position = if reverse {
                        captured.len() as f64 - 1. - moved
                    } else {
                        moved
                    };
                    let cutoff = (1. / (initial_speed * (1. - t) * ratio).max(1.)).min(1.);
                    *out = sinc_sample(captured, position, cutoff)
                        .map(|v| v * (1. - smooth_unit(t)) as f32);
                }
            }
            SourceEditKind::Scratch { positions } => {
                require(
                    positions.len() >= 3
                        && positions[0].output_frame == spec.output_start_frame
                        && positions.last().unwrap().output_frame == spec.output_end_frame
                        && positions
                            .windows(2)
                            .all(|w| w[0].output_frame < w[1].output_frame)
                        && positions.iter().all(|p| {
                            p.source_frame.is_finite()
                                && p.source_frame >= spec.source_start_frame as f64
                                && p.source_frame < spec.source_end_frame as f64
                        }),
                    "EDIT_SCRATCH",
                    "scratch needs bounded, ordered source-position gestures",
                )?;
                for w in positions.windows(2) {
                    let speed = (w[1].source_frame - w[0].source_frame).abs()
                        / (w[1].output_frame - w[0].output_frame) as f64;
                    require(
                        speed <= 8. * spec.source_sample_rate_hz as f64 / output_rate as f64,
                        "EDIT_SPEED",
                        "scratch gesture exceeds speed limit",
                    )?;
                }
                let mut at = 0;
                for (i, out) in samples.iter_mut().enumerate() {
                    if i % 65536 == 0 {
                        cancel.check()?;
                    }
                    let frame = spec.output_start_frame + i as u64;
                    while at + 2 < positions.len() && positions[at + 1].output_frame <= frame {
                        at += 1;
                    }
                    let a = &positions[at];
                    let b = &positions[at + 1];
                    let t =
                        (frame - a.output_frame) as f64 / (b.output_frame - a.output_frame) as f64;
                    let position = a.source_frame
                        + smooth_unit(t) * (b.source_frame - a.source_frame)
                        - spec.source_start_frame as f64;
                    let speed = 6. * t * (1. - t) * (b.source_frame - a.source_frame).abs()
                        / (b.output_frame - a.output_frame) as f64;
                    *out = sinc_sample(captured, position, (1. / speed.max(1.)).min(1.));
                }
            }
            SourceEditKind::PitchSequence { notes } => {
                require(
                    !notes.is_empty() && notes.len() <= 256,
                    "EDIT_NOTES",
                    "pitch sequence needs a bounded note list",
                )?;
                let mut previous_end = spec.output_start_frame;
                for note in notes {
                    cancel.check()?;
                    require(
                        note.start_frame >= previous_end
                            && note.end_frame > note.start_frame
                            && note.end_frame <= spec.output_end_frame
                            && note.semitones.is_finite()
                            && (-24.0..=24.).contains(&note.semitones)
                            && note.gain.is_finite()
                            && (0.0..=1.).contains(&note.gain),
                        "EDIT_NOTE",
                        "invalid, overlapping or excessive pitch note",
                    )?;
                    previous_end = note.end_frame;
                    let speed = 2f64.powf(note.semitones / 12.) * spec.source_sample_rate_hz as f64
                        / output_rate as f64;
                    let sounding_end = note
                        .end_frame
                        .min(note.start_frame + (captured.len() as f64 / speed).ceil() as u64);
                    for frame in note.start_frame..sounding_end {
                        let local = (frame - note.start_frame) as usize;
                        let position = local as f64 * speed;
                        if position >= captured.len() as f64 {
                            break;
                        }
                        let edge = spec
                            .declick_frames
                            .min((sounding_end - note.start_frame) / 2)
                            .max(1);
                        let gain = note.gain
                            * smooth_unit(local as f64 / edge as f64)
                            * smooth_unit((sounding_end - frame - 1) as f64 / edge as f64);
                        samples[(frame - spec.output_start_frame) as usize] =
                            sinc_sample(captured, position, (1. / speed.max(1.)).min(1.))
                                .map(|v| v * gain as f32);
                    }
                }
            }
        }
        // Edit boundaries are always de-clicked. Ownership decides whether this
        // replaces a lane or is an overlay; no hidden extra full-mix summation.
        for (i, sample) in samples.iter_mut().enumerate() {
            let gain = smooth_unit(i as f64 / spec.declick_frames as f64)
                * smooth_unit((count - i - 1) as f64 / spec.declick_frames as f64);
            for v in sample {
                *v *= gain as f32;
            }
        }
        require(
            samples.iter().flatten().all(|v| v.is_finite()),
            "EDIT_NONFINITE",
            "nonfinite prepared source edit",
        )?;
        Ok(Self {
            spec: spec.clone(),
            samples,
        })
    }

    pub fn sample(&self, output_frame: u64) -> Option<[f32; 2]> {
        if output_frame < self.spec.output_start_frame || output_frame >= self.spec.output_end_frame
        {
            None
        } else {
            Some(self.samples[(output_frame - self.spec.output_start_frame) as usize])
        }
    }

    pub fn spec(&self) -> &SourceEditSpec {
        &self.spec
    }
    pub fn frames(&self) -> &[[f32; 2]] {
        &self.samples
    }
}

fn validate_period(period: f64, natural_frames: usize, rate: u32) -> Result<()> {
    require(
        period.is_finite()
            && period >= rate as f64 * 0.01
            && period <= rate as f64 * 60.
            && (0.125..=8.).contains(&(period / natural_frames.max(1) as f64)),
        "EDIT_PERIOD",
        "loop period exceeds duration or stretch bounds",
    )
}

fn stretch_cycle(
    captured: &[[f32; 2]],
    source_rate: u32,
    output_rate: u32,
    period: usize,
    cancel: &Cancellation,
) -> Result<Vec<[f32; 2]>> {
    cancel.check()?;
    let natural =
        (captured.len() as f64 * output_rate as f64 / source_rate as f64).round() as usize;
    let mut input = Vec::with_capacity(natural * 2);
    for i in 0..natural {
        if i % 65536 == 0 {
            cancel.check()?;
        }
        input.extend(sinc_sample(
            captured,
            i as f64 * source_rate as f64 / output_rate as f64,
            (output_rate as f64 / source_rate as f64).min(1.),
        ));
    }
    if period == natural {
        return Ok(input.chunks_exact(2).map(|x| [x[0], x[1]]).collect());
    }
    let mut stretch = Stretch::preset_default(2, output_rate);
    // Short rolls still need pitch-preserving stretching. Extend the periodic
    // input/output beyond the engine's latency, then take an interior cycle.
    let repeats = ((stretch.output_latency() * 4).div_ceil(period)).max(3);
    require(
        repeats <= 1024
            && period
                .checked_mul(repeats)
                .is_some_and(|n| n <= MAX_BUFFER_FRAMES)
            && natural
                .checked_mul(repeats)
                .is_some_and(|n| n <= MAX_BUFFER_FRAMES),
        "EDIT_STRETCH",
        "loop exceeds bounded stretch preparation",
    )?;
    let mut repeated = Vec::with_capacity(input.len() * repeats);
    for _ in 0..repeats {
        repeated.extend_from_slice(&input);
    }
    let mut output = vec![0.; period * repeats * 2];
    require(
        stretch.exact(&repeated, &mut output),
        "EDIT_STRETCH",
        "time-stretch engine rejected loop duration",
    )?;
    cancel.check()?;
    let start = (repeats / 2) * period * 2;
    Ok(output[start..start + period * 2]
        .chunks_exact(2)
        .map(|v| [v[0], v[1]])
        .collect())
}

fn soften_seams(samples: &mut [[f32; 2]], seams: &[usize], requested: usize) {
    // Correct a local step across each verified loop seam without changing the
    // period. The correction vanishes smoothly on both sides of the seam.
    for (index, &at) in seams.iter().enumerate() {
        let previous = if index == 0 { 0 } else { seams[index - 1] };
        let next = seams.get(index + 1).copied().unwrap_or(samples.len());
        let width = requested.min((at - previous) / 2).min((next - at) / 2);
        if width == 0 || at == 0 || at >= samples.len() {
            continue;
        }
        let difference = std::array::from_fn::<_, 2, _>(|c| samples[at][c] - samples[at - 1][c]);
        for i in 0..width {
            let weight = (1. - smooth_unit(i as f64 / width as f64)) as f32 * 0.5;
            for (c, difference) in difference.iter().enumerate() {
                samples[at - 1 - i][c] += difference * weight;
                samples[at + i][c] -= difference * weight;
            }
        }
    }
}

fn sinc_sample(samples: &[[f32; 2]], position: f64, cutoff: f64) -> [f32; 2] {
    if position < 0. || position > (samples.len() - 1) as f64 {
        return [0.; 2];
    }
    let center = position.floor() as i64;
    let mut out = [0.; 2];
    let mut norm = 0.;
    for k in -15..=16 {
        let at = center + k;
        let distance = position - at as f64;
        if distance.abs() >= 16. {
            continue;
        }
        let x = distance * cutoff;
        let sinc = if x.abs() < 1e-12 {
            1.
        } else {
            (PI * x).sin() / (PI * x)
        };
        let weight = sinc * cutoff * (0.5 + 0.5 * (PI * distance / 16.).cos());
        norm += weight;
        if at >= 0 && at < samples.len() as i64 {
            for (c, value) in out.iter_mut().enumerate() {
                *value += samples[at as usize][c] as f64 * weight;
            }
        }
    }
    if norm.abs() < 1e-12 {
        return [0.; 2];
    }
    out.map(|v| (v / norm) as f32)
}
