//! Read-only adapter from saved source analysis and prepared-stem evidence to
//! tactic facts. Unknown semantic properties never become positive defaults.
use crate::cached_tactic_evidence::{apply_cached_tactic_facts, load_tactic_track_analysis};
use crate::tactics::*;
use mixlib_analysis::{analyze_segments, apply_overrides};
use mixlib_audio::{automation, WavReader};
use mixlib_catalog::{validate_analysis, Store};
use mixlib_types::*;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

struct Snapshot {
    track: Track,
    beats: Beats,
    segments: SegmentAnalysis,
    features: Vec<FeatureFrame>,
    provenance: Vec<String>,
    phase: BTreeMap<u64, crate::phrase::PhrasePosition>,
}

/// Reuse immutable source analysis during one read-only search operation.
/// Cue bounds/maps, PCM vocal checks, gains and pair profiles are never cached.
/// The caller must use the same Store::read_snapshot view for every call and
/// verify_read_snapshot before accepting the result, followed by fresh source
/// validation before publication/rendering.
#[derive(Default)]
pub struct CandidateEvidenceCache {
    store: Option<Store>,
    sources: BTreeMap<String, Arc<SourceEvidence>>,
}
struct SourceEvidence {
    snapshot: Snapshot,
    tactic: Option<TacticTrackAnalysis>,
}
impl CandidateEvidenceCache {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cached_sources(&self) -> usize {
        self.sources.len()
    }
    fn source(
        &mut self,
        store: &Store,
        p: &Placement,
        cancel: &Cancellation,
    ) -> Result<Arc<SourceEvidence>> {
        cancel.check()?;
        require(
            store.read_snapshot_stats()?.is_some(),
            "TACTIC_READ_SNAPSHOT",
            "candidate evidence cache requires an operation-scoped Store::read_snapshot",
        )?;
        if let Some(bound) = &self.store {
            require(
                bound.shares_read_snapshot(store),
                "TACTIC_READ_SNAPSHOT",
                "candidate evidence cache cannot be reused across metadata snapshots",
            )?;
        } else {
            self.store = Some(store.clone());
        }
        let key = canonical_hash(&serde_json::json!({"track":p.track_id,"audio":p.audio_hash,
            "revision":p.audio_revision,"analysis":p.analysis,"override":p.override_revision,
            "source_rate":p.time_map.source_sample_rate_hz}))?
        .digest;
        if let Some(evidence) = self.sources.get(&key) {
            validate_placement_source(&evidence.snapshot.track, p)?;
            return Ok(Arc::clone(evidence));
        }
        let evidence = Arc::new(SourceEvidence {
            snapshot: read_snapshot(store, p, cancel)?,
            tactic: load_tactic_track_analysis(store, p)?,
        });
        self.sources.insert(key, Arc::clone(&evidence));
        Ok(evidence)
    }
    pub fn candidate_facts(
        &mut self,
        store: &Store,
        plan: &MixPlan,
        transition_index: usize,
        cancel: &Cancellation,
    ) -> Result<CandidateFacts> {
        candidate_facts_cached(store, plan, transition_index, cancel, self)
    }
}

/// One-shot read-only adapter. Repeated search uses CandidateEvidenceCache.
pub fn candidate_facts(
    store: &Store,
    plan: &MixPlan,
    transition_index: usize,
    cancel: &Cancellation,
) -> Result<CandidateFacts> {
    cancel.check()?;
    let view = store.read_snapshot();
    let result =
        CandidateEvidenceCache::new().candidate_facts(&view, plan, transition_index, cancel)?;
    view.verify_read_snapshot(cancel)?;
    Ok(result)
}

