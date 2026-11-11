//! Bind supplementary full-track evidence to a saved source generation and cue.
use crate::tactic_evidence::output_at;
use crate::tactics::*;
use mixlib_audio::WavReader;
use mixlib_catalog::Store;
use mixlib_types::*;
use serde_json::Value;

/// A newer supplementary generation is usable only when it analyzes the same
/// audio, beat artifact and correction revision as the saved placement.
pub fn load_tactic_track_analysis(
    store: &Store,
    p: &Placement,
) -> Result<Option<TacticTrackAnalysis>> {
    let track = store.track(&p.track_id)?;
    require(
        track.audio_hash == p.audio_hash
            && track.audio_revision == p.audio_revision
            && track.override_revision == p.override_revision,
        "TACTIC_ANALYSIS_STALE",
        "supplementary tactic analysis cannot replace a changed source or correction revision",
    )?;
    let pinned: AnalysisManifest = store.read_ref(&p.analysis)?;
    let Some(beats) = pinned.artifacts.get("beats") else {
        return Ok(None);
    };
    let beat_hash = beats.hash.clone();
    let mut candidates = vec![pinned];
    if let Some(active) = track
        .active_analysis
        .as_ref()
        .filter(|r| r.hash != p.analysis.hash)
    {
        candidates.push(store.read_ref::<AnalysisManifest>(active)?);
    }
    for manifest in candidates {
        if manifest.audio_hash != p.audio_hash {
            continue;
        }
        let Some(reference) = manifest.artifacts.get("tactic_analysis") else {
            continue;
        };
        let record: TacticTrackAnalysis = store.read_ref(reference)?;
        if record.audio_hash == p.audio_hash
            && record.audio_revision == p.audio_revision
            && record.track_id == p.track_id
            && record.beat_artifact.hash == beat_hash
            && record.override_revision == p.override_revision
            && record.source_sample_rate_hz == p.time_map.source_sample_rate_hz
        {
            return Ok(Some(record));
        }
    }
    Ok(None)
}

pub(crate) fn apply_cached_tactic_facts(
    store: &Store,
    record: &TacticTrackAnalysis,
    p: &Placement,
    anchors: &[u64],
    facts: &mut PassageFacts,
    cancel: &Cancellation,
) -> Result<()> {
    facts.evidence.push(format!(
        "supplementary_tactic_analysis:{} cache:{}; pinned audio/beat/correction identity verified",
        record.tactic_analysis_id, record.cache_key.digest
    ));
    facts.evidence.extend(record.limitations.iter().cloned());
    facts.vocal_quiet = mapped_quiet_spans(store, record, p, anchors, cancel)?;
    facts.vocal_phrases =
        cached_vocal_phrases(store, record, p, anchors, &mut facts.evidence, cancel)?;
    let rows = window_rows(record, p, anchors);
    if let Some(rows) = &rows {
        facts.stems = stem_quality(rows, facts.vocal_phrases.is_some(), &mut facts.evidence);
        facts.stem_leakage = mapped_leakage(rows, p, anchors);
        if facts.stems.is_some() {
            facts.stem_assets = record
                .stems
                .iter()
                .map(|(lane, audio)| StemAsset {
                    placement: facts.placement,
                    lane: *lane,
                    audio: audio.clone(),
                    source_offset_frame: 0,
                    source_end_frame: record.source_frames,
                })
                .collect();
        }
        let mut roles = std::collections::BTreeMap::new();
        for row in rows {
            for role in row
                .get("roles")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let key = match role.as_str() {
                    Some("rest") => Some(TacticRole::Rest),
                    Some("low_percussion_bed") => Some(TacticRole::LowPercussionBed),
                    Some("sustained_texture") => Some(TacticRole::SustainedTexture),
                    Some("drum_groove") => Some(TacticRole::SteadyGroove),
                    Some("mixed") => Some(TacticRole::Mixed),
                    _ => None,
                };
                if let Some(key) = key {
                    *roles.entry(key).or_insert(0.) += 1. / rows.len() as f64;
                }
            }
        }
        facts.roles.extend(roles);
    }
    for bar in &mut facts.bars {
        if let Some(row) = matching_bar(record, p, bar.start_frame, bar.end_frame) {
            let stems = row.get("stems").and_then(Value::as_object);
            bar.active_stems = stems.and_then(|s| {
                let activity = s
                    .values()
                    .map(|v| num(v, "activity_fraction"))
                    .collect::<Option<Vec<_>>>()?;
                (activity.len() == 4).then(|| activity.iter().filter(|v| **v >= 0.6).count() as u8)
            });
        }
    }
    apply_events(record, p, anchors, facts);
    let end_source = p
        .time_map
        .source_at(*anchors.last().unwrap() as f64)
        .round() as u64;
    facts.endpoint_reasons = crate::transition_endpoints::measured(record)
        .into_iter()
        .filter(|e| {
            e.frame.abs_diff(end_source) <= u64::from(record.source_sample_rate_hz) * 8 / 100
        })
        .map(|e| e.reason)
        .collect();
    facts.evidence.extend(
        facts
            .endpoint_reasons
            .iter()
            .map(|r| format!("measured transition endpoint: {r}")),
    );
    apply_motif(record, p, anchors, facts);
    apply_loops(record, p, anchors, facts);
    Ok(())
}

