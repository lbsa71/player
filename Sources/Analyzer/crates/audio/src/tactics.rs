//! Deterministic stereo processors for the saved mixing-tactic program.
//!
//! This module owns effect state, not musical decisions. Times are output frames.
//! Input closes at `input_end_frame`; frozen effects also close their input at
//! the freeze boundary. A declared tail is always faded to silence by `end_frame`.
//! Outside a processor's window it is bypassed, so the lane's ownership envelope
//! must still retire an outgoing source. Process before the lane's final gain
//! envelope if that envelope is intended to control both dry audio and its tail.

use mixlib_types::{require, EffectKind, EffectSpec, Error, FilterMode, Result, ScalarPoint};
use signalsmith_stretch::Stretch;
use std::f64::consts::TAU;

const MAX_EFFECTS: usize = 16;
const MAX_DELAY_SECONDS: f64 = 16.;
const MAX_EFFECT_SECONDS: f64 = 300.;

/// Smoothstep interpolation in output-frame time, clamped to the curve ends.
pub fn scalar_value(points: &[ScalarPoint], frame: u64) -> f64 {
    if points.is_empty() {
        return 0.;
    }
    let next = points.partition_point(|p| p.output_frame <= frame);
    if next == 0 {
        return points[0].value;
    }
    if next == points.len() {
        return points[next - 1].value;
    }
    let a = &points[next - 1];
    let b = &points[next];
    let x = (frame - a.output_frame) as f64 / (b.output_frame - a.output_frame) as f64;
    let x = x * x * (3. - 2. * x);
    a.value + x * (b.value - a.value)
}

fn curve(points: &[ScalarPoint], min: f64, max: f64, spec: &EffectSpec) -> Result<()> {
    require(
        !points.is_empty()
            && points.first().unwrap().output_frame == spec.start_frame
            && points.last().unwrap().output_frame == spec.end_frame
            && points
                .windows(2)
                .all(|w| w[0].output_frame < w[1].output_frame)
            && points
                .iter()
                .all(|p| p.value.is_finite() && (min..=max).contains(&p.value)),
        "EFFECT_CURVE",
        "effect curve must cover its complete window with ordered, finite, bounded values",
    )
}

fn finite_sample(sample: [f64; 2]) -> Result<[f32; 2]> {
    require(
        sample
            .iter()
            .all(|v| v.is_finite() && v.abs() <= f32::MAX as f64),
        "EFFECT_NONFINITE",
        "effect produced a nonfinite or unrepresentable sample",
    )?;
    Ok(sample.map(|v| v as f32))
}

#[derive(Clone, Default)]
struct Biquad {
    z1: [f64; 2],
    z2: [f64; 2],
}

impl Biquad {
    // Normalized RBJ low/high-pass coefficients, with linked stereo controls.
    fn tick(&mut self, x: [f64; 2], mode: FilterMode, hz: f64, q: f64, rate: u32) -> [f64; 2] {
        let w = TAU * hz / rate as f64;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2. * q);
        let inv = 1. / (1. + alpha);
        let (b0, b1, b2) = match mode {
            FilterMode::LowPass => ((1. - cos) * 0.5, 1. - cos, (1. - cos) * 0.5),
            FilterMode::HighPass => ((1. + cos) * 0.5, -(1. + cos), (1. + cos) * 0.5),
        };
        let (b0, b1, b2, a1, a2) = (
            b0 * inv,
            b1 * inv,
            b2 * inv,
            -2. * cos * inv,
            (1. - alpha) * inv,
        );
        let mut y = [0.; 2];
        for c in 0..2 {
            y[c] = b0 * x[c] + self.z1[c];
            self.z1[c] = b1 * x[c] - a1 * y[c] + self.z2[c];
            self.z2[c] = b2 * x[c] - a2 * y[c];
            if self.z1[c].abs() < 1e-30 {
                self.z1[c] = 0.;
            }
            if self.z2[c].abs() < 1e-30 {
                self.z2[c] = 0.;
            }
        }
        y
    }
}

#[derive(Clone)]
struct DelayLine {
    samples: Vec<[f64; 2]>,
    cursor: usize,
}

