use crate::WavReader;
use mixlib_types::*;
use signalsmith_stretch::Stretch;
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
use tempfile::NamedTempFile;

pub const RENDERER: &str = "signalsmith-0.1.3-sinc32-eq1/1";
/// Random-access, bounded resampling cache. 32-tap Hann-windowed sinc, explicit anti-alias cutoff.
struct Resampled {
    wav: WavReader,
    output_rate: u32,
    start: i64,
    cache: Vec<[f32; 2]>,
}
impl Resampled {
    fn at(&mut self, frame: i64) -> Result<[f32; 2]> {
        if frame < 0 || frame >= self.wav.format.frames as i64 {
            return Ok([0.; 2]);
        }
        if frame < self.start || frame >= self.start + self.cache.len() as i64 {
            self.start = frame.saturating_sub(32).max(0);
            self.wav.seek_frame(self.start as u64)?;
            self.cache = self.wav.read_frames(16384)?;
        }
        Ok(self.cache[(frame - self.start) as usize])
    }
    fn sample(&mut self, frame: i64) -> Result<[f32; 2]> {
        if self.output_rate == self.wav.format.sample_rate_hz {
            return self.at(frame);
        }
        let x = frame as f64 * self.wav.format.sample_rate_hz as f64 / self.output_rate as f64;
        let cutoff = (self.output_rate as f64 / self.wav.format.sample_rate_hz as f64).min(1.);
        let center = x.floor() as i64;
        let mut sum = [0.; 2];
        let mut norm = 0.;
        for k in -15..=16 {
            let i = center + k;
            let d = x - i as f64;
            let w =
                sinc(d * cutoff) * cutoff * (0.5 + 0.5 * (std::f64::consts::PI * d / 16.).cos());
            let s = self.at(i)?;
            norm += w;
            for c in 0..2 {
                sum[c] += s[c] as f64 * w;
            }
        }
        Ok([(sum[0] / norm) as f32, (sum[1] / norm) as f32])
    }
    fn read(&mut self, start: i64, count: usize) -> Result<Vec<f32>> {
        let mut b = Vec::with_capacity(count * 2);
        for i in 0..count {
            b.extend(self.sample(start + i as i64)?);
        }
        Ok(b)
    }
}
fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.
    } else {
        (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
    }
}
struct Voice {
    placement: Placement,
    source: Resampled,
    stretch: Option<Stretch>,
    input_cursor: i64,
    generated: i64,
    pending: Vec<f32>,
    pending_offset: usize,
    low: [f64; 2],
    upper: [f64; 2],
    alpha: [f64; 2],
    rate: u32,
}
impl Voice {
    fn new(p: &Placement, path: &Path, rate: u32, crossovers: [f64; 2]) -> Result<Self> {
        let wav = WavReader::open(path)?;
        require(
            wav.format.sample_rate_hz == p.time_map.source_sample_rate_hz
                && p.source_end_frame <= wav.format.frames,
            "SOURCE_FORMAT",
            "source does not match plan timebase",
        )?;
        let source = Resampled {
            wav,
            output_rate: rate,
            start: -1,
            cache: vec![],
        };
        let native = p.time_map.slopes().iter().all(|s| (s - 1.).abs() < 1e-8);
        let stretch = if native {
            None
        } else {
            Some(Stretch::preset_default(2, rate))
        };
        let mut v = Self {
            placement: p.clone(),
            source,
            stretch,
            input_cursor: 0,
            generated: 0,
            pending: vec![],
            pending_offset: 0,
            low: [0.; 2],
            upper: [0.; 2],
            alpha: crossovers.map(|f| 1. - (-2. * std::f64::consts::PI * f / rate as f64).exp()),
            rate,
        };
        v.input_cursor = (p.source_start_frame as f64 / p.time_map.source_sample_rate_hz as f64
            * rate as f64)
            .round() as i64;
        if let Some(stretch) = v.stretch.as_mut() {
            let n = stretch.input_latency();
            let pre = v.source.read(v.input_cursor, n)?;
            stretch.seek(&pre, p.time_map.slopes()[0]);
            v.input_cursor += n as i64;
            v.generated = -(stretch.output_latency() as i64);
        }
        Ok(v)
    }
    fn fill(&mut self) -> Result<()> {
        let p = &self.placement;
        let start = p.time_map.anchors[0].output_frame;
        let end = p.time_map.anchors.last().unwrap().output_frame;
        let mut output = vec![0f32; 512];
        if let Some(s) = self.stretch.as_mut() {
            let center = self.generated + s.output_latency() as i64;
            let target = start as f64 + center as f64 + 256.;
            let source = if target <= end as f64 {
                p.time_map.source_at(target)
            } else {
                p.source_end_frame as f64
                    + (target - end as f64) * p.time_map.source_sample_rate_hz as f64
                        / self.rate as f64
            };
            let input_end = (source / p.time_map.source_sample_rate_hz as f64 * self.rate as f64)
                .round() as i64
                + s.input_latency() as i64;
            let n = (input_end - self.input_cursor).max(0) as usize;
            require(
                n <= 1024,
                "STRETCH_BLOCK",
                "time map requires oversized stretch block",
            )?;
            let input = self.source.read(self.input_cursor, n)?;
            s.process(&input, &mut output);
            self.input_cursor = input_end;
        } else {
            output = self.source.read(self.input_cursor, 256)?;
            self.input_cursor += 256;
        }
        let discard = (-self.generated).clamp(0, 256) as usize;
        self.generated += 256;
        self.pending = output;
        self.pending_offset = discard * 2;
        Ok(())
    }
    fn next(&mut self, frame: u64) -> Result<[f32; 2]> {
        while self.pending_offset >= self.pending.len() {
            self.fill()?;
        }
        let mut sample = [
            self.pending[self.pending_offset],
            self.pending[self.pending_offset + 1],
        ];
        self.pending_offset += 2;
        let (gain, bands) = automation(&self.placement.automation, frame);
        for (c, sample) in sample.iter_mut().enumerate() {
            let x = *sample as f64;
            self.low[c] += self.alpha[0] * (x - self.low[c]);
            self.upper[c] += self.alpha[1] * (x - self.upper[c]);
            let y = self.low[c] * bands[0]
                + (self.upper[c] - self.low[c]) * bands[1]
                + (x - self.upper[c]) * bands[2];
            *sample = (y * gain) as f32;
        }
        Ok(sample)
    }
}
pub fn automation(points: &[AutomationPoint], frame: u64) -> (f64, [f64; 3]) {
    let i = points
        .partition_point(|p| p.output_frame <= frame)
        .saturating_sub(1);
    let a = &points[i];
    let b = &points[(i + 1).min(points.len() - 1)];
    let t = if b.output_frame == a.output_frame {
        0.
    } else {
        (frame - a.output_frame) as f64 / (b.output_frame - a.output_frame) as f64
    };
    let t = t * t * (3. - 2. * t);
    let ga = if a.mute { 0. } else { a.gain_linear };
    let gb = if b.mute { 0. } else { b.gain_linear };
    (
        ga + (gb - ga) * t,
        std::array::from_fn(|c| a.bands_linear[c] + (b.bands_linear[c] - a.bands_linear[c]) * t),
    )
}