fn candidate_facts_cached(
    store: &Store,
    plan: &MixPlan,
    transition_index: usize,
    cancel: &Cancellation,
    evidence_cache: &mut CandidateEvidenceCache,
) -> Result<CandidateFacts> {
    cancel.check()?;
    let transition = plan
        .transitions
        .get(transition_index)
        .ok_or_else(|| Error::new("TACTIC_TRANSITION", "transition index is outside the plan"))?;
    let index = plan
        .placements
        .windows(2)
        .position(|p| {
            p[0].track_id == transition.from_track_id
                && p[1].track_id == transition.to_track_id
                && covers(
                    &p[0].time_map,
                    transition.start_output_frame,
                    transition.end_output_frame,
                )
                && covers(
                    &p[1].time_map,
                    transition.start_output_frame,
                    transition.end_output_frame,
                )
        })
        .ok_or_else(|| {
            Error::new(
                "TACTIC_PLACEMENTS",
                "transition has no adjacent covering placements",
            )
        })?;
    let placements = [&plan.placements[index], &plan.placements[index + 1]];
    for placement in placements {
        placement.time_map.validate()?;
        require(
            placement.time_map.output_sample_rate_hz == plan.output.sample_rate_hz,
            "TACTIC_TIMEBASE",
            "placement and plan output rates differ",
        )?;
    }
    let source_evidence = [
        evidence_cache.source(store, placements[0], cancel)?,
        evidence_cache.source(store, placements[1], cancel)?,
    ];
    let snapshots = [&source_evidence[0].snapshot, &source_evidence[1].snapshot];
    let timing_marker = plan
        .header
        .extensions
        .get("tactic_cue_candidate")
        .and_then(|v| v.get("timing_basis"))
        .and_then(Value::as_str);
    let explicit_structural = timing_marker == Some("structural");
    let grid = if explicit_structural {
        Err(Error::new(
            "TACTIC_BAR_WINDOW",
            "saved cue explicitly uses structural timing",
        ))
    } else {
        bar_anchors(
            snapshots[0],
            &placements[0].time_map,
            transition.start_output_frame,
            transition.end_output_frame,
        )
    };
    let (timing_basis, bar_frames, structural_reason) = match grid {
        Ok(bars) => (
            if timing_marker == Some("independent_pulses") {
                TacticTimingBasis::IndependentPulses
            } else {
                TacticTimingBasis::Bars
            },
            bars,
            None,
        ),
        Err(error) if error.code == "TACTIC_BAR_WINDOW" => (
            TacticTimingBasis::Structural,
            vec![transition.start_output_frame, transition.end_output_frame],
            Some(error.message),
        ),
        Err(error) => return Err(error),
    };
    let requested_pulse_map = plan
        .header
        .extensions
        .get("tactic_cue_candidate")
        .and_then(|v| v.get("pulse_ratio"))
        .and_then(Value::as_f64)
        .is_some_and(|ratio| (ratio - 2.).abs() < 0.02 || (ratio - 0.5).abs() < 0.01);
    let native_bars = timing_marker == Some("independent_pulses") || requested_pulse_map;
    let incoming_anchors = if native_bars {
        native_bar_anchors(
            snapshots[1],
            &placements[1].time_map,
            transition.start_output_frame,
            transition.end_output_frame,
        )?
    } else {
        bar_frames.clone()
    };
    let prepared = prepared_evidence(plan, transition_index);
    let mut a = passage_facts(
        snapshots[0],
        placements[0],
        index,
        &bar_frames,
        prepared,
        "outgoing",
        plan.output.sample_rate_hz,
    );
    let mut b = passage_facts(
        snapshots[1],
        placements[1],
        index + 1,
        &incoming_anchors,
        prepared,
        "incoming",
        plan.output.sample_rate_hz,
    );
    if let Some(reason) = structural_reason {
        structural_passage(snapshots[0], placements[0], &bar_frames, &mut a, &reason);
        structural_passage(snapshots[1], placements[1], &bar_frames, &mut b, &reason);
    }
    let cached = [&source_evidence[0].tactic, &source_evidence[1].tactic];
    if let Some(record) = &cached[0] {
        apply_cached_tactic_facts(store, record, placements[0], &bar_frames, &mut a, cancel)?;
    }
    if let Some(record) = &cached[1] {
        apply_cached_tactic_facts(store, record, placements[1], &bar_frames, &mut b, cancel)?;
    }
    // PCM inspection is bounded to short candidate seams and only follows
    // affirmative vocal evidence. Stereo energy is never a vocal classifier.
    add_loop_candidates(store, snapshots[0], placements[0], &mut a, cancel)?;
    add_loop_candidates(store, snapshots[1], placements[1], &mut b, cancel)?;
    let beat_skew_ms = (timing_basis == TacticTimingBasis::Bars)
        .then(|| {
            mapped_beat_skew(
                &snapshots,
                placements,
                &bar_frames,
                plan.output.sample_rate_hz,
            )
        })
        .flatten();
    let harmonic_distance = if timing_basis == TacticTimingBasis::Structural {
        window_chroma(snapshots[0], placements[0], &bar_frames)
            .zip(window_chroma(snapshots[1], placements[1], &bar_frames))
            .map(|(a, b)| chroma_distance(&a, &b))
    } else {
        a.bars
            .iter()
            .zip(&b.bars)
            .map(|(a, b)| a.chroma.zip(b.chroma).map(|(a, b)| chroma_distance(&a, &b)))
            .collect::<Option<Vec<_>>>()
            .map(|v| v.into_iter().fold(0., f64::max))
    };
    let tempo_maps_validated = beat_skew_ms.is_some_and(|s| s <= 30.)
        && a.bars
            .iter()
            .zip(&b.bars)
            .all(|(a, b)| a.beats_per_bar.is_some() && a.beats_per_bar == b.beats_per_bar);
    let shared_bar_bpm = tempo_maps_validated.then(|| {
        let mut values: Vec<_> = a
            .bars
            .iter()
            .map(|bar| {
                60. * f64::from(bar.beats_per_bar.unwrap()) * f64::from(plan.output.sample_rate_hz)
                    / (bar.end_frame - bar.start_frame) as f64
            })
            .collect();
        values.push(*values.last().unwrap());
        values
    });
    let mean_tempo = |s: &Snapshot, p: &Placement| -> Option<f64> {
        let frames = (p.time_map.source_at(*bar_frames.last()? as f64)
            - p.time_map.source_at(bar_frames[0] as f64))
        .max(0.);
        let pulses: Option<u32> = bar_frames
            .windows(2)
            .map(|w| matched_tile(s, &p.time_map, w[0], w[1]).map(|t| t.pulses_per_bar))
            .try_fold(0u32, |sum, n| n.map(|n| sum + n));
        pulses
            .filter(|_| frames > 0.)
            .map(|n| 60. * f64::from(n) * f64::from(s.track.format.sample_rate_hz) / frames)
    };
    let target_tempo_ratio = mean_tempo(snapshots[0], placements[0])
        .zip(mean_tempo(snapshots[1], placements[1]))
        .map(|(a, b)| b / a);
    let pair = PairEvidence {
        beat_skew_ms,
        harmonic_distance,
        target_tempo_ratio,
        predicted_peak_dbfs: predicted_dry_peak(&snapshots, placements, &bar_frames),
        shared_bar_bpm,
        tempo_maps_validated,
        selected_composite_density: density_values(
            prepared,
            "selected_composite",
            &bar_frames,
            plan.output.sample_rate_hz,
        ),
        retained_bed_density: density_values(
            prepared,
            "retained_bed",
            &bar_frames,
            plan.output.sample_rate_hz,
        ),
        // Chroma does not identify lead conflicts; BPM ratios do not establish
        // perceived half-time; titles do not establish a semantic relationship.
        ..PairEvidence::default()
    };
    let mut result = CandidateFacts {
        output_sample_rate_hz: plan.output.sample_rate_hz,
        timing_basis,
        bar_frames,
        a,
        b,
        pair,
    };
    if let (Some(a), Some(b)) = (&cached[0], &cached[1]) {
        crate::tactic_pair_evidence::enrich_pair_evidence(a, b, placements, &mut result)?;
        if result.timing_basis == TacticTimingBasis::Bars
            && result.bar_frames.len() <= 3
            && plan.drum_intro.is_none()
            && plan.prepared_patches.is_empty()
            && result
                .pair
                .profiles
                .get(&TacticId::S02)
                .is_some_and(|p| p.predicted_peak_dbfs.is_some())
        {
            let paths = [
                store.source(&snapshots[0].track)?,
                store.source(&snapshots[1].track)?,
            ];
            result.pair.short_fade_match = crate::short_fade::measure(
                store,
                [&paths[0], &paths[1]],
                placements,
                [a, b],
                [transition.start_output_frame, transition.end_output_frame],
                plan.crossover_hz,
                cancel,
            )?;
        }
    }
    result.validate()?;
    Ok(result)
}

