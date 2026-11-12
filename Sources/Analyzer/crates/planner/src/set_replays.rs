//! Conservative source-backed phrase reuse. No ASR-empty => instrumental shortcut.
use crate::{cached_tactic_evidence::verified_absence, load_tactic_track_analysis};
use mixlib_audio::WavReader;
use mixlib_catalog::Store;
use mixlib_types::*;
use serde_json::{json, Value};

#[derive(Clone, Debug, serde::Serialize)]
pub struct MeasuredReplay {
    pub start: u64,
    pub end: u64,
    pub destination: u64,
    pub bars: u32,
    pub quality: f64,
    pub evidence: Value,
}
pub fn read_set_replays(
    store: &Store,
    track_id: &str,
    cancel: &Cancellation,
) -> Result<Vec<MeasuredReplay>> {
    let snapshot = store.read_snapshot();
    let analyzed = crate::load_analyzed(&snapshot, snapshot.track(track_id)?)?;
    let map = crate::native_map(&analyzed.track, 0, 0, &MixingConfig::default());
    let result = measure_set_replays(&snapshot, &crate::placement(&analyzed, map), cancel)?;
    snapshot.verify_read_snapshot(cancel)?;
    Ok(result)
}
fn number(row: &Value, path: &str) -> Option<f64> {
    row.pointer(path)?.as_f64().filter(|n| n.is_finite())
}
fn frame(row: &Value, key: &str) -> Option<u64> {
    row[key].as_u64()
}
fn distance(a: &Value, b: &Value) -> Option<f64> {
    let a: Vec<_> = a
        .as_array()?
        .iter()
        .map(Value::as_f64)
        .collect::<Option<_>>()?;
    let b: Vec<_> = b
        .as_array()?
        .iter()
        .map(Value::as_f64)
        .collect::<Option<_>>()?;
    if a.len() != 12 || b.len() != 12 || a.iter().chain(&b).any(|v| !v.is_finite()) {
        return None;
    }
    let aa = a.iter().map(|v| v * v).sum::<f64>().sqrt();
    let bb = b.iter().map(|v| v * v).sum::<f64>().sqrt();
    (aa * bb > 1e-9)
        .then(|| (1. - a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>() / (aa * bb)).max(0.))
}
fn compatible(a: &Value, b: &Value) -> Option<f64> {
    let chroma = distance(&a["source"]["chroma"], &b["source"]["chroma"])?;
    let rms = number(a, "/source/rms")? / number(b, "/source/rms")?.max(1e-9);
    let drums = (number(a, "/stems/drums/onset_density_per_second")?
        - number(b, "/stems/drums/onset_density_per_second")?)
    .abs();
    let brightness = number(a, "/source/centroid_hz")? / number(b, "/source/centroid_hz")?.max(1.);
    for band in ["low_fraction", "mid_fraction", "high_fraction"] {
        let path = format!("/source/{band}");
        if (number(a, &path)? - number(b, &path)?).abs() > 0.10 {
            return None;
        }
    }
    for lane in ["drums", "bass", "other"] {
        let path = format!("/stems/{lane}/energy_share_of_source");
        if (number(a, &path)? - number(b, &path)?).abs() > 0.15 {
            return None;
        }
    }
    (chroma <= 0.10
        && (0.8..=1.25).contains(&rms)
        && drums <= 0.5
        && (0.75..=1.33).contains(&brightness))
    .then_some(chroma)
}
fn clear(record: &TacticTrackAnalysis, start: u64, end: u64) -> bool {
    let guard = u64::from(record.source_sample_rate_hz) * 3 / 10;
    let a = start.saturating_sub(guard);
    let b = end.saturating_add(guard).min(record.source_frames);
    let Some(track) = record.vocal_evidence.get("track") else {
        return false;
    };
    verified_absence(track, a, b, record.source_sample_rate_hz)
        && track
            .get("phrases")
            .and_then(Value::as_array)
            .is_some_and(|phrases| {
                phrases.iter().all(|p| {
                    number(p, "/start_seconds")
                        .zip(number(p, "/end_seconds"))
                        .is_some_and(|(x, y)| {
                            y > x
                                && (y * f64::from(record.source_sample_rate_hz) <= a as f64
                                    || x * f64::from(record.source_sample_rate_hz) >= b as f64)
                        })
                })
            })
        && record
            .signal_evidence
            .get("events")
            .and_then(Value::as_array)
            .is_some_and(|events| {
                events.iter().all(|e| {
                    if !matches!(
                        e["kind"].as_str(),
                        Some("build" | "drop" | "beat_return" | "drum_return")
                    ) {
                        return true;
                    }
                    let Some(x) = frame(e, "frame") else {
                        return false;
                    };
                    let y = frame(e, "end_frame").unwrap_or(x.saturating_add(guard));
                    y <= a || x >= b
                })
            })
}
fn seam(wav: &mut WavReader, tail: u64, head: u64, level: f64) -> Result<f64> {
    if tail < 2 {
        return Ok(f64::INFINITY);
    }
    wav.seek_frame(tail - 2)?;
    let a = wav.read_frames(2)?;
    wav.seek_frame(head)?;
    let b = wav.read_frames(2)?;
    if a.len() != 2 || b.len() != 2 {
        return Ok(f64::INFINITY);
    }
    Ok((0..2)
        .map(|c| {
            let before = f64::from(a[1][c] - a[0][c]);
            let after = f64::from(b[1][c] - b[0][c]);
            let continuation = (f64::from(b[0][c] - a[1][c]) - (before + after) * 0.5).abs();
            continuation.max((before - after).abs() * 0.5) / level.max(0.001)
        })
        .fold(0., f64::max))
}