/// Oversampled meter: 4x, 32-tap Hann-windowed sinc interpolation, stereo peak hold.
/// Delayed by 16 samples; caller flushes with 32 zero frames.
pub struct PeakMeter {
    history: [[f64; 2]; 32],
    weights: [[f64; 32]; 3],
    pub sample_peak: f64,
    pub true_peak: f64,
}
impl Default for PeakMeter {
    fn default() -> Self {
        Self {
            history: [[0.; 2]; 32],
            weights: std::array::from_fn(|phase| {
                let mut weights: [f64; 32] = std::array::from_fn(|k| {
                    let d = 15. + (phase + 1) as f64 / 4. - k as f64;
                    sinc(d) * (0.5 + 0.5 * (std::f64::consts::PI * d / 16.).cos())
                });
                let norm = weights.iter().sum::<f64>();
                for w in &mut weights {
                    *w /= norm;
                }
                weights
            }),
            sample_peak: 0.,
            true_peak: 0.,
        }
    }
}
impl PeakMeter {
    pub fn push(&mut self, x: [f32; 2]) {
        self.history.rotate_left(1);
        self.history[31] = x.map(|x| x as f64);
        for v in x {
            self.sample_peak = self.sample_peak.max(v.abs() as f64);
        }
        self.true_peak = self.true_peak.max(self.sample_peak);
        for phase in 0..3 {
            let mut sum = [0.; 2];
            for k in 0..32 {
                let w = self.weights[phase][k];
                for (c, s) in sum.iter_mut().enumerate() {
                    *s += w * self.history[k][c];
                }
            }
            for s in sum {
                self.true_peak = self.true_peak.max(s.abs());
            }
        }
    }
}