fn covers(map: &TimeMap, start: u64, end: u64) -> bool {
    map.anchors.first().is_some_and(|a| a.output_frame <= start)
        && map.anchors.last().is_some_and(|a| a.output_frame >= end)
}

fn validate_placement_source(track: &Track, p: &Placement) -> Result<()> {
    require(
        p.time_map.source_sample_rate_hz == track.format.sample_rate_hz
            && p.source_start_frame < p.source_end_frame
            && p.source_end_frame <= track.format.frames
            && p.time_map
                .anchors
                .first()
                .is_some_and(|a| a.source_frame == p.source_start_frame)
            && p.time_map
                .anchors
                .last()
                .is_some_and(|a| a.source_frame == p.source_end_frame),
        "TACTIC_SOURCE_TIMEBASE",
        "placement does not retain the source rate or frame bounds",
    )
}

fn read_snapshot(store: &Store, p: &Placement, cancel: &Cancellation) -> Result<Snapshot> {
    let track = store.track(&p.track_id)?;
    validate_placement_source(&track, p)?;
    require(
        track.audio_hash == p.audio_hash && track.audio_revision == p.audio_revision,
        "SOURCE_CHANGED",
        "tactic facts require the saved audio revision",
    )?;
    require(
        track.override_revision == p.override_revision,
        "OVERRIDE_CHANGED",
        "corrections changed since the saved plan; replan",
    )?;
    let analysis: AnalysisManifest = store.read_ref(&p.analysis)?;
    require(
        analysis.audio_hash == p.audio_hash,
        "ANALYSIS_STALE",
        "saved analysis belongs to different audio",
    )?;
    validate_analysis(store, &analysis)?;
    let mut beats: Beats = store.read_ref(&analysis.artifacts["beats"])?;
    let mut regions: Regions = store.read_ref(&analysis.artifacts["regions"])?;
    let harmony: Harmony = store.read_ref(&analysis.artifacts["harmony"])?;
    let overrides = store.overrides(&track)?;
    apply_overrides(&track, &p.analysis, &mut beats, &mut regions, &overrides)?;
    let mut features = vec![];
    for reference in &harmony.feature_chunks {
        cancel.check()?;
        let chunk: FeatureChunk = store.read_ref(reference)?;
        features.extend(chunk.frames);
    }
    require(
        features
            .windows(2)
            .all(|w| w[0].source_frame < w[1].source_frame),
        "FEATURE_ORDER",
        "saved feature chunks overlap or are out of order",
    )?;
    let derived = !overrides.operations.is_empty() || !analysis.artifacts.contains_key("segments");
    let segments = if derived {
        analyze_segments(&beats, &features, &track.format)?
    } else {
        store.read_ref::<SegmentAnalysis>(&analysis.artifacts["segments"])?
    };
    require(
        segments.source_sample_rate_hz == track.format.sample_rate_hz
            && segments.source_frames == track.format.frames,
        "SEGMENT_TIMEBASE",
        "segment analysis belongs to a different source timebase",
    )?;
    let provenance=vec![format!("analysis:{} hash:{}",p.analysis.path,p.analysis.hash.digest),
        format!("correction_revision:{}",p.override_revision),
        format!("segments:{} {}; {}",segments.method,
            if derived {"derived read-only from corrected beats and saved features"}else{"saved generation"},segments.score_semantics),
        "unmeasured lead identity, semantic relation, pulse reinterpretation and stem artifact score remain unknown".into()];
    let phase = crate::phrase::infer_phrase_positions(&beats, &features);
    Ok(Snapshot {
        phase,
        track,
        beats,
        segments,
        features,
        provenance,
    })
}

/// Invert the actual monotone cubic map, not a linear approximation between cues.
pub(crate) fn output_at(map: &TimeMap, source: f64) -> Option<f64> {
    let first = map.anchors.first()?;
    let last = map.anchors.last()?;
    if !source.is_finite()
        || source < first.source_frame as f64
        || source > last.source_frame as f64
    {
        return None;
    }
    let (mut lo, mut hi) = (first.output_frame as f64, last.output_frame as f64);
    for _ in 0..56 {
        let mid = (lo + hi) * 0.5;
        if map.source_at(mid) < source {
            lo = mid
        } else {
            hi = mid
        }
    }
    Some((lo + hi) * 0.5)
}

fn bar_anchors(s: &Snapshot, map: &TimeMap, start: u64, end: u64) -> Result<Vec<u64>> {
    let ids: BTreeSet<_> = s.beats.downbeat_ids.iter().collect();
    let mut boundaries: Vec<_> = s
        .beats
        .events
        .iter()
        .filter(|b| ids.contains(&b.event_id))
        .filter_map(|b| output_at(map, b.source_frame as f64))
        .filter(|f| *f >= start as f64 - 1.5 && *f <= end as f64 + 1.5)
        .map(|f| f.round() as u64)
        .collect();
    require(
        boundaries.len() >= 2
            && boundaries[0].abs_diff(start) <= 1
            && boundaries.last().unwrap().abs_diff(end) <= 1,
        "TACTIC_BAR_WINDOW",
        "candidate must begin and end on supported downbeats; no fixed-meter grid is synthesized",
    )?;
    boundaries[0] = start;
    *boundaries.last_mut().unwrap() = end;
    require(
        boundaries
            .windows(2)
            .all(|w| w[0] < w[1] && matched_tile(s, map, w[0], w[1]).is_some()),
        "TACTIC_BAR_WINDOW",
        "the candidate crosses unreliable or contradictory bar/meter evidence",
    )?;
    Ok(boundaries)
}

fn native_bar_anchors(s: &Snapshot, map: &TimeMap, start: u64, end: u64) -> Result<Vec<u64>> {
    let ids: BTreeSet<_> = s.beats.downbeat_ids.iter().collect();
    let anchors: Vec<_> = s
        .beats
        .events
        .iter()
        .filter(|b| ids.contains(&b.event_id))
        .filter_map(|b| output_at(map, b.source_frame as f64))
        .filter(|f| *f >= start as f64 && *f <= end as f64)
        .map(|f| f.round() as u64)
        .collect();
    require(
        anchors.len() >= 2
            && anchors
                .windows(2)
                .all(|w| w[0] < w[1] && matched_tile(s, map, w[0], w[1]).is_some()),
        "TACTIC_NATIVE_GRID",
        "independent incoming clock requires supported complete native bars",
    )?;
    Ok(anchors)
}