impl DelayLine {
    fn new(frames: usize) -> Self {
        Self {
            samples: vec![[0.; 2]; frames.max(2)],
            cursor: 0,
        }
    }
    fn read(&self, delay: f64) -> [f64; 2] {
        let pos = (self.cursor as f64 - delay).rem_euclid(self.samples.len() as f64);
        let first = pos.floor() as usize;
        let second = (first + 1) % self.samples.len();
        let f = pos - first as f64;
        std::array::from_fn(|c| self.samples[first][c] * (1. - f) + self.samples[second][c] * f)
    }
    fn write(&mut self, x: [f64; 2]) {
        self.samples[self.cursor] = x;
        self.cursor = (self.cursor + 1) % self.samples.len();
    }
    fn clear(&mut self) {
        self.samples.fill([0.; 2]);
        self.cursor = 0;
    }
}

struct Echo {
    line: DelayLine,
    high: Biquad,
    low: Biquad,
}

impl Echo {
    fn tick(
        &mut self,
        input: [f64; 2],
        delay: f64,
        feedback: f64,
        hp: f64,
        lp: f64,
        rate: u32,
    ) -> [f64; 2] {
        let echo = self.line.read(delay);
        let filtered = if hp == 0. {
            echo
        } else {
            self.high.tick(
                echo,
                FilterMode::HighPass,
                hp,
                std::f64::consts::FRAC_1_SQRT_2,
                rate,
            )
        };
        let filtered = self.low.tick(
            filtered,
            FilterMode::LowPass,
            lp,
            std::f64::consts::FRAC_1_SQRT_2,
            rate,
        );
        self.line
            .write(std::array::from_fn(|c| input[c] + filtered[c] * feedback));
        filtered
    }
    fn clear(&mut self) {
        self.line.clear();
        self.high = Biquad::default();
        self.low = Biquad::default();
    }
}

/// Four independent, incommensurate comb delays. Damping is applied outside
/// frozen recirculation so a frozen captured field is bounded and can sustain.
struct Reverb {
    lines: [DelayLine; 4],
    lengths: [usize; 4],
    high: Biquad,
    low: Biquad,
}

impl Reverb {
    fn new(rate: u32) -> Self {
        let lengths = [0.0297, 0.0371, 0.0411, 0.0437].map(|s| (s * rate as f64).round() as usize);
        Self {
            lines: std::array::from_fn(|i| DelayLine::new(lengths[i] + 1)),
            lengths,
            high: Biquad::default(),
            low: Biquad::default(),
        }
    }
    fn tick(
        &mut self,
        input: [f64; 2],
        decay: f64,
        frozen: bool,
        hp: f64,
        lp: f64,
        rate: u32,
    ) -> [f64; 2] {
        let input = if hp == 0. {
            input
        } else {
            self.high.tick(
                input,
                FilterMode::HighPass,
                hp,
                std::f64::consts::FRAC_1_SQRT_2,
                rate,
            )
        };
        let input = self.low.tick(
            input,
            FilterMode::LowPass,
            lp,
            std::f64::consts::FRAC_1_SQRT_2,
            rate,
        );
        let input = if frozen { [0.; 2] } else { input };
        let mut out = [0.; 2];
        for i in 0..4 {
            let delayed = self.lines[i].read(self.lengths[i] as f64);
            let feedback = if frozen {
                1.
            } else {
                10f64.powf(-3. * self.lengths[i] as f64 / (rate as f64 * decay))
            };
            // Alternating signs reduce alignment of the comb resonances. Stereo
            // channels use the same control and delay lengths, preserving mono.
            let sign = if i % 2 == 0 { 1. } else { -1. };
            self.lines[i].write(std::array::from_fn(|c| {
                input[c] * 0.25 + delayed[c] * feedback
            }));
            for c in 0..2 {
                out[c] += sign * delayed[c] * 0.5;
            }
        }
        out
    }
    fn clear(&mut self) {
        for line in &mut self.lines {
            line.clear();
        }
        self.high = Biquad::default();
        self.low = Biquad::default();
    }
}

struct Spiral {
    line: DelayLine,
    pitch: Stretch,
    input: Vec<f32>,
    output: Vec<f32>,
    offset: usize,
    latency_frames: usize,
}

impl Spiral {
    fn new(delay: f64, rate: u32) -> Result<Self> {
        let window = ((rate as f64 * 0.04).ceil() as usize).next_power_of_two();
        let pitch = Stretch::new(2, window, window / 4);
        // Each block is processed once its input is complete; the process call
        // itself has the library's documented input plus output latency.
        let block = 128;
        let latency_frames = pitch.input_latency() + pitch.output_latency() + block;
        require(
            delay > latency_frames as f64 + 1.,
            "EFFECT_SPIRAL_LATENCY",
            "spiral delay is shorter than the pitch shifter's compensated latency",
        )?;
        Ok(Self {
            line: DelayLine::new(delay.ceil() as usize + 2),
            pitch,
            input: Vec::with_capacity(block * 2),
            output: vec![0.; block * 2],
            offset: 0,
            latency_frames,
        })
    }