fn num(value: &Value, key: &str) -> Option<f64> {
    value.get(key)?.as_f64().filter(|v| v.is_finite())
}

pub(crate) fn matching_bar<'a>(
    record: &'a TacticTrackAnalysis,
    p: &Placement,
    start: u64,
    end: u64,
) -> Option<&'a Value> {
    let tolerance = f64::from(p.time_map.output_sample_rate_hz) * 0.030;
    record
        .signal_evidence
        .get("bars")?
        .as_array()?
        .iter()
        .find(|bar| {
            let bounds = bar
                .get("start_frame")
                .and_then(Value::as_u64)
                .zip(bar.get("end_frame").and_then(Value::as_u64));
            bounds
                .and_then(|(a, b)| {
                    output_at(&p.time_map, a as f64).zip(output_at(&p.time_map, b as f64))
                })
                .is_some_and(|(a, b)| {
                    (a - start as f64).abs() <= tolerance && (b - end as f64).abs() <= tolerance
                })
        })
}

fn window_rows<'a>(
    record: &'a TacticTrackAnalysis,
    p: &Placement,
    anchors: &[u64],
) -> Option<Vec<&'a Value>> {
    let start = p.time_map.source_at(anchors[0] as f64).round() as u64;
    let end = p.time_map.source_at(*anchors.last()? as f64).round() as u64;
    covering_rows(&record.signal_evidence, start, end)
}