fn matched_tile<'a>(
    s: &'a Snapshot,
    map: &TimeMap,
    start: u64,
    end: u64,
) -> Option<&'a SegmentTile> {
    let tolerance = f64::from(map.output_sample_rate_hz) * 0.030;
    s.segments.tiles.iter().filter(|t| t.bars == 1).find(|t| {
        output_at(map, t.start_frame as f64)
            .zip(output_at(map, t.end_frame as f64))
            .is_some_and(|(a, b)| {
                (a - start as f64).abs() <= tolerance && (b - end as f64).abs() <= tolerance
            })
    })
}

fn prepared_evidence(plan: &MixPlan, index: usize) -> Option<&Value> {
    if plan.transitions.len() == 1 {
        return plan.header.extensions.get("stem_evidence");
    }
    plan.header
        .extensions
        .get("chain_joins")?
        .as_array()?
        .get(index)?
        .get("stem_evidence")
}

fn passage_facts(
    s: &Snapshot,
    p: &Placement,
    index: usize,
    anchors: &[u64],
    prepared: Option<&Value>,
    role: &str,
    rate: u32,
) -> PassageFacts {
    let mut evidence = s.provenance.clone();
    let clip = prepared
        .and_then(|v| v.get("clips"))
        .and_then(Value::as_array)
        .and_then(|clips| {
            clips
                .iter()
                .find(|c| c.get("role").and_then(Value::as_str) == Some(role))
        });
    let mut bars = vec![];
    let mut roles: BTreeMap<TacticRole, f64> = BTreeMap::new();
    for (i, w) in anchors.windows(2).enumerate() {
        let tile = matched_tile(s, &p.time_map, w[0], w[1]);
        let active_stems = clip
            .and_then(|v| v.get("bars"))
            .and_then(Value::as_array)
            .and_then(|v| v.get(i))
            .and_then(|v| v.get("stem_activity_fraction"))
            .and_then(Value::as_object)
            .and_then(|m| {
                let vals = m.values().map(Value::as_f64).collect::<Option<Vec<_>>>()?;
                (vals.len() == 4 && vals.iter().all(|v| v.is_finite() && (0.0..=1.).contains(v)))
                    .then(|| vals.iter().filter(|v| **v >= 0.6).count() as u8)
            });
        if let Some(tile) = tile {
            for (name, score) in &tile.roles {
                if let Some(key) = tactic_role(name) {
                    *roles.entry(key).or_default() += score / (anchors.len() - 1) as f64;
                }
            }
        }
        bars.push(BarEvidence {
            start_frame: w[0],
            end_frame: w[1],
            rms: tile.map(|t| t.rms),
            crest_db: tile.map(|t| t.crest_factor_db.max(0.)),
            onsets_per_beat: tile.map(|t| t.onset_rate_hz * 60. / t.local_bpm),
            low_occupancy: tile.map(|t| t.band_occupancy[0]),
            mid_occupancy: tile.map(|t| t.band_occupancy[1]),
            high_occupancy: tile.map(|t| t.band_occupancy[2]),
            active_stems,
            chroma: tile.map(|t| t.chroma),
            beats_per_bar: tile.map(|t| t.pulses_per_bar as u16),
            grid_score: tile.map(|t| t.pulse_score_min),
            stability: tile.map(|t| {
                // Include surrounding same-meter tiles: a single tile's default
                // variation=0 does not establish an eight-bar stable passage.
                let full: Vec<_> = s
                    .segments
                    .tiles
                    .iter()
                    .filter(|n| {
                        n.bars == 1
                            && n.start_frame
                                >= p.time_map.source_at(anchors[0] as f64).round() as u64
                            && n.end_frame
                                <= p.time_map
                                    .source_at(*anchors.last().unwrap() as f64)
                                    .round() as u64
                    })
                    .collect();
                let mean = full.iter().map(|n| n.rms).sum::<f64>() / full.len().max(1) as f64;
                (1. - (t.rms - mean).abs() / mean.max(1e-9) - t.harmonic_variation).clamp(0., 1.)
            }),
        });
    }
    let mut phrase_boundaries = vec![];
    for boundary in &s.segments.boundaries {
        if boundary.repeated_support < 2 || boundary.supported_period_bars.is_none() {
            continue;
        }
        if let Some(frame) = output_at(&p.time_map, boundary.source_frame as f64) {
            if frame < anchors[0] as f64 || frame > *anchors.last().unwrap() as f64 {
                continue;
            }
            let margin = structural_margin(s, boundary);
            phrase_boundaries.push(PhraseBoundary {
                supported_period_bars: boundary.supported_period_bars.map(|n| n as u16),
                output_frame: frame.round() as u64,
                kind: BoundaryKind::Both,
                score_margin: margin,
                complete: margin >= 0.20,
            });
        }
    }
    add_phase_boundaries(
        &s.phase,
        &p.time_map,
        anchors,
        &mut phrase_boundaries,
        &mut evidence,
    );
    let vocal_phrases = parse_vocal_phrases(prepared, clip, role, anchors[0], rate, &mut evidence);
    let natural_end_frame = natural_end(s, p);
    if let Some(frame) = natural_end_frame {
        phrase_boundaries.push(PhraseBoundary {
            supported_period_bars: None,
            output_frame: frame,
            kind: BoundaryKind::Exit,
            score_margin: 1.,
            complete: true,
        });
        roles.insert(TacticRole::Outro, 1.);
    }
    let attack_at = |frame: u64| {
        let source = p.time_map.source_at(frame as f64);
        let radius = f64::from(s.track.format.sample_rate_hz) * 0.075;
        let lo = s
            .features
            .partition_point(|f| (f.source_frame as f64) < source - radius);
        s.features[lo..]
            .iter()
            .take_while(|f| f.source_frame as f64 <= source + radius)
            .any(|f| f.rms >= 0.01 && f.onset >= f.rms * 0.18)
    };
    let arrival_frame = phrase_boundaries
        .iter()
        .filter(|b| b.complete && b.kind != BoundaryKind::Exit)
        .find_map(|boundary| attack_at(boundary.output_frame).then_some(boundary.output_frame));
    let cut_attacks = anchors
        .iter()
        .copied()
        .filter(|&frame| attack_at(frame))
        .collect();
    evidence.push("basic stereo analysis does not establish isolated-stem quality; supplementary source-aligned evidence is required".into());
    PassageFacts {
        placement: index,
        source: SourceWindow {
            sample_rate_hz: s.track.format.sample_rate_hz,
            start_frame: p.source_start_frame,
            end_frame: p.source_end_frame,
            output_start_frame: p.time_map.anchors[0].output_frame,
            output_end_frame: p.time_map.anchors.last().unwrap().output_frame,
        },
        bars,
        roles,
        phrase_boundaries,
        vocal_phrases,
        vocal_quiet: None,
        stems: None,
        stem_assets: vec![],
        loop_candidates: vec![],
        motif: None,
        version_identity: None,
        natural_end_frame,
        arrival_frame,
        cut_attacks,
        build_release_frame: None,
        ready_buffer_frames: 0,
        evidence,
    }
}