    fn tick(&mut self, input: [f64; 2], delay: f64, feedback: f64, semitones: f64) -> [f64; 2] {
        let delayed = self.line.read(delay - self.latency_frames as f64);
        self.input
            .extend((0..2).map(|c| (input[c] + delayed[c] * feedback) as f32));
        let shifted = [
            self.output[self.offset] as f64,
            self.output[self.offset + 1] as f64,
        ];
        self.offset += 2;
        if self.offset == self.output.len() {
            self.pitch
                .set_transpose_factor_semitones(semitones as f32, None);
            self.pitch.process(&self.input, &mut self.output);
            self.input.clear();
            self.offset = 0;
        }
        self.line.write(shifted);
        delayed
    }
    fn clear(&mut self) {
        self.line.clear();
        self.pitch.reset();
        self.input.clear();
        self.output.fill(0.);
        self.offset = 0;
    }
}

enum State {
    Filter(Biquad),
    Echo(Echo),
    Reverb(Reverb),
    Gate,
    Noise(u64),
    Spiral(Spiral),
}

struct Processor {
    spec: EffectSpec,
    state: State,
    rate: u32,
}

impl Processor {
    fn new(spec: &EffectSpec, rate: u32) -> Result<Self> {
        spec.validate(rate)?;
        require(
            spec.start_frame < spec.input_end_frame
                && spec.input_end_frame <= spec.end_frame
                && (spec.end_frame - spec.start_frame) as f64 <= rate as f64 * MAX_EFFECT_SECONDS,
            "EFFECT_BOUNDS",
            "effect input/tail bounds are invalid or exceed five minutes",
        )?;
        curve(&spec.wet, 0., 1., spec)?;
        let band = |hp: f64, lp: f64| {
            require(
                hp.is_finite() && lp.is_finite() && hp >= 0. && lp > hp && lp < rate as f64 * 0.5,
                "EFFECT_BAND",
                "effect band must lie below Nyquist with HP below LP",
            )
        };
        let freeze = |at: Option<u64>| {
            require(
                at.is_none_or(|f| spec.start_frame < f && f <= spec.input_end_frame),
                "EFFECT_FREEZE",
                "freeze needs a nonempty capture interval before input closes",
            )
        };
        let state = match &spec.kind {
            EffectKind::Filter {
                mode: _,
                cutoff_hz,
                resonance,
            } => {
                curve(cutoff_hz, 10., rate as f64 * 0.49, spec)?;
                require(
                    resonance.is_finite() && (0.5..=2.).contains(resonance),
                    "EFFECT_RESONANCE",
                    "filter Q must be within 0.5..2",
                )?;
                State::Filter(Biquad::default())
            }
            EffectKind::Delay {
                delay_frames,
                feedback,
                freeze_at,
                high_pass_hz,
                low_pass_hz,
            } => {
                curve(delay_frames, 1., rate as f64 * MAX_DELAY_SECONDS, spec)?;
                require(
                    feedback.is_finite() && (0.0..=0.95).contains(feedback),
                    "EFFECT_FEEDBACK",
                    "delay feedback must be within 0..0.95",
                )?;
                freeze(*freeze_at)?;
                band(*high_pass_hz, *low_pass_hz)?;
                let frames = delay_frames
                    .iter()
                    .map(|p| p.value)
                    .fold(1., f64::max)
                    .ceil() as usize
                    + 2;
                State::Echo(Echo {
                    line: DelayLine::new(frames),
                    high: Biquad::default(),
                    low: Biquad::default(),
                })
            }
            EffectKind::Reverb {
                decay_seconds,
                freeze_at,
                high_pass_hz,
                low_pass_hz,
            } => {
                require(
                    decay_seconds.is_finite() && (0.05..=30.).contains(decay_seconds),
                    "EFFECT_DECAY",
                    "reverb decay must be within 0.05..30 seconds",
                )?;
                freeze(*freeze_at)?;
                band(*high_pass_hz, *low_pass_hz)?;
                State::Reverb(Reverb::new(rate))
            }
            EffectKind::Gate {
                period_frames,
                phase_frames,
                duty_cycle,
                smoothing_frames,
            } => {
                require(
                    *period_frames >= 4
                        && duty_cycle.is_finite()
                        && (0.01..=0.99).contains(duty_cycle)
                        && *phase_frames < *period_frames
                        && *smoothing_frames > 0
                        && *smoothing_frames <= period_frames / 2,
                    "EFFECT_GATE",
                    "invalid gate period, duty cycle or smoothing",
                )?;
                State::Gate
            }
            EffectKind::Noise { seed, gain } => {
                require(
                    gain.is_finite() && (0.0..=0.25).contains(gain),
                    "EFFECT_NOISE",
                    "noise gain must be within 0..0.25",
                )?;
                State::Noise((*seed).max(1))
            }
            EffectKind::Spiral {
                delay_frames,
                feedback,
                pitch_semitones,
            } => {
                require(
                    delay_frames.is_finite()
                        && (1.0..=rate as f64 * MAX_DELAY_SECONDS).contains(delay_frames)
                        && feedback.is_finite()
                        && (0.0..=0.95).contains(feedback),
                    "EFFECT_SPIRAL",
                    "invalid spiral delay or feedback",
                )?;
                curve(pitch_semitones, -24., 24., spec)?;
                State::Spiral(Spiral::new(*delay_frames, rate)?)
            }
        };
        Ok(Self {
            spec: spec.clone(),
            state,
            rate,
        })
    }