fn covering_rows(signal: &Value, start: u64, end: u64) -> Option<Vec<&Value>> {
    let mut rows: Vec<_> = ["bars", "unmetered_windows"]
        .into_iter()
        .flat_map(|key| {
            signal
                .get(key)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter(|row| {
            row.get("start_frame")
                .and_then(Value::as_u64)
                .zip(row.get("end_frame").and_then(Value::as_u64))
                .is_some_and(|(a, b)| a < end && b > start && a < b)
        })
        .collect();
    rows.sort_by_key(|row| row.get("start_frame").and_then(Value::as_u64));
    if rows.first()?.get("start_frame")?.as_u64()? > start
        || rows.last()?.get("end_frame")?.as_u64()? < end
        || rows
            .windows(2)
            .any(|w| w[0].get("end_frame") != w[1].get("start_frame"))
    {
        return None;
    }
    Some(rows)
}

fn stem_quality(
    rows: &[&Value],
    known_vocals: bool,
    evidence: &mut Vec<String>,
) -> Option<StemEvidence> {
    let (mut reconstruction, mut artifact, mut projection) = (0f64, 0f64, 0f64);
    for row in rows {
        reconstruction = reconstruction.max(num(row.get("reconstruction")?, "relative_rms_error")?);
        for name in ["drums", "bass", "other", "vocals"] {
            let defect = row.get("stems")?.get(name)?.get("artifact")?;
            if defect.get("method")?.as_str()? != "pcm_discontinuity_added_clipping/1" {
                return None;
            }
            artifact = artifact.max(num(defect, "score")?);
            if name != "vocals" {
                let leakage = row.get("vocal_leakage")?.get(name)?;
                match leakage.get("status")?.as_str()? {
                    "suspected" => projection = projection.max(1.),
                    "not_detected" => {
                        // A regression coefficient against a noisy separated
                        // reference is not a certified bleed amplitude. Keep
                        // the legacy amplitude bound adverse; the mapped,
                        // versioned veto record owns schedule-aware admission.
                        num(leakage, "max_projection_ratio")?;
                        projection = projection.max(1.);
                    }
                    "unassessed"
                        if known_vocals
                            && row.get("vocal")?.get("acoustic_active")?.as_bool()
                                == Some(false) => {}
                    // Preserve unknown isolation as an adverse legacy bound.
                    // Mapped leakage windows allow the schedule to determine
                    // whether that uncertainty is actually audible.
                    "unassessed" => projection = projection.max(1.),
                    _ => return None,
                }
            }
        }
    }
    evidence.push("stem quality uses measured discontinuity/clipping, reconstruction and lagged leakage vetoes; low scores do not certify perceptual separation quality".into());
    Some(StemEvidence {
        available: [
            StemLane::Drums,
            StemLane::Bass,
            StemLane::Other,
            StemLane::Vocals,
        ]
        .into_iter()
        .collect(),
        max_vocal_leakage_db: 20. * projection.max(1e-6).log10(),
        reconstruction_error: reconstruction,
        artifact_score: artifact,
    })
}

fn mapped_leakage(
    rows: &[&Value],
    p: &Placement,
    anchors: &[u64],
) -> Option<Vec<StemLeakageWindow>> {
    let start = *anchors.first()?;
    let end = *anchors.last()?;
    let mapped = |source: u64| output_at(&p.time_map, source as f64).map(|v| v.round() as u64);
    let mut result = vec![];
    for row in rows {
        let a = row
            .get("start_frame")?
            .as_u64()?
            .max(p.time_map.source_at(start as f64).floor() as u64);
        let b = row
            .get("end_frame")?
            .as_u64()?
            .min(p.time_map.source_at(end as f64).ceil() as u64);
        let lo = mapped(a)?.max(start);
        let hi = mapped(b)?.min(end);
        if lo >= hi {
            continue;
        }
        for (name, lane) in [
            ("drums", StemLane::Drums),
            ("bass", StemLane::Bass),
            ("other", StemLane::Other),
        ] {
            let leakage = row.get("vocal_leakage")?.get(name)?;
            if leakage.get("method")?.as_str()? != "lagged_vocal_projection_and_coherence/1" {
                return None;
            }
            let spans = match leakage.get("status")?.as_str()? {
                "not_detected" if leakage.get("measured_window_count")?.as_u64()? > 0 => {
                    Some(vec![])
                }
                "suspected" => {
                    let mut spans = vec![];
                    let source_spans = leakage.get("suspect_spans")?.as_array()?;
                    if source_spans.is_empty() {
                        return None;
                    }
                    for span in source_spans {
                        let a = span.get("start_frame")?.as_u64()?.max(a);
                        let b = span.get("end_frame")?.as_u64()?.min(b);
                        if a < b {
                            spans.push([mapped(a)?.max(lo), mapped(b)?.min(hi)]);
                        }
                    }
                    Some(spans)
                }
                "unassessed" => None,
                _ => return None,
            };
            result.push(StemLeakageWindow {
                lane,
                start_frame: lo,
                end_frame: hi,
                suspect_spans: spans,
            });
        }
    }
    Some(result)
}

fn cached_vocal_phrases(
    store: &Store,
    record: &TacticTrackAnalysis,
    p: &Placement,
    anchors: &[u64],
    evidence: &mut Vec<String>,
    cancel: &Cancellation,
) -> Result<Option<Vec<VocalPhrase>>> {
    let Some(provider) = record.vocal_evidence.as_object() else {
        return Ok(None);
    };
    let Some(method) = provider.get("method").and_then(Value::as_str) else {
        return Ok(None);
    };
    let Some(reference) = provider.get("reference").and_then(Value::as_str) else {
        return Ok(None);
    };
    let Some(track) = provider.get("track") else {
        return Ok(None);
    };
    if method.is_empty() || reference.is_empty() {
        return Ok(None);
    }
    let source_start = p.time_map.source_at(anchors[0] as f64).floor() as u64;
    let source_end = p.time_map.source_at(*anchors.last().unwrap() as f64).ceil() as u64;
    if track
        .get("phrases")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty)
        && track.get("instrumental").and_then(Value::as_bool) != Some(true)
        && !verified_absence(
            track,
            source_start,
            source_end,
            record.source_sample_rate_hz,
        )
    {
        return Ok(None);
    }
    let Some(asset) = record.stems.get(&StemLane::Vocals) else {
        return Ok(None);
    };
    let mut wav = WavReader::open(&store.path(&asset.path)?)?;
    require(
        wav.format.sample_rate_hz == record.source_sample_rate_hz
            && wav.format.frames == record.source_frames
            && wav.format.channels == 2,
        "TACTIC_STEM_FORMAT",
        "cached vocal stem source timebase changed",
    )?;
    wav.seek_frame(source_start)?;
    let block = (record.source_sample_rate_hz / 50).max(1) as usize;
    let mut active = Vec::<(u64, u64)>::new();
    let mut negligible = true;
    let mut cursor = source_start;
    while cursor < source_end {
        cancel.check()?;
        let requested = (source_end - cursor).min(block as u64) as usize;
        let samples = wav.read_frames(requested)?;
        require(
            samples.len() == requested,
            "TACTIC_STEM_FORMAT",
            "cached vocal stem ended during analysis",
        )?;
        let rms = (samples
            .iter()
            .flat_map(|s| s.iter())
            .map(|x| f64::from(*x).powi(2))
            .sum::<f64>()
            / (samples.len() * 2) as f64)
            .sqrt();
        let peak = samples
            .iter()
            .flat_map(|s| s.iter())
            .map(|x| f64::from(x.abs()))
            .fold(0., f64::max);
        let quiet = rms < 0.0005 && peak < 0.003;
        negligible &= quiet;
        if !quiet {
            if let Some(last) = active.last_mut().filter(|s| s.1 == cursor) {
                last.1 = cursor + requested as u64;
            } else {
                active.push((cursor, cursor + requested as u64));
            }
        }
        cursor += requested as u64;
    }
    let source_phrases = track.get("phrases").and_then(Value::as_array);
    let provider_absent = track.get("instrumental").and_then(Value::as_bool) == Some(true);
    let context_supported = num(track, "acoustic_word_coverage").is_some_and(|c| c >= 0.85)
        && source_phrases.is_some_and(|p| !p.is_empty());
    let local_absent = verified_absence(
        track,
        source_start,
        source_end,
        record.source_sample_rate_hz,
    );
    if negligible && (provider_absent || context_supported || local_absent) {
        evidence.push(format!("measured quiet vocal PCM in selected window with affirmative provider/context evidence {method} {reference}; estimated absence, not a human certificate"));
        return Ok(Some(vec![]));
    }
    let Some(phrases) = source_phrases else {
        return Ok(None);
    };
    let rate = f64::from(record.source_sample_rate_hz);
    let mut candidates = vec![];
    for phrase in phrases {
        let Some((start, end)) = num(phrase, "start_seconds").zip(num(phrase, "end_seconds"))
        else {
            continue;
        };
        let Some(score) = num(phrase, "confidence_margin") else {
            continue;
        };
        if start < 0. || end <= start || score < 0.20 {
            continue;
        }
        let (a, b) = ((start * rate).round() as u64, (end * rate).round() as u64);
        if a >= source_end || b + record.source_sample_rate_hz as u64 / 2 <= source_start {
            continue;
        }
        if phrase.get("entry_supported").and_then(Value::as_bool) != Some(true)
            || phrase.get("exit_supported").and_then(Value::as_bool) != Some(true)
        {
            continue;
        }
        candidates.push((a, b, b, score));
    }
    for (start, end) in active {
        let tolerance = u64::from(record.source_sample_rate_hz) * 15 / 100;
        let tail = u64::from(record.source_sample_rate_hz) / 2;
        let Some(phrase) = candidates
            .iter_mut()
            .find(|(a, b, _, _)| start + tolerance >= *a && end <= b + tail)
        else {
            evidence.push(
                "unrecognized acoustic vocal activity prevents complete-phrase/absence claims"
                    .into(),
            );
            return Ok(None);
        };
        phrase.2 = phrase.2.max(end);
    }
    let mut phrases = vec![];
    for (start, end, tail, score) in candidates {
        let clipped_start = start.max(p.source_start_frame);
        let clipped_end = end.min(p.source_end_frame);
        if clipped_start >= clipped_end {
            continue;
        }
        let (Some(a), Some(b), Some(t)) = (
            output_at(&p.time_map, clipped_start as f64),
            output_at(&p.time_map, clipped_end as f64),
            output_at(&p.time_map, tail.min(p.source_end_frame) as f64),
        ) else {
            continue;
        };
        phrases.push(VocalPhrase {
            start_frame: a.round() as u64,
            end_frame: b.round() as u64,
            tail_end_frame: t.round() as u64,
            entry_supported: start >= p.source_start_frame,
            exit_supported: end <= p.source_end_frame && tail <= p.source_end_frame,
            confidence_margin: score,
        });
    }
    phrases.sort_by_key(|p| p.start_frame);
    if phrases.is_empty()
        || phrases
            .windows(2)
            .any(|w| w[0].end_frame > w[1].start_frame)
    {
        return Ok(None);
    }
    evidence.push(format!("full-track provider phrases {method} {reference} agree with actual selected vocal PCM; timing mapped through saved cubic time map"));
    Ok(Some(phrases))
}

/// Affirmative provider spans must cover the entire selected source interval.
/// Gaps between spans remain unknown, even if ASR returned no words there.
pub(crate) fn source_quiet_spans(track: &Value, rate: u32) -> Vec<(u64, u64)> {
    let Some(spans) = track
        .get("verified_absence_spans")
        .and_then(Value::as_array)
    else {
        return vec![];
    };
    let mut intervals: Vec<_> = spans
        .iter()
        .filter_map(|span| {
            let method = span.get("verification_method")?.as_str()?;
            let reference = span.get("reference")?.as_str()?;
            if method != "separated_vocal_rms_and_peak_negligible/1" || reference.is_empty() {
                return None;
            }
            let (a, b) = (num(span, "start_seconds")?, num(span, "end_seconds")?);
            (a >= 0. && b > a).then_some((
                (a * f64::from(rate)).round() as u64,
                (b * f64::from(rate)).round() as u64,
            ))
        })
        .collect();
    intervals.sort_unstable();
    intervals
}

fn verified_absence(track: &Value, start: u64, end: u64, rate: u32) -> bool {
    let mut cursor = start;
    for (a, b) in source_quiet_spans(track, rate) {
        if a > cursor {
            return false;
        }
        if b > cursor {
            cursor = b;
        }
        if cursor >= end {
            return true;
        }
    }
    false
}

fn mapped_quiet_spans(
    store: &Store,
    record: &TacticTrackAnalysis,
    p: &Placement,
    anchors: &[u64],
    cancel: &Cancellation,
) -> Result<Option<Vec<[u64; 2]>>> {
    let Some(track) = record.vocal_evidence.get("track") else {
        return Ok(None);
    };
    let Some(asset) = record.stems.get(&StemLane::Vocals) else {
        return Ok(None);
    };
    let rate = record.source_sample_rate_hz;
    let map = &p.time_map;
    let first = map.anchors.first().unwrap();
    let last = map.anchors.last().unwrap();
    let slopes = map.slopes();
    let rate_ratio = f64::from(map.output_sample_rate_hz) / f64::from(rate);
    let map_gap = |source: u64| {
        output_at(map, source as f64)
            .unwrap_or_else(|| {
                let (anchor, speed) = if source < first.source_frame {
                    (first, slopes[0])
                } else {
                    (last, *slopes.last().unwrap())
                };
                anchor.output_frame as f64
                    + (source as f64 - anchor.source_frame as f64) * rate_ratio / speed
            })
            .max(0.)
    };
    let pad = u64::from(rate) / 2;
    let start = (map.source_at(anchors[0] as f64).floor() as u64).saturating_sub(pad);
    let end = ((map.source_at(*anchors.last().unwrap() as f64).ceil() as u64) + pad)
        .min(record.source_frames);
    let mut result: Vec<[u64; 2]> = vec![];
    let mut wav = WavReader::open(&store.path(&asset.path)?)?;
    require(
        wav.format.sample_rate_hz == rate
            && wav.format.frames == record.source_frames
            && wav.format.channels == 2,
        "TACTIC_STEM_FORMAT",
        "vocal gap source timebase changed",
    )?;
    let block = u64::from((rate / 50).max(1));
    for (a, b) in source_quiet_spans(track, rate) {
        let (a, b) = (a.max(start), b.min(end));
        if a >= b {
            continue;
        }
        wav.seek_frame(a)?;
        let mut at = a;
        let mut verified = true;
        while at < b {
            cancel.check()?;
            let count = block.min(b - at) as usize;
            let frames = wav.read_frames(count)?;
            require(
                frames.len() == count,
                "TACTIC_STEM_FORMAT",
                "vocal gap source ended early",
            )?;
            let energy: f64 = frames.iter().flatten().map(|x| f64::from(*x).powi(2)).sum();
            if energy >= 2. * count as f64 * 0.0005f64.powi(2)
                || frames.iter().flatten().any(|x| x.abs() >= 0.003)
            {
                verified = false;
                break;
            }
            at += count as u64;
        }
        if verified {
            let [lo, hi] = [map_gap(a).ceil() as u64, map_gap(b).floor() as u64];
            if lo < hi {
                if let Some(last) = result.last_mut().filter(|last| last[1] >= lo) {
                    last[1] = last[1].max(hi);
                } else {
                    result.push([lo, hi]);
                }
            }
        }
    }
    Ok(Some(result))
}

fn apply_loops(
    record: &TacticTrackAnalysis,
    p: &Placement,
    anchors: &[u64],
    facts: &mut PassageFacts,
) {
    let Some(quality) = facts.stems.as_ref() else {
        return;
    };
    let Some(phrases) = facts.vocal_phrases.as_ref() else {
        return;
    };
    let Some(loops) = record
        .signal_evidence
        .get("loops")
        .and_then(Value::as_array)
    else {
        return;
    };
    for capture in loops {
        let Some((start, end)) = capture
            .get("start_frame")
            .and_then(Value::as_u64)
            .zip(capture.get("end_frame").and_then(Value::as_u64))
        else {
            continue;
        };
        if start < p.source_start_frame || end > p.source_end_frame || start >= end {
            continue;
        }
        let Some((a, b)) =
            output_at(&p.time_map, start as f64).zip(output_at(&p.time_map, end as f64))
        else {
            continue;
        };
        let (a, b) = (a.round() as u64, b.round() as u64);
        if a < anchors[0] || b > *anchors.last().unwrap() || a >= b {
            continue;
        }
        let Some(lanes) = capture.get("lanes").and_then(Value::as_object) else {
            continue;
        };
        for (name, lane) in [
            ("drums", StemLane::Drums),
            ("bass", StemLane::Bass),
            ("other", StemLane::Other),
            ("vocals", StemLane::Vocals),
        ] {
            let Some(measured) = lanes.get(name) else {
                continue;
            };
            if measured.get("signal_safe").and_then(Value::as_bool) != Some(true) {
                continue;
            }
            let Some((seam, tonal)) = num(measured, "continuation_error_ratio")
                .zip(num(measured, "tonal_cycle_distance"))
            else {
                continue;
            };
            let Some(score) = num(measured, "score") else {
                continue;
            };
            if seam < 0. || !(0.0..=1.).contains(&tonal) || !(0.0..=1.).contains(&score) {
                continue;
            }
            let overlapping: Vec<_> = phrases
                .iter()
                .filter(|v| v.start_frame < b && v.tail_end_frame > a)
                .collect();
            let vocal_free = if lane == StemLane::Vocals {
                overlapping.is_empty()
            } else {
                quality.max_vocal_leakage_db <= -26.
            };
            let complete = if lane == StemLane::Vocals {
                Some(
                    !overlapping.is_empty()
                        && overlapping.iter().all(|v| {
                            v.start_frame >= a
                                && v.tail_end_frame <= b
                                && v.entry_supported
                                && v.exit_supported
                        }),
                )
            } else {
                None
            };
            facts.loop_candidates.push(LoopEvidence {
                lane,
                source_start_frame: start,
                source_end_frame: end,
                output_start_frame: a,
                output_end_frame: b,
                seam_error: seam,
                tonal_cycle_distance: tonal,
                vocal_free,
                contains_complete_language: complete,
                confidence_margin: score - 0.5,
            });
        }
    }
    if !facts.loop_candidates.is_empty() {
        facts.evidence.push("cached source PCM loop continuation and tonal cycle measured per lane; vocal-language safety checked separately".into());
    }
}

fn apply_events(
    record: &TacticTrackAnalysis,
    p: &Placement,
    anchors: &[u64],
    facts: &mut PassageFacts,
) {
    let Some(events) = record
        .signal_evidence
        .get("events")
        .and_then(Value::as_array)
    else {
        return;
    };
    let start = anchors[0];
    let end = *anchors.last().unwrap();
    let source_start = p.time_map.source_at(start as f64).round() as u64;
    let source_end = p.time_map.source_at(end as f64).round() as u64;
    for event in events {
        let Some(source) = event.get("frame").and_then(Value::as_u64) else {
            continue;
        };
        let Some(score) = num(event, "score").filter(|s| *s >= 0.6 && *s <= 1.) else {
            continue;
        };
        let kind = event.get("kind").and_then(Value::as_str);
        if kind == Some("build") {
            if let Some(release) =
                event
                    .get("end_frame")
                    .and_then(Value::as_u64)
                    .filter(|&release| {
                        source < release
                            && release <= record.source_frames
                            && source < source_end
                            && release >= source_start
                    })
            {
                let output = if release == source_start {
                    start
                } else if release > source_end {
                    end.saturating_add(1)
                } else {
                    output_at(&p.time_map, release as f64)
                        .map_or(end.saturating_add(1), |v| v.round() as u64)
                };
                facts.build_release_frame =
                    Some(facts.build_release_frame.unwrap_or(0).max(output));
                facts.roles.insert(TacticRole::Build, score);
                facts.evidence.push(format!(
                    "measured build span {source}–{release} source frames"
                ));
            }
            continue;
        }
        let Some(frame) = output_at(&p.time_map, source as f64).map(|v| v.round() as u64) else {
            continue;
        };
        if frame < start || frame > end {
            continue;
        }
        match kind {
            Some("drum_return") => {
                facts.roles.insert(TacticRole::DrumReturn, score);
                facts.arrival_frame = Some(frame);
            }
            Some("drop") => {
                facts.roles.insert(TacticRole::Drop, score);
                facts.arrival_frame = Some(frame);
            }
            Some("drum_attack") => {
                if facts.arrival_frame.is_none() {
                    facts.arrival_frame = Some(frame);
                }
            }
            Some("audible_start") if facts.vocal_phrases.is_some() => {
                facts.roles.insert(TacticRole::Intro, score);
                facts.arrival_frame = Some(frame);
                facts.phrase_boundaries.push(PhraseBoundary {
                    supported_period_bars: None,
                    output_frame: frame,
                    kind: BoundaryKind::Entry,
                    score_margin: score,
                    complete: true,
                });
            }
            Some("natural_ending")
                if facts.vocal_phrases.as_ref().is_some_and(|v| {
                    v.iter()
                        .all(|v| v.exit_supported && v.tail_end_frame <= frame)
                }) =>
            {
                facts.roles.insert(TacticRole::Outro, score);
                facts.natural_end_frame = Some(frame);
                facts.phrase_boundaries.push(PhraseBoundary {
                    supported_period_bars: None,
                    output_frame: frame,
                    kind: BoundaryKind::Exit,
                    score_margin: score,
                    complete: true,
                });
            }
            _ => {}
        }
    }
}

fn apply_motif(
    record: &TacticTrackAnalysis,
    p: &Placement,
    anchors: &[u64],
    facts: &mut PassageFacts,
) {
    if facts.stems.is_none() {
        return;
    }
    let Some(motifs) = record
        .signal_evidence
        .get("motifs")
        .and_then(Value::as_array)
    else {
        return;
    };
    for motif in motifs {
        let Some((start, end)) = motif
            .get("start_frame")
            .and_then(Value::as_u64)
            .zip(motif.get("end_frame").and_then(Value::as_u64))
        else {
            continue;
        };
        let Some((a, b)) =
            output_at(&p.time_map, start as f64).zip(output_at(&p.time_map, end as f64))
        else {
            continue;
        };
        if a < anchors[0] as f64 || b > *anchors.last().unwrap() as f64 {
            continue;
        }
        let Some(score) = num(motif, "score").filter(|s| *s >= 0.85) else {
            continue;
        };
        let lane = match motif.get("lane").and_then(Value::as_str) {
            Some("bass") => StemLane::Bass,
            Some("other") => StemLane::Other,
            Some("vocals") => StemLane::Vocals,
            _ => continue,
        };
        if lane == StemLane::Vocals
            && !facts.vocal_phrases.as_ref().is_some_and(|v| {
                v.iter().any(|phrase| {
                    phrase.start_frame == a.round() as u64
                        && phrase.end_frame == b.round() as u64
                        && phrase.entry_supported
                        && phrase.exit_supported
                })
            })
        {
            continue;
        }
        let Some(boundary) = facts
            .phrase_boundaries
            .iter()
            .filter(|p| {
                p.complete
                    && p.kind != BoundaryKind::Entry
                    && p.output_frame >= b.round() as u64
                    && p.output_frame <= *anchors.last().unwrap()
            })
            .min_by_key(|p| p.output_frame)
        else {
            continue;
        };
        facts.motif = Some(MotifEvidence {
            source_start_frame: start,
            source_end_frame: end,
            phrase_end_frame: boundary.output_frame,
            lane,
            isolation_score: score * (1. - facts.stems.as_ref().unwrap().artifact_score),
            transcription_margin: score - 0.5,
            sample: motif_sample(motif, start, end, record.source_sample_rate_hz),
        });
        break;
    }
}

fn motif_sample(motif: &Value, start: u64, end: u64, rate: u32) -> Option<MotifSample> {
    motif
        .get("notes")?
        .as_array()?
        .iter()
        .filter_map(|note| {
            if note.get("method")?.as_str()? != "normalized_lag_difference_harmonic_veto/1"
                || note.get("semantics")?.as_str()?
                    != "supported_single_fundamental_not_proof_of_one_instrument"
            {
                return None;
            }
            let a = note.get("start_frame")?.as_u64()?;
            let b = note.get("end_frame")?.as_u64()?;
            let score = num(note, "score")?;
            let midi = num(note, "midi_note")?;
            (a >= start
                && b <= end
                && b > a
                && b - a >= u64::from(rate) * 8 / 100
                && (0.90..=1.).contains(&score)
                && (0.0..=127.).contains(&midi))
            .then_some(MotifSample {
                source_start_frame: a,
                source_end_frame: b,
                midi_note: midi,
                single_fundamental_score: score,
            })
        })
        .max_by(|a, b| {
            a.single_fundamental_score
                .total_cmp(&b.single_fundamental_score)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn structural_coverage_uses_real_gap_descriptors_without_making_bars() {
        let signal = json!({"bars":[{"start_frame":100,"end_frame":200}],
            "unmetered_windows":[{"start_frame":200,"end_frame":250},{"start_frame":0,"end_frame":100}]});
        let rows = covering_rows(&signal, 50, 240).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["start_frame"], json!(0));
        assert_eq!(signal["bars"].as_array().unwrap().len(), 1);
        let mut gap = signal.clone();
        gap["unmetered_windows"][0]["start_frame"] = json!(201);
        assert!(covering_rows(&gap, 50, 240).is_none());
        let mut overlap = signal.clone();
        overlap["unmetered_windows"][0]["start_frame"] = json!(199);
        assert!(covering_rows(&overlap, 50, 240).is_none());
        assert!(covering_rows(&json!({"bars":[]}), 0, 200).is_none());
    }
    #[test]
    fn local_absence_requires_complete_coverage_and_versioned_provenance() {
        let span = |a: f64, b: f64| json!({"start_seconds":a,"end_seconds":b,"verification_method":"separated_vocal_rms_and_peak_negligible/1","reference":"provider:model"});
        let mut track = json!({"verified_absence_spans":[span(0.,1.),span(1.,3.)]});
        assert!(verified_absence(&track, 500, 2500, 1000));
        track["verified_absence_spans"][1]["start_seconds"] = json!(1.001);
        assert!(!verified_absence(&track, 500, 2500, 1000));
        track["verified_absence_spans"][1]["start_seconds"] = json!(1.);
        track["verified_absence_spans"][0]["reference"] = json!("");
        assert!(!verified_absence(&track, 500, 2500, 1000));
        assert!(!verified_absence(
            &json!({"words":[],"instrumental":false}),
            0,
            1000,
            1000
        ));
    }
    #[test]
    fn single_note_sample_needs_pitch_evidence_and_boundaries() {
        let mut motif = json!({"notes":[{"start_frame":100,"end_frame":300,"midi_note":60.,"score":0.95,
            "method":"normalized_lag_difference_harmonic_veto/1","semantics":"supported_single_fundamental_not_proof_of_one_instrument"}]});
        assert_eq!(
            motif_sample(&motif, 0, 400, 1000).unwrap().source_end_frame,
            300
        );
        assert!(motif_sample(&motif, 150, 400, 1000).is_none());
        motif["notes"][0]["score"] = json!(0.85);
        assert!(motif_sample(&motif, 0, 400, 1000).is_none());
    }
    #[test]
    fn mapped_leakage_keeps_veto_status_separate_from_projection_amplitude() {
        let plan: MixPlan =
            serde_json::from_str(include_str!("../../../examples/mix-plan.json")).unwrap();
        let p = &plan.placements[0];
        let first = p.time_map.anchors.first().unwrap();
        let last = p.time_map.anchors.last().unwrap();
        let anchors = [first.output_frame, last.output_frame];
        let mut row = json!({"start_frame":first.source_frame,"end_frame":last.source_frame,"vocal_leakage":{}});
        for lane in ["drums", "bass", "other"] {
            row["vocal_leakage"][lane] = json!({"method":"lagged_vocal_projection_and_coherence/1",
                "status":"not_detected","measured_window_count":4,"max_projection_ratio":2.,"suspect_spans":[]});
        }
        let mapped = mapped_leakage(&[&row], p, &anchors).unwrap();
        assert!(mapped.iter().all(|w| w.suspect_spans == Some(vec![])));
        row["vocal_leakage"]["other"]["status"] = json!("unassessed");
        assert!(mapped_leakage(&[&row], p, &anchors).unwrap()[2]
            .suspect_spans
            .is_none());
        row["vocal_leakage"]["other"]["method"] = json!("unknown");
        assert!(mapped_leakage(&[&row], p, &anchors).is_none());
    }

    #[test]
    fn stem_quality_preserves_unknown_leakage_and_reconstruction_measurements() {
        let mut row = json!({"reconstruction":{"relative_rms_error":0.02},"stems":{},"vocal_leakage":{},"vocal":{"acoustic_active":false}});
        for lane in ["drums", "bass", "other", "vocals"] {
            row["stems"][lane] =
                json!({"artifact":{"method":"pcm_discontinuity_added_clipping/1","score":0.01}});
            if lane != "vocals" {
                row["vocal_leakage"][lane] = json!({"status":"unassessed"});
            }
        }
        assert_eq!(
            stem_quality(&[&row], false, &mut vec![])
                .unwrap()
                .max_vocal_leakage_db,
            0.
        );
        let quiet = stem_quality(&[&row], true, &mut vec![]).unwrap();
        assert_eq!(quiet.reconstruction_error, 0.02);
        row["vocal"]["acoustic_active"] = json!(true);
        assert_eq!(
            stem_quality(&[&row], true, &mut vec![])
                .unwrap()
                .max_vocal_leakage_db,
            0.
        );
        for lane in ["drums", "bass", "other"] {
            row["vocal_leakage"][lane] =
                json!({"status":"not_detected","max_projection_ratio":0.01});
        }
        assert_eq!(
            stem_quality(&[&row], true, &mut vec![])
                .unwrap()
                .max_vocal_leakage_db,
            0.
        );
        row["vocal_leakage"]["other"]["status"] = json!("suspected");
        assert_eq!(
            stem_quality(&[&row], true, &mut vec![])
                .unwrap()
                .max_vocal_leakage_db,
            0.
        );
    }
}