fn add_phase_boundaries(
    positions: &BTreeMap<u64, crate::phrase::PhrasePosition>,
    map: &TimeMap,
    anchors: &[u64],
    boundaries: &mut Vec<PhraseBoundary>,
    evidence: &mut Vec<String>,
) {
    for (&source, position) in positions {
        if !position.boundary || position.confidence < 0.20 || position.supporting_changes < 2 {
            continue;
        }
        let Some(frame) = output_at(map, source as f64).map(|v| v.round() as u64) else {
            continue;
        };
        if frame < anchors[0] || frame > *anchors.last().unwrap() {
            continue;
        }
        evidence.push(format!("four_bar_phase/1 source_frame:{source} output_frame:{frame} anchor_source_frame:{} supporting_changes:{} best_runner_up_margin:{:.6}; supported_period_bars:4 only, not an eight/sixteen-bar phrase or lyric-completion claim",position.anchor_frame,position.supporting_changes,position.confidence));
        if !boundaries
            .iter()
            .any(|b| b.output_frame == frame && b.complete)
        {
            boundaries.push(PhraseBoundary {
                output_frame: frame,
                supported_period_bars: Some(4),
                kind: BoundaryKind::Both,
                score_margin: position.confidence,
                complete: true,
            });
        }
    }
    boundaries.sort_by_key(|b| b.output_frame);
}

fn tactic_role(name: &str) -> Option<TacticRole> {
    Some(match name {
        "rest" => TacticRole::Rest,
        "low_percussion_bed" => TacticRole::LowPercussionBed,
        "sustained_texture" => TacticRole::SustainedTexture,
        "drum_return" => TacticRole::DrumReturn,
        "steady_groove" => TacticRole::SteadyGroove,
        "mixed" => TacticRole::Mixed,
        "intro" => TacticRole::Intro,
        "outro" => TacticRole::Outro,
        "breakdown" => TacticRole::Breakdown,
        "build" => TacticRole::Build,
        "drop" => TacticRole::Drop,
        "verse" => TacticRole::Verse,
        "chorus" => TacticRole::Chorus,
        "vocal_foreground" => TacticRole::VocalForeground,
        _ => return None,
    })
}

fn window_features<'a>(s: &'a Snapshot, p: &Placement, anchors: &[u64]) -> Vec<&'a FeatureFrame> {
    let start = p.time_map.source_at(anchors[0] as f64);
    let end = p.time_map.source_at(*anchors.last().unwrap() as f64);
    s.features
        .iter()
        .filter(|f| f.source_frame as f64 >= start && (f.source_frame as f64) < end)
        .collect()
}

fn window_chroma(s: &Snapshot, p: &Placement, anchors: &[u64]) -> Option<[f64; 12]> {
    let features = window_features(s, p, anchors);
    if features.is_empty() {
        return None;
    }
    let mut chroma: [f64; 12] =
        std::array::from_fn(|i| features.iter().map(|f| f.chroma[i] * f.rms).sum());
    let total = chroma.iter().sum::<f64>();
    if total <= 1e-12 {
        return None;
    }
    chroma.iter_mut().for_each(|v| *v /= total);
    Some(chroma)
}

fn structural_passage(
    s: &Snapshot,
    p: &Placement,
    anchors: &[u64],
    facts: &mut PassageFacts,
    reason: &str,
) {
    facts.bars.clear();
    facts.roles.clear();
    facts.evidence.push(format!(
        "structural timing uses saved transition endpoints, with no inferred bars: {reason}"
    ));
    let features = window_features(s, p, anchors);
    if features.is_empty() {
        return;
    }
    let rms = (features.iter().map(|f| f.rms * f.rms).sum::<f64>() / features.len() as f64).sqrt();
    let seconds = (p.time_map.source_at(*anchors.last().unwrap() as f64)
        - p.time_map.source_at(anchors[0] as f64))
        / f64::from(s.track.format.sample_rate_hz);
    let mut last = None;
    let count = features
        .iter()
        .filter(|f| {
            let onset = f.onset >= (rms * 0.12).max(0.0005)
                && last.is_none_or(|n| {
                    f.source_frame - n >= u64::from(s.track.format.sample_rate_hz) / 12
                });
            if onset {
                last = Some(f.source_frame)
            }
            onset
        })
        .count();
    let rate = count as f64 / seconds.max(1e-9);
    let rest = (1. - rms / 0.002).clamp(0., 1.);
    let texture = (1. - rate / 1.5).clamp(0., 1.) * (1. - rest);
    facts.roles.insert(TacticRole::Rest, rest);
    facts.roles.insert(TacticRole::SustainedTexture, texture);
    facts.roles.insert(TacticRole::LowPercussionBed, texture);
    facts.evidence.push(format!("whole structural window: RMS {rms:.6}, measured onset rate {rate:.4}/s; no verse/chorus/drop or vocal inference"));
    // A natural source beginning and an audibly completed file tail are explicit
    // structural endpoints. Arbitrary internal silence creates no phrase claim.
    if let Some(first) = s.features.iter().find(|f| f.rms >= 0.003 || f.peak >= 0.01) {
        let source_start = p.time_map.source_at(anchors[0] as f64);
        let lead = f64::from(s.track.format.sample_rate_hz) * 0.15;
        if source_start <= first.source_frame as f64
            && first.source_frame as f64 - source_start <= lead
        {
            facts.roles.insert(TacticRole::Intro, 1.);
            facts.phrase_boundaries.push(PhraseBoundary {
                supported_period_bars: None,
                output_frame: anchors[0],
                kind: BoundaryKind::Entry,
                score_margin: 1.,
                complete: true,
            });
            facts.arrival_frame = Some(anchors[0]);
            facts.evidence.push("entry preserves the first measured audible source attack, including its leading pickup".into());
        }
    }
    if facts.natural_end_frame.is_some() {
        facts.roles.insert(TacticRole::Outro, 1.);
    }
}