    fn process(&mut self, frame: u64, input: [f64; 2]) -> [f64; 2] {
        let s = &self.spec;
        if frame < s.start_frame || frame >= s.end_frame {
            return input;
        }
        let input_open = frame < s.input_end_frame;
        let dry = if input_open { input } else { [0.; 2] };
        let wet = scalar_value(&s.wet, frame);
        let effected = match (&s.kind, &mut self.state) {
            (
                EffectKind::Filter {
                    mode,
                    cutoff_hz,
                    resonance,
                },
                State::Filter(filter),
            ) => filter.tick(
                dry,
                *mode,
                scalar_value(cutoff_hz, frame),
                *resonance,
                self.rate,
            ),
            (
                EffectKind::Delay {
                    delay_frames,
                    feedback,
                    freeze_at,
                    high_pass_hz,
                    low_pass_hz,
                },
                State::Echo(echo),
            ) => {
                let frozen = freeze_at.is_some_and(|at| frame >= at);
                echo.tick(
                    if frozen { [0.; 2] } else { dry },
                    scalar_value(delay_frames, frame),
                    if frozen { 1. } else { *feedback },
                    *high_pass_hz,
                    *low_pass_hz,
                    self.rate,
                )
            }
            (
                EffectKind::Reverb {
                    decay_seconds,
                    freeze_at,
                    high_pass_hz,
                    low_pass_hz,
                },
                State::Reverb(reverb),
            ) => {
                let frozen = freeze_at.is_some_and(|at| frame >= at);
                reverb.tick(
                    if frozen { [0.; 2] } else { dry },
                    *decay_seconds,
                    frozen,
                    *high_pass_hz,
                    *low_pass_hz,
                    self.rate,
                )
            }
            (
                EffectKind::Gate {
                    period_frames,
                    phase_frames,
                    duty_cycle,
                    smoothing_frames,
                },
                State::Gate,
            ) => {
                let phase =
                    ((frame - s.start_frame) % period_frames + phase_frames) % period_frames;
                let open = (*period_frames as f64 * duty_cycle).round() as u64;
                let smooth = (*smoothing_frames)
                    .min(open / 2)
                    .min((period_frames - open) / 2)
                    .max(1);
                // Center the smoothing around each edge. Half-period-offset
                // 50% gates then sum to unity instead of creating rhythmic holes.
                let gain = if phase < smooth {
                    smooth_unit(0.5 + phase as f64 / (2 * smooth) as f64)
                } else if phase < open.saturating_sub(smooth) {
                    1.
                } else if phase < open + smooth {
                    1. - smooth_unit((phase + smooth - open) as f64 / (2 * smooth) as f64)
                } else if phase < period_frames - smooth {
                    0.
                } else {
                    smooth_unit((phase - (period_frames - smooth)) as f64 / (2 * smooth) as f64)
                };
                dry.map(|v| v * gain)
            }
            (EffectKind::Noise { gain, .. }, State::Noise(random)) => {
                *random ^= *random << 13;
                *random ^= *random >> 7;
                *random ^= *random << 17;
                let noise = if input_open {
                    ((*random >> 11) as f64 / ((1u64 << 53) as f64) * 2. - 1.) * gain
                } else {
                    0.
                };
                // Identical noise in each channel preserves a stable phantom image.
                [noise; 2]
            }
            (
                EffectKind::Spiral {
                    delay_frames,
                    feedback,
                    pitch_semitones,
                },
                State::Spiral(spiral),
            ) => spiral.tick(
                dry,
                *delay_frames,
                *feedback,
                scalar_value(pitch_semitones, frame),
            ),
            _ => unreachable!("effect state matches its validated specification"),
        };
        // Declared tails have a bounded 50 ms release. Stateless processors use
        // their written wet automation; silently fading every filter would dip
        // an incoming track just before its full-band restoration.
        let release = if s.end_frame > s.input_end_frame {
            let release_frames = (self.rate as u64 / 20)
                .min(s.end_frame - s.input_end_frame)
                .max(1);
            smooth_unit((s.end_frame - frame - 1) as f64 / release_frames as f64)
        } else {
            1.
        };
        if matches!(s.kind, EffectKind::Noise { .. }) {
            let envelope = smooth_unit((frame - s.start_frame) as f64 / (self.rate as f64 * 0.005))
                * smooth_unit((s.end_frame - frame - 1) as f64 / (self.rate as f64 * 0.005));
            std::array::from_fn(|c| dry[c] + effected[c] * wet * envelope)
        } else {
            std::array::from_fn(|c| (dry[c] * (1. - wet) + effected[c] * wet) * release)
        }
    }