/// Whole 4/8/16-bar loops, optionally moved later within the same song. The
/// caller must still enforce solo ownership, automation and output-clock bounds.
pub fn measure_set_replays(
    store: &Store,
    p: &Placement,
    cancel: &Cancellation,
) -> Result<Vec<MeasuredReplay>> {
    let Some(record) = load_tactic_track_analysis(store, p)? else {
        return Ok(vec![]);
    };
    let Some(bars) = record.signal_evidence["bars"].as_array() else {
        return Ok(vec![]);
    };
    let mut candidates = vec![];
    let mut wav = None;
    for start in (0..bars.len()).step_by(4) {
        for count in [4usize, 8, 16] {
            cancel.check()?;
            let Some(window) = bars.get(start..start + count) else {
                continue;
            };
            let Some((a, b)) =
                frame(&window[0], "start_frame").zip(frame(window.last().unwrap(), "end_frame"))
            else {
                continue;
            };
            if b <= a
                || b - a > u64::from(record.source_sample_rate_hz) * 60
                || !clear(&record, a, b)
            {
                continue;
            }
            let mut lengths = vec![];
            let mut levels = vec![];
            let mut valid = true;
            for (i, bar) in window.iter().enumerate() {
                let Some((x, y)) = frame(bar, "start_frame").zip(frame(bar, "end_frame")) else {
                    valid = false;
                    break;
                };
                if y <= x
                    || (i > 0 && frame(&window[i - 1], "end_frame") != Some(x))
                    || bar["pulse"]["beat_count"].as_u64() != Some(4)
                    || !number(bar, "/pulse/interval_cv").is_some_and(|v| (0.0..=0.04).contains(&v))
                {
                    valid = false;
                    break;
                }
                lengths.push((y - x) as f64);
                levels.push(number(bar, "/source/rms").unwrap_or(0.));
            }
            if !valid {
                continue;
            }
            let period = lengths.iter().sum::<f64>() / count as f64;
            let level = levels.iter().sum::<f64>() / count as f64;
            if level < 0.003
                || lengths.iter().any(|n| (n / period - 1.).abs() > 0.02)
                || levels.iter().any(|n| !(0.7..=1.4).contains(&(n / level)))
            {
                continue;
            }
            // Compare equivalent positions in successive cycles, not the last
            // chord/fill against the opening bar of the same progression.
            let Some(next_cycle) = bars.get(start + count) else {
                continue;
            };
            let Some(tonal) = compatible(&window[0], next_cycle) else {
                continue;
            };
            if wav.is_none() {
                let track = store.track(&p.track_id)?;
                let mut reader = WavReader::open(&store.source(&track)?)?;
                require(
                    reader.hashes(cancel)?.1 == p.audio_hash,
                    "SOURCE_CHANGED",
                    "set loop source changed",
                )?;
                wav = Some(reader);
            }
            let reader = wav.as_mut().unwrap();
            let cycle = seam(reader, b, a, level)?;
            if cycle > 0.20 {
                continue;
            }
            // Original endpoint plus up to two later phrase-aligned destinations.
            for at in [start + count, start + count * 2, start + count * 3] {
                let Some(next) = bars.get(at) else {
                    continue;
                };
                let Some(destination) = frame(next, "start_frame") else {
                    continue;
                };
                let Some(prev) = bars.get(at - 1) else {
                    continue;
                };
                if frame(prev, "end_frame") != Some(destination)
                    || destination < b
                    || !clear(&record, destination, destination + 1)
                    || compatible(next, &window[0]).is_none()
                    || compatible(prev, window.last().unwrap()).is_none()
                    || !frame(next, "end_frame").is_some_and(|end| {
                        end > destination
                            && ((end - destination) as f64 / period - 1.).abs() <= 0.02
                    })
                {
                    continue;
                }
                let entrance = seam(reader, destination, a, level)?;
                let exit = seam(reader, b, destination, level)?;
                let worst = cycle.max(entrance).max(exit);
                if worst > 0.20 {
                    continue;
                }
                candidates.push(MeasuredReplay {
                    start: a,
                    end: b,
                    destination,
                    bars: count as u32,
                    quality: 1. - worst / 0.4 - tonal,
                    evidence: json!({"method":"measured-set-replay/1","analysis":record.cache_key,
                        "source_frames":[a,b],"destination_source_frame":destination,
                        "vocal_absence":"verified with 300 ms guards; recognized phrases veto",
                        "continuation_error":worst,"tonal_distance":tonal,
                        "maximum_continuation_error":0.20,"declick_milliseconds":5,
                        "natural_cycle_recurrence_source_frame":b,
                        "steady_bars":count,"source_level":level,
                        "relocated":destination != b,"development_credit":0}),
                });
            }
        }
    }
    candidates.sort_by(|a, b| b.quality.total_cmp(&a.quality).then(a.start.cmp(&b.start)));
    Ok(candidates)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_vocals_continuing_lines_and_builds_veto_reuse() {
        let hash = hash_bytes(b"fixture");
        let reference = FileRef {
            path: "fixture.json".into(),
            hash: hash.clone(),
        };
        let mut record = TacticTrackAnalysis {
            header: Header::default(),
            tactic_analysis_id: "fixture".into(),
            track_id: "fixture".into(),
            audio_hash: hash.clone(),
            audio_revision: 1,
            source_sample_rate_hz: 100,
            source_frames: 10000,
            base_analysis: reference.clone(),
            beat_artifact: reference,
            override_revision: 0,
            method: TACTIC_ANALYSIS_METHOD.into(),
            implementation_hash: hash.clone(),
            cache_key: hash,
            component_hashes: Default::default(),
            models: Default::default(),
            stems: Default::default(),
            vocal_evidence: json!({"track":{"instrumental":true,"phrases":[]}}),
            signal_evidence: json!({"events":[]}),
            limitations: vec![],
            created_at: "fixture".into(),
        };
        assert!(!clear(&record, 1000, 2000));
        record.vocal_evidence["track"]["verified_absence_spans"] = json!([{
            "start_seconds":0.,"end_seconds":100.,"reference":"fixture",
            "verification_method":"separated_vocal_rms_and_peak_negligible/1"}]);
        assert!(clear(&record, 1000, 2000));
        record.vocal_evidence["track"]["phrases"] = json!([{"start_seconds":5.,"end_seconds":25.}]);
        assert!(!clear(&record, 1000, 2000));
        record.vocal_evidence["track"]["phrases"] = json!([]);
        record.signal_evidence["events"] = json!([{"kind":"build","frame":1500,"end_frame":2500}]);
        assert!(!clear(&record, 1000, 2000));
        assert!(clear(&record, 3000, 4000));
    }
    #[test]
    fn pcm_continuation_rejects_a_discontinuous_splice() {
        use std::io::Write;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("seams.wav");
        let mut f = std::fs::File::create(&path).unwrap();
        mixlib_audio::write_header(&mut f, 48000, "float32", 66 * 8).unwrap();
        for i in 0..66 {
            let s = if i < 64 {
                (0.1 * (std::f64::consts::TAU * i as f64 / 64.).sin()) as f32
            } else {
                0.9
            };
            for _ in 0..2 {
                f.write_all(&s.to_le_bytes()).unwrap();
            }
        }
        drop(f);
        let mut wav = WavReader::open(&path).unwrap();
        assert!(seam(&mut wav, 64, 0, 0.1).unwrap() < 0.001);
        assert!(seam(&mut wav, 64, 64, 0.1).unwrap() > 1.);
    }
    #[test]
    fn different_harmony_and_rhythm_are_not_matching_loop_destinations() {
        let a = json!({"source":{"chroma":[1.,0.,0.,0.,0.,0.,0.,0.,0.,0.,0.,0.],"rms":0.1,
            "centroid_hz":1000.,"low_fraction":0.2,"mid_fraction":0.6,"high_fraction":0.2},
            "stems":{"drums":{"onset_density_per_second":2.,"energy_share_of_source":0.3},
                "bass":{"energy_share_of_source":0.2},"other":{"energy_share_of_source":0.5}}});
        assert!(compatible(&a, &a).is_some());
        let mut b = a.clone();
        b["source"]["chroma"] = json!([0., 1., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0.]);
        assert!(compatible(&a, &b).is_none());
        b = a.clone();
        b["stems"]["drums"]["onset_density_per_second"] = json!(4.);
        assert!(compatible(&a, &b).is_none());
        b = a.clone();
        b["source"]["high_fraction"] = json!(0.6);
        assert!(
            compatible(&a, &b).is_none(),
            "same key and loudness cannot hide a different band balance"
        );
    }
}