fn structural_margin(s: &Snapshot, b: &StructuralBoundary) -> f64 {
    let singles: Vec<_> = s.segments.tiles.iter().filter(|t| t.bars == 1).collect();
    let Some(at) = singles.iter().position(|t| t.start_frame == b.source_frame) else {
        return 0.;
    };
    let period = b.supported_period_bars.unwrap_or(0) as usize;
    if period == 0 {
        return 0.;
    }
    let first = at.saturating_sub(period * 2);
    let competitors = s
        .segments
        .boundaries
        .iter()
        .filter(|n| {
            n.source_frame >= singles[first].start_frame && n.source_frame <= b.source_frame
        })
        .filter_map(|n| {
            let i = singles
                .iter()
                .position(|t| t.start_frame == n.source_frame)?;
            ((at - i) % period != 0).then_some(n.novelty)
        })
        .fold(0., f64::max);
    (b.novelty - competitors).clamp(0., 1.)
}

fn natural_end(s: &Snapshot, p: &Placement) -> Option<u64> {
    if p.source_end_frame != s.track.format.frames {
        return None;
    }
    let end = s.track.format.frames;
    let tail = u64::from(s.track.format.sample_rate_hz) / 2;
    let frames: Vec<_> = s
        .features
        .iter()
        .filter(|f| f.source_frame >= end.saturating_sub(tail))
        .collect();
    if frames.len() < 3 || frames.iter().any(|f| f.rms > 0.001 || f.peak > 0.005) {
        return None;
    }
    output_at(&p.time_map, end as f64).map(|f| f.round() as u64)
}

fn parse_vocal_phrases(
    prepared: Option<&Value>,
    clip: Option<&Value>,
    role: &str,
    offset: u64,
    rate: u32,
    evidence: &mut Vec<String>,
) -> Option<Vec<VocalPhrase>> {
    let phrases = prepared?.get("vocal_phrase_evidence")?;
    let method = phrases.get("method")?.as_str()?;
    let reference = phrases.get("reference")?.as_str()?;
    if method.is_empty() || reference.is_empty() {
        return None;
    }
    let side = phrases.get(role)?;
    let active = clip?
        .get("acoustic_vocals")?
        .get("active_spans")?
        .as_array()?;
    evidence.push(format!(
        "vocal_provider:{method}; reference:{reference}; role:{role}"
    ));
    if side.get("instrumental").and_then(Value::as_bool) == Some(true) && active.is_empty() {
        evidence.push("affirmative provider instrumental classification agrees with measured vocal inactivity".into());
        return Some(vec![]);
    }
    let source = side.get("phrases")?.as_array()?;
    if source.is_empty() {
        return None;
    }
    let to_frame = |seconds: f64| {
        let frame = offset as f64 + seconds * f64::from(rate);
        (frame.is_finite() && (0.0..=MAX_FRAME as f64).contains(&frame))
            .then(|| frame.round() as u64)
    };
    let mut bounds = vec![];
    for phrase in source {
        let start = phrase.get("start_seconds")?.as_f64()?;
        let end = phrase.get("end_seconds")?.as_f64()?;
        let confidence = phrase.get("confidence_margin")?.as_f64()?;
        if start >= end
            || !confidence.is_finite()
            || confidence < 0.20
            || phrase.get("entry_supported")?.as_bool() != Some(true)
            || phrase.get("exit_supported")?.as_bool() != Some(true)
        {
            return None;
        }
        bounds.push((start, end, confidence, end));
    }
    for span in active {
        let start = span.get(0)?.as_f64()?;
        let end = span.get(1)?.as_f64()?;
        let covered = bounds
            .iter_mut()
            .find(|(a, b, _, _)| start >= a - 0.15 && end <= b + 0.5)?;
        covered.3 = covered.3.max(end);
    }
    let result: Option<Vec<_>> = bounds
        .into_iter()
        .map(|(a, b, score, tail)| {
            Some(VocalPhrase {
                start_frame: to_frame(a)?,
                end_frame: to_frame(b)?,
                tail_end_frame: to_frame(tail)?,
                entry_supported: true,
                exit_supported: true,
                confidence_margin: score,
            })
        })
        .collect();
    result.filter(|v| v.windows(2).all(|w| w[0].end_frame <= w[1].start_frame))
}

fn density_values(
    prepared: Option<&Value>,
    key: &str,
    anchors: &[u64],
    rate: u32,
) -> Option<Vec<f64>> {
    let data = prepared?.get(key)?;
    if data.get("method")?.as_str()? != "relative_level_crest_active_lanes/1" {
        return None;
    }
    let bars = data.get("bars")?.as_array()?;
    if bars.len() + 1 != anchors.len() {
        return None;
    }
    bars.iter()
        .enumerate()
        .map(|(i, b)| {
            let start = b.get("start_seconds")?.as_f64()?;
            let end = b.get("end_seconds")?.as_f64()?;
            let density = b.get("density_score")?.as_f64()?;
            (density.is_finite()
                && (0.0..=1.).contains(&density)
                && (start * f64::from(rate) - (anchors[i] - anchors[0]) as f64).abs() <= 1.
                && (end * f64::from(rate) - (anchors[i + 1] - anchors[0]) as f64).abs() <= 1.)
                .then_some(density)
        })
        .collect()
}

fn mapped_beat_skew(
    s: &[&Snapshot; 2],
    p: [&Placement; 2],
    bars: &[u64],
    rate: u32,
) -> Option<f64> {
    let pulses: Vec<Vec<f64>> = (0..2)
        .map(|i| {
            s[i].beats
                .events
                .iter()
                .filter_map(|b| output_at(&p[i].time_map, b.source_frame as f64))
                .filter(|f| *f >= bars[0] as f64 - 1. && *f <= *bars.last().unwrap() as f64 + 1.)
                .collect()
        })
        .collect();
    if pulses[0].len() < 2 || pulses[0].len() != pulses[1].len() {
        return None;
    }
    Some(
        pulses[0]
            .iter()
            .zip(&pulses[1])
            .map(|(a, b)| (a - b).abs() * 1000. / f64::from(rate))
            .fold(0., f64::max),
    )
}