/// Resolving and verifying pinned catalogue/analysis revisions is the caller's responsibility.
/// Audio hashes are independently verified here before output creation. All outputs are atomic.
pub fn render(
    plan: &MixPlan,
    sources: &BTreeMap<String, PathBuf>,
    output: &Path,
    overwrite: bool,
    range: Option<(u64, u64)>,
    cancel: &Cancellation,
) -> Result<Report> {
    plan.validate()?;
    require(
        plan.renderer_version == RENDERER,
        "RENDERER_VERSION",
        "plan requires another renderer; replan",
    )?;
    let (first, last) = range.unwrap_or((0, plan.total_output_frames));
    bounds(first, last, plan.total_output_frames)?;
    let bytes_per_frame = if plan.output.encoding == "pcm24" {
        6u64
    } else {
        8
    };
    let bytes = (last - first)
        .checked_mul(bytes_per_frame)
        .ok_or_else(|| Error::new("RIFF_SIZE", "output too large"))?;
    require(
        bytes + 36 <= u32::MAX as u64,
        "RIFF_SIZE",
        "output exceeds RIFF capacity; choose a shorter plan (RF64 export not supported)",
    )?;
    require(
        overwrite || !output.exists(),
        "OUTPUT_EXISTS",
        "output exists; pass --overwrite to replace it",
    )?;
    for p in &plan.placements {
        cancel.check()?;
        let path = sources
            .get(&p.track_id)
            .ok_or_else(|| Error::new("SOURCE_MISSING", &p.track_id))?;
        if output.exists() {
            require(
                std::fs::canonicalize(output)? != std::fs::canonicalize(path)?,
                "SOURCE_OVERWRITE",
                "render output cannot replace source audio",
            )?;
        }
        let mut wav = WavReader::open(path)?;
        require(
            wav.hashes(cancel)?.1 == p.audio_hash,
            "SOURCE_CHANGED",
            format!("{} audio changed; replan", p.track_id),
        )?;
    }
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut temp = NamedTempFile::new_in(parent)?;
    write_header(
        temp.as_file_mut(),
        plan.output.sample_rate_hz,
        plan.output.encoding.as_str(),
        bytes as u32,
    )?;
    let mut active: Vec<(usize, Voice)> = vec![];
    let mut next = 0;
    let mut meter = PeakMeter::default();
    let mut rng = plan.seed ^ 0x9e3779b97f4a7c15;
    let mut block = Vec::with_capacity(65536);
    let mut rendered = 0;
    for frame in 0..last {
        if frame % 4096 == 0 {
            cancel.check()?;
        }
        while next < plan.placements.len()
            && plan.placements[next].time_map.anchors[0].output_frame <= frame
        {
            let p = &plan.placements[next];
            active.push((
                next,
                Voice::new(
                    p,
                    &sources[&p.track_id],
                    plan.output.sample_rate_hz,
                    plan.crossover_hz,
                )?,
            ));
            next += 1;
        }
        active.retain(|(i, _)| {
            frame
                < plan.placements[*i]
                    .time_map
                    .anchors
                    .last()
                    .unwrap()
                    .output_frame
        });
        let mut sum = [0f32; 2];
        for (_, v) in &mut active {
            let sample = v.next(frame)?;
            for c in 0..2 {
                sum[c] += sample[c];
            }
        }
        require(
            sum.iter().all(|v| v.is_finite()),
            "DSP_NON_FINITE",
            "non-finite render output",
        )?;
        if frame >= first {
            meter.push(sum);
            for s in sum {
                if plan.output.encoding == "pcm24" {
                    let noise = (random(&mut rng) - random(&mut rng)) / 8388608.;
                    let v = ((s as f64 + noise) * 8388608.)
                        .round()
                        .clamp(-8388608., 8388607.) as i32;
                    block.extend_from_slice(&v.to_le_bytes()[..3]);
                } else {
                    block.extend_from_slice(&s.to_le_bytes());
                }
            }
            rendered += 1;
            if block.len() >= 65536 {
                temp.write_all(&block)?;
                block.clear();
            }
        }
    }
    temp.write_all(&block)?;
    for _ in 0..32 {
        meter.push([0.; 2]);
    }
    require(
        meter.true_peak <= 10f64.powf(-1. / 20.),
        "TRUE_PEAK",
        format!(
            "render measured {:.2} dBTP above -1 dBTP limit; reduce planned gains and replan",
            20. * meter.true_peak.log10()
        ),
    )?;
    // Reverify after decoding: never publish a mix from a source changed mid-render.
    for p in &plan.placements {
        let mut wav = WavReader::open(&sources[&p.track_id])?;
        require(
            wav.hashes(cancel)?.1 == p.audio_hash,
            "SOURCE_CHANGED",
            format!("{} changed during render", p.track_id),
        )?;
    }
    temp.as_file().sync_all()?;
    let check = WavReader::open(temp.path())?;
    require(
        check.format.frames == rendered,
        "OUTPUT_VALIDATE",
        "render frame count mismatch",
    )?;
    cancel.check()?;
    if overwrite {
        temp.persist(output).map_err(|e| Error::from(e.error))?;
    } else {
        temp.persist_noclobber(output)
            .map_err(|e| Error::from(e.error))?;
    }
    File::open(parent)?.sync_all()?;
    let mut report = Report::new("render");
    report.counts.insert("output_frames".into(), rendered);
    report
        .header
        .extensions
        .insert("sample_peak".into(), serde_json::json!(meter.sample_peak));
    report
        .header
        .extensions
        .insert("true_peak".into(), serde_json::json!(meter.true_peak));
    report.header.extensions.insert(
        "true_peak_meter".into(),
        serde_json::json!(
            "4x, 32-tap Hann-windowed sinc; engineering meter, not certified BS.1770"
        ),
    );
    report
        .header
        .extensions
        .insert("output".into(), serde_json::json!(output));
    Ok(report)
}
fn random(s: &mut u64) -> f64 {
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    (*s >> 11) as f64 / ((1u64 << 53) as f64)
}
pub fn write_header(f: &mut File, rate: u32, encoding: &str, bytes: u32) -> Result<()> {
    let (tag, bits) = if encoding == "pcm24" {
        (1u16, 24u16)
    } else {
        (3, 32)
    };
    let align = 2 * (bits / 8);
    f.seek(SeekFrom::Start(0))?;
    f.write_all(b"RIFF")?;
    f.write_all(&(bytes + 36).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&tag.to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * align as u32).to_le_bytes())?;
    f.write_all(&align.to_le_bytes())?;
    f.write_all(&bits.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&bytes.to_le_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complementary_filters_reconstruct() {
        let mut low = 0.;
        let mut upper = 0.;
        for n in 0..1000 {
            let x = (n as f64 * 0.17).sin();
            low += 0.03 * (x - low);
            upper += 0.3 * (x - upper);
            assert!((low + (upper - low) + (x - upper) - x).abs() < 1e-12);
        }
    }
    #[test]
    fn meter_detects_intersample_peak() {
        let mut m = PeakMeter::default();
        for n in 0..1000 {
            let x =
                (std::f64::consts::PI * 0.5 * n as f64 + std::f64::consts::FRAC_PI_4).sin() as f32;
            m.push([x, x]);
        }
        assert!(m.true_peak > m.sample_peak * 1.3);
    }
}