    fn reset(&mut self) {
        match &mut self.state {
            State::Filter(f) => *f = Biquad::default(),
            State::Echo(e) => e.clear(),
            State::Reverb(r) => r.clear(),
            State::Gate => {}
            State::Noise(seed) => {
                if let EffectKind::Noise { seed: initial, .. } = self.spec.kind {
                    *seed = initial.max(1);
                }
            }
            State::Spiral(s) => s.clear(),
        }
    }
}

pub(crate) fn smooth_unit(x: f64) -> f64 {
    let x = x.clamp(0., 1.);
    x * x * (3. - 2. * x)
}

/// One ordered chain for one placement/lane. No state is shared between tracks.
pub struct StereoEffectChain {
    processors: Vec<Processor>,
    last_frame: Option<u64>,
}

impl StereoEffectChain {
    pub fn new(specs: &[EffectSpec], sample_rate_hz: u32) -> Result<Self> {
        require(
            (8000..=192000).contains(&sample_rate_hz) && specs.len() <= MAX_EFFECTS,
            "EFFECT_LIMIT",
            "unsupported effect sample rate or excessive chain length",
        )?;
        if let Some(first) = specs.first() {
            require(
                specs
                    .iter()
                    .all(|s| s.placement == first.placement && s.lane == first.lane),
                "EFFECT_ROUTING",
                "an effect chain must contain exactly one placement and lane",
            )?;
        }
        Ok(Self {
            processors: specs
                .iter()
                .map(|s| Processor::new(s, sample_rate_hz))
                .collect::<Result<_>>()?,
            last_frame: None,
        })
    }

    /// Process consecutive frames, including zero input throughout declared tails.
    /// A jump requires a reset and replay of capture material, never silent reuse
    /// of stale delay/reverb state from an earlier preview.
    pub fn process(&mut self, frame: u64, input: [f32; 2]) -> Result<[f32; 2]> {
        require(
            input.iter().all(|v| v.is_finite()),
            "EFFECT_NONFINITE",
            "nonfinite effect input",
        )?;
        require(
            self.last_frame
                .is_none_or(|previous| previous.checked_add(1) == Some(frame)),
            "EFFECT_DISCONTINUITY",
            "effect processing requires consecutive output frames",
        )?;
        if self.last_frame.is_none() {
            require(
                self.processors
                    .iter()
                    .all(|p| frame <= p.spec.start_frame || frame >= p.spec.end_frame),
                "EFFECT_MISSING_PREROLL",
                "effect rendering started after its capture boundary",
            )?;
        }
        self.last_frame = Some(frame);
        let mut sample = input.map(f64::from);
        for processor in &mut self.processors {
            sample = processor.process(frame, sample);
            if sample.iter().any(|v| !v.is_finite()) {
                return Err(Error::new(
                    "EFFECT_NONFINITE",
                    "nonfinite internal effect state",
                ));
            }
        }
        finite_sample(sample)
    }

    pub fn end_frame(&self) -> Option<u64> {
        self.processors.iter().map(|p| p.spec.end_frame).max()
    }

    pub fn reset(&mut self) {
        for processor in &mut self.processors {
            processor.reset();
        }
        self.last_frame = None;
    }
}