fn chroma_distance(a: &[f64; 12], b: &[f64; 12]) -> f64 {
    (a.iter().zip(b).map(|(a, b)| (a - b).abs()).sum::<f64>() / 2.).clamp(0., 1.)
}

fn predicted_dry_peak(s: &[&Snapshot; 2], p: [&Placement; 2], bars: &[u64]) -> Option<f64> {
    let mut peak = 0.;
    for i in 0..2 {
        let features = window_features(s[i], p[i], bars);
        if features.is_empty() {
            return None;
        }
        let source_peak = features.iter().map(|f| f.peak).fold(0., f64::max);
        let points: Vec<_> = bars
            .iter()
            .copied()
            .chain(
                p[i].automation
                    .iter()
                    .map(|a| a.output_frame)
                    .filter(|f| *f >= bars[0] && *f <= *bars.last().unwrap()),
            )
            .map(|frame| automation(&p[i].automation, frame))
            .collect();
        let gain = points.iter().map(|(g, _)| *g).fold(0., f64::max);
        let band = points
            .iter()
            .flat_map(|(_, b)| b)
            .copied()
            .fold(0., f64::max);
        peak += source_peak * gain * band;
    }
    // Dry-sum engineering bound, with 3 dB interpolation reserve. New effects
    // still require their own metered render check and headroom allocation.
    Some(20. * peak.max(1e-12).log10() + 3.)
}

fn add_loop_candidates(
    store: &Store,
    s: &Snapshot,
    p: &Placement,
    facts: &mut PassageFacts,
    cancel: &Cancellation,
) -> Result<()> {
    let Some(vocals) = facts.vocal_phrases.as_ref() else {
        return Ok(());
    };
    let mut wav = None;
    for bar in facts.bars.iter().take(4) {
        cancel.check()?;
        if !bar.stability.is_some_and(|v| v >= 0.65)
            || vocals
                .iter()
                .any(|v| v.start_frame < bar.end_frame && v.tail_end_frame > bar.start_frame)
        {
            continue;
        }
        let source_start = p.time_map.source_at(bar.start_frame as f64).round() as u64;
        let source_end = p.time_map.source_at(bar.end_frame as f64).round() as u64;
        if source_end <= source_start + 256 {
            continue;
        }
        if wav.is_none() {
            let mut reader = WavReader::open(&store.source(&s.track)?)?;
            require(
                reader.hashes(cancel)?.1 == p.audio_hash,
                "SOURCE_CHANGED",
                "loop seam source changed",
            )?;
            wav = Some(reader);
        }
        let reader = wav.as_mut().unwrap();
        let n = (u64::from(s.track.format.sample_rate_hz) / 100).clamp(32, 512) as usize;
        reader.seek_frame(source_start)?;
        let head = reader.read_frames(n)?;
        reader.seek_frame(source_end - n as u64)?;
        let tail = reader.read_frames(n)?;
        if head.len() != n || tail.len() != n {
            continue;
        }
        let seam_error = loop_seam_error(&head, &tail);
        let a = s.features.iter().find(|f| f.source_frame >= source_start);
        let b = s
            .features
            .iter()
            .rev()
            .find(|f| f.source_frame < source_end);
        if let Some((a, b)) = a.zip(b) {
            facts.loop_candidates.push(LoopEvidence {
                lane: StemLane::Stereo,
                source_start_frame: source_start,
                source_end_frame: source_end,
                output_start_frame: bar.start_frame,
                output_end_frame: bar.end_frame,
                seam_error,
                tonal_cycle_distance: chroma_distance(&a.chroma, &b.chroma),
                vocal_free: true,
                contains_complete_language: None,
                confidence_margin: bar.grid_score.unwrap_or(0.) * 0.5,
            });
        }
    }
    Ok(())
}

