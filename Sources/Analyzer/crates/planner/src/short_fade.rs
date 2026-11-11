//! Positive evidence for promoting an otherwise last-resort stereo fade.
use crate::tactics::ShortFadeMatch;
use mixlib_audio::WavReader;
use mixlib_catalog::Store;
use mixlib_types::*;
use std::path::Path;

struct Excerpt<'a> {
    map: &'a TimeMap,
    offset: u64,
    gain: f64,
    pcm: Vec<[f32; 2]>,
}

impl<'a> Excerpt<'a> {
    fn read(
        path: &Path,
        p: &'a Placement,
        [start, end]: [u64; 2],
        cancel: &Cancellation,
    ) -> Result<Self> {
        let map = &p.time_map;
        let mut reader = WavReader::open(path)?;
        let offset = map.source_at(start as f64).floor() as u64;
        let limit = (map.source_at(end as f64).ceil() as u64 + 1).min(reader.format.frames);
        require(
            reader.format.sample_rate_hz == map.source_sample_rate_hz && offset < limit,
            "TACTIC_PCM",
            "short-fade source does not cover the mapped window",
        )?;
        reader.seek_frame(offset)?;
        let count = (limit - offset) as usize;
        let mut pcm = Vec::with_capacity(count);
        while pcm.len() < count {
            cancel.check()?;
            let block = (count - pcm.len()).min(65_536);
            let frames = reader.read_frames(block)?;
            require(
                frames.len() == block,
                "TACTIC_PCM",
                "short-fade source ended early",
            )?;
            pcm.extend(frames);
        }
        let gain = p
            .automation
            .iter()
            .filter(|a| !a.mute)
            .map(|a| a.gain_linear)
            .fold(0., f64::max);
        Ok(Self {
            map,
            offset,
            gain,
            pcm,
        })
    }

    fn at(&self, output: u64) -> [f64; 2] {
        let source = (self.map.source_at(output as f64) - self.offset as f64).max(0.);
        let index = (source as usize).min(self.pcm.len() - 1);
        let next = (index + 1).min(self.pcm.len() - 1);
        let frac = source.fract();
        std::array::from_fn(|ch| {
            self.gain
                * ((1. - frac) * f64::from(self.pcm[index][ch])
                    + frac * f64::from(self.pcm[next][ch]))
        })
    }
}

#[derive(Clone, Copy, Default)]
struct Comparison {
    max_energy: f64,
    error: f64,
}

impl Comparison {
    fn add(&mut self, a: f64, b: f64) {
        self.max_energy += a.abs().max(b.abs()).powi(2);
        self.error += (a - b).powi(2);
    }

    fn near(self, samples: f64) -> bool {
        self.error <= (0.20f64.powi(2) * self.max_energy).max(samples * 0.0005f64.powi(2))
    }
}

pub(crate) fn measure(
    store: &Store,
    paths: [&Path; 2],
    placements: [&Placement; 2],
    records: [&TacticTrackAnalysis; 2],
    bounds @ [start, end]: [u64; 2],
    crossovers: [f64; 2],
    cancel: &Cancellation,
) -> Result<Option<ShortFadeMatch>> {
    let rate = placements[0].time_map.output_sample_rate_hz;
    if end - start > u64::from(rate) * 8 {
        return Ok(None);
    }
    let excerpts = [
        Excerpt::read(paths[0], placements[0], bounds, cancel)?,
        Excerpt::read(paths[1], placements[1], bounds, cancel)?,
    ];
    let alpha = crossovers.map(|hz| 1. - (-std::f64::consts::TAU * hz / f64::from(rate)).exp());
    let mut filters = [[[0.; 2]; 2]; 2];
    let mut matches = [true; 3];
    let mut quiet = true;
    let block = u64::from(rate / 2);
    for first in (start..end).step_by(block as usize) {
        cancel.check()?;
        let last = (first + block).min(end);
        let samples = (2 * (last - first)) as f64;
        let mut bands = [Comparison::default(); 3];
        let mut dry = Comparison::default();
        let mut peak: f64 = 0.;
        for frame in first..last {
            let stereo = excerpts.each_ref().map(|e| e.at(frame));
            for ch in 0..2 {
                let [a, b] = [stereo[0][ch], stereo[1][ch]];
                dry.add(a, b);
                peak = peak.max(a.abs()).max(b.abs());
                let split = [0, 1].map(|side| {
                    let x = stereo[side][ch];
                    let [low, upper] = &mut filters[side][ch];
                    *low += alpha[0] * (x - *low);
                    *upper += alpha[1] * (x - *upper);
                    [*low, *upper - *low, x - *upper]
                });
                for (i, band) in bands.iter_mut().enumerate() {
                    band.add(split[0][i], split[1][i]);
                }
            }
        }
        quiet &= dry.near(samples) && dry.max_energy <= samples * 0.006f64.powi(2) && peak <= 0.02;
        for (matched, band) in matches.iter_mut().zip(bands) {
            *matched &= band.near(samples);
        }
    }
    if quiet && matches.iter().all(|&matched| matched) {
        return Ok(Some(ShortFadeMatch::QuietNearMatch));
    }
    if matches.iter().all(|&matched| matched) {
        return Ok(Some(ShortFadeMatch::AllBandsMatch));
    }
    if matches.iter().filter(|&&matched| matched).count() != 2 {
        return Ok(None);
    }
    let (Some(a), Some(b)) = (
        records[0].stems.get(&StemLane::Drums),
        records[1].stems.get(&StemLane::Drums),
    ) else {
        return Ok(None);
    };
    let drums = [
        Excerpt::read(&store.path(&a.path)?, placements[0], bounds, cancel)?,
        Excerpt::read(&store.path(&b.path)?, placements[1], bounds, cancel)?,
    ];
    let mut energy = 0.;
    for first in (start..end).step_by(block as usize) {
        cancel.check()?;
        let last = (first + block).min(end);
        let mut drum = Comparison::default();
        for frame in first..last {
            let [a, b] = drums.each_ref().map(|d| d.at(frame));
            for ch in 0..2 {
                drum.add(a[ch], b[ch]);
            }
        }
        if !drum.near((2 * (last - first)) as f64) {
            return Ok(None);
        }
        energy += drum.max_energy;
    }
    Ok((energy >= (2 * (end - start)) as f64 * 0.003f64.powi(2))
        .then_some(ShortFadeMatch::OneBandDifference))
}