fn loop_seam_error(head: &[[f32; 2]], tail: &[[f32; 2]]) -> f64 {
    if head.len() < 2 || tail.len() < 2 {
        return f64::INFINITY;
    }
    let n = tail.len();
    let discontinuity = (0..2)
        .map(|c| {
            let wrap = head[0][c] - tail[n - 1][c];
            let expected = (tail[n - 1][c] - tail[n - 2][c] + head[1][c] - head[0][c]) * 0.5;
            f64::from((wrap - expected).abs())
        })
        .fold(0., f64::max);
    let peak = head
        .iter()
        .chain(tail)
        .flat_map(|s| s.iter())
        .map(|x| f64::from(x.abs()))
        .fold(0., f64::max);
    discontinuity / peak.max(1e-6)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn supported_four_bar_phase_survives_missing_local_novelty_boundaries() {
        let levels: Vec<f64> = (0..24)
            .map(|i| if (i + 1) / 4 % 2 == 0 { 0.2 } else { 0.8 })
            .collect();
        let events: Vec<_> = (0..=96)
            .map(|i| Beat {
                event_id: format!("beat{i}"),
                source_frame: i * 100,
                score: 1.,
                local_bpm: Some(120.),
            })
            .collect();
        let beats = Beats {
            header: Header::default(),
            beats_id: "fixture".into(),
            source_sample_rate_hz: 200,
            source_frames: 9601,
            downbeat_ids: events
                .iter()
                .step_by(4)
                .map(|e| e.event_id.clone())
                .collect(),
            events,
            meter_segments: vec![],
            unreliable_intervals: vec![],
            score_semantics: "fixture".into(),
        };
        let features: Vec<_> = levels
            .iter()
            .enumerate()
            .map(|(i, &x)| FeatureFrame {
                source_frame: i as u64 * 400,
                chroma: [1.; 12],
                band_energy: [x * x; 3],
                rms: x,
                peak: x,
                onset: x,
            })
            .collect();
        let phase = crate::phrase::infer_phrase_positions(&beats, &features);
        let map = TimeMap {
            header: Header::default(),
            map_id: "fixture".into(),
            source_sample_rate_hz: 200,
            output_sample_rate_hz: 200,
            interpolation: "monotone_cubic".into(),
            anchors: vec![
                Anchor {
                    source_frame: 0,
                    output_frame: 100,
                },
                Anchor {
                    source_frame: 9600,
                    output_frame: 9700,
                },
            ],
            speed_ratio_limits: [0.9, 1.1],
            max_speed_change_per_second: 0.02,
            preserve_pitch: true,
        };
        let (mut boundaries, mut evidence) = (vec![], vec![]);
        add_phase_boundaries(&phase, &map, &[1300, 4500], &mut boundaries, &mut evidence);
        assert_eq!(
            boundaries
                .iter()
                .map(|b| b.output_frame)
                .collect::<Vec<_>>(),
            vec![1300, 2900, 4500]
        );
        assert!(boundaries
            .iter()
            .all(|b| b.supported_period_bars == Some(4) && b.complete && b.score_margin >= 0.2));
        assert!(evidence
            .iter()
            .all(|v| v.contains("not an eight/sixteen-bar phrase or lyric-completion claim")));
        // Existing stricter long-period evidence is retained, never overwritten.
        boundaries[0].supported_period_bars = Some(16);
        add_phase_boundaries(&phase, &map, &[1300, 4500], &mut boundaries, &mut evidence);
        assert_eq!(boundaries.len(), 3);
        assert_eq!(boundaries[0].supported_period_bars, Some(16));
        let steady: Vec<_> = features
            .iter()
            .map(|f| FeatureFrame {
                source_frame: f.source_frame,
                chroma: [1.; 12],
                band_energy: [0.04; 3],
                rms: 0.2,
                peak: 0.2,
                onset: 0.2,
            })
            .collect();
        let ambiguous = crate::phrase::infer_phrase_positions(&beats, &steady);
        let mut absent = vec![];
        add_phase_boundaries(&ambiguous, &map, &[1300, 4500], &mut absent, &mut vec![]);
        assert!(absent.is_empty());
    }

    #[test]
    fn inverse_uses_the_cubic_map_inside_each_interval() {
        let map = TimeMap {
            header: Header::default(),
            map_id: id(),
            source_sample_rate_hz: 1000,
            output_sample_rate_hz: 1000,
            interpolation: "monotone_cubic".into(),
            anchors: vec![
                Anchor {
                    source_frame: 0,
                    output_frame: 0,
                },
                Anchor {
                    source_frame: 10000,
                    output_frame: 10400,
                },
                Anchor {
                    source_frame: 20000,
                    output_frame: 20000,
                },
            ],
            speed_ratio_limits: [0.8, 1.2],
            max_speed_change_per_second: 0.1,
            preserve_pitch: true,
        };
        map.validate().unwrap();
        let source = map.source_at(5200.);
        assert!((source - 5000.).abs() > 10.);
        assert!((output_at(&map, source).unwrap() - 5200.).abs() < 1e-7);
        assert!(output_at(&map, -1.).is_none());
        assert!(output_at(&map, 20001.).is_none());
    }

    #[test]
    fn vocal_absence_requires_positive_provider_and_acoustic_evidence() {
        let prepared = json!({"vocal_phrase_evidence":{"method":"provider/1","reference":"fixture:analysis",
            "incoming":{"instrumental":true}}});
        let quiet = json!({"acoustic_vocals":{"active_spans":[]}});
        let active = json!({"acoustic_vocals":{"active_spans":[[0.2,1.0]]}});
        assert!(
            parse_vocal_phrases(None, Some(&quiet), "incoming", 0, 48000, &mut vec![]).is_none()
        );
        assert!(parse_vocal_phrases(
            Some(&prepared),
            Some(&quiet),
            "incoming",
            0,
            48000,
            &mut vec![]
        )
        .unwrap()
        .is_empty());
        assert!(parse_vocal_phrases(
            Some(&prepared),
            Some(&active),
            "incoming",
            0,
            48000,
            &mut vec![]
        )
        .is_none());
    }

    #[test]
    fn vocal_tail_and_unrecognized_activity_are_preserved() {
        let prepared = json!({"vocal_phrase_evidence":{"method":"provider/1","reference":"fixture:analysis",
            "outgoing":{"phrases":[{"start_seconds":0.1,"end_seconds":1.,"confidence_margin":0.3,
                "entry_supported":true,"exit_supported":true}]}}});
        let tail = json!({"acoustic_vocals":{"active_spans":[[0.1,1.2]]}});
        let result = parse_vocal_phrases(
            Some(&prepared),
            Some(&tail),
            "outgoing",
            96000,
            48000,
            &mut vec![],
        )
        .unwrap();
        assert_eq!(result[0].start_frame, 100800);
        assert_eq!(result[0].end_frame, 144000);
        assert_eq!(result[0].tail_end_frame, 153600);
        let unknown = json!({"acoustic_vocals":{"active_spans":[[0.1,1.2],[1.8,2.1]]}});
        assert!(parse_vocal_phrases(
            Some(&prepared),
            Some(&unknown),
            "outgoing",
            96000,
            48000,
            &mut vec![]
        )
        .is_none());
    }

    #[test]
    fn density_requires_selected_layer_measurements_with_matching_timebase() {
        let raw = json!({"clips":[{"bars":[{"source_rms":0.9}]}]});
        assert!(
            density_values(Some(&raw), "selected_composite", &[96000, 192000], 48000).is_none()
        );
        let prepared = json!({"selected_composite":{"method":"relative_level_crest_active_lanes/1",
            "bars":[{"start_seconds":0.,"end_seconds":2.,"density_score":0.75}]}});
        assert_eq!(
            density_values(
                Some(&prepared),
                "selected_composite",
                &[96000, 192000],
                48000
            ),
            Some(vec![0.75])
        );
        assert!(density_values(
            Some(&prepared),
            "selected_composite",
            &[96000, 240000],
            48000
        )
        .is_none());
    }

    #[test]
    fn loop_seam_distinguishes_normal_wave_slope_from_an_actual_jump() {
        let wave: Vec<_> = (0..4800)
            .map(|i| [0.1 * (std::f32::consts::TAU * 440. * i as f32 / 48000.).sin(); 2])
            .collect();
        let good = loop_seam_error(&wave[..480], &wave[4320..]);
        assert!(good < 0.002, "periodic seam was rejected: {good}");
        let head = vec![[0.1, 0.1]; 64];
        let tail = vec![[-0.1, -0.1]; 64];
        assert!(loop_seam_error(&head, &tail) > 1.9);
    }
}
