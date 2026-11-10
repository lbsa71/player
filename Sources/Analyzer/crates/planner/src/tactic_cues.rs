//! Native-clock timing alternatives for structural and independent-pulse joins.
//! These skeletons are never admission: the tactic selector owns music safety.
use super::*;
use crate::stitch::StitchRequest;

/// A search prior, never a vocal admission decision. Prefer windows for which
/// the provider has positive local evidence before spending the bounded cue
/// budget on unknown language. The adapter still checks the actual vocal PCM.
pub(crate) fn vocal_window_supported(
    record: Option<&TacticTrackAnalysis>,
    start: u64,
    end: u64,
) -> bool {
    let Some(record) = record else { return false };
    let Some(track) = record.vocal_evidence.get("track") else {
        return false;
    };
    let mut intervals = Vec::new();
    for key in ["verified_absence_spans", "phrases"] {
        for row in track
            .get(key)
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let supported = if key == "phrases" {
                row.get("entry_supported")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                    && row
                        .get("exit_supported")
                        .and_then(serde_json::Value::as_bool)
                        == Some(true)
            } else {
                row.get("verification_method")
                    .and_then(serde_json::Value::as_str)
                    == Some("separated_vocal_rms_and_peak_negligible/1")
            };
            if !supported {
                continue;
            }
            if let Some((a, b)) = row
                .get("start_seconds")
                .and_then(serde_json::Value::as_f64)
                .zip(row.get("end_seconds").and_then(serde_json::Value::as_f64))
            {
                if a.is_finite() && b.is_finite() && a >= 0. && b > a {
                    intervals.push((
                        (a * record.source_sample_rate_hz as f64).round() as u64,
                        (b * record.source_sample_rate_hz as f64).round() as u64,
                    ));
                }
            }
        }
    }
    intervals.sort_unstable();
    let mut covered = start;
    for (a, b) in intervals {
        if b <= covered {
            continue;
        }
        if a > covered {
            return false;
        }
        covered = b;
        if covered >= end {
            return true;
        }
    }
    false
}

pub(crate) fn native_pair(
    tracks: [&Analyzed; 2],
    windows: [(u64, u64); 2],
    context: usize,
    policy: &MixingConfig,
    basis: &str,
) -> Result<Option<MixPlan>> {
    let [a, b] = tracks;
    let [(a0, a1), (b0, b1)] = windows;
    if a0 >= a1 || b0 >= b1 || a1 > a.track.format.frames || b1 > b.track.format.frames {
        return Ok(None);
    }
    let seconds = [
        (a1 - a0) as f64 / a.track.format.sample_rate_hz as f64,
        (b1 - b0) as f64 / b.track.format.sample_rate_hz as f64,
    ];
    let duration = (seconds[0] + seconds[1]) * 0.5;
    if duration > 120.
        || seconds
            .iter()
            .any(|s| (s / duration - 1.).abs() > policy.max_speed_deviation)
    {
        return Ok(None);
    }
    let source_context = |t: &Analyzed| {
        ((context as f64 * 0.5).clamp(2., 16.) * t.track.format.sample_rate_hz as f64).round()
            as u64
    };
    let a_before = a0.saturating_sub(source_context(a));
    let b_after = b1
        .saturating_add(source_context(b))
        .min(b.track.format.frames);
    if a_before == a0 || b_after == b1 {
        return Ok(None);
    }
    let rate = policy.output_sample_rate_hz;
    let overlap = (duration * rate as f64).round() as u64;
    let start = ((a0 - a_before) as f64 * overlap as f64 / (a1 - a0) as f64).round() as u64;
    let end = start + overlap;
    let total = end + ((b_after - b1) as f64 * overlap as f64 / (b1 - b0) as f64).round() as u64;
    let make = |t: &Analyzed, points: [(u64, u64); 3]| TimeMap {
        header: Header::default(),
        map_id: id(),
        source_sample_rate_hz: t.track.format.sample_rate_hz,
        output_sample_rate_hz: rate,
        interpolation: "monotone_cubic".into(),
        anchors: points
            .into_iter()
            .map(|(source_frame, output_frame)| Anchor {
                source_frame,
                output_frame,
            })
            .collect(),
        speed_ratio_limits: [
            1. - policy.max_speed_deviation,
            1. + policy.max_speed_deviation,
        ],
        max_speed_change_per_second: policy.max_speed_change_per_second,
        preserve_pitch: true,
    };
    let maps = [
        make(a, [(a_before, 0), (a0, start), (a1, end)]),
        make(b, [(b0, start), (b1, end), (b_after, total)]),
    ];
    if maps.iter().any(|m| m.validate().is_err()) {
        return Ok(None);
    }
    let [ma, mb] = maps;
    let mut placements = vec![placement(a, ma), placement(b, mb)];
    for (i, p) in placements.iter_mut().enumerate() {
        let g = gain(tracks[i]);
        let points = if i == 0 {
            vec![(0, 0.), (u64::from(rate) / 100, g), (start, g), (end, 0.)]
        } else {
            vec![
                (start, 0.),
                (end, g),
                (total - u64::from(rate) / 100, g),
                (total, 0.),
            ]
        };
        p.automation = points
            .into_iter()
            .map(|(output_frame, gain_linear)| AutomationPoint {
                output_frame,
                gain_linear,
                bands_linear: [1.; 3],
                mute: gain_linear == 0.,
            })
            .collect();
    }
    let mut policy = policy.clone();
    policy.allow_fallback = false;
    policy.template = Template::ShortCrossfade;
    let mut header = Header::default();
    header.extensions.insert(
        "tactic_cue_candidate".into(),
        serde_json::json!({
            "timing_basis":basis,"outgoing_source_frame":a0,"incoming_source_frame":b0,
            "outgoing_end_frame":a1,"incoming_end_frame":b1,
            "source_speed_multipliers":[seconds[0]/duration,seconds[1]/duration],
        }),
    );
    let plan = MixPlan {
        header,
        plan_id: id(),
        planner_version: "joint-tactic-cues/1".into(),
        renderer_version: RENDERER.into(),
        policy_hash: canonical_hash(&policy)?,
        policy,
        seed: 42,
        objective: "tactic_timing_candidate".into(),
        requested_duration_seconds: None,
        complete: true,
        output: OutputFormat {
            sample_rate_hz: rate,
            channels: 2,
            encoding: "float32".into(),
            dither: "seeded_tpdf_1_lsb".into(),
        },
        drum_intro: None,
        prepared_patches: vec![],
        placements,
        transitions: vec![Transition {
            transition_id: id(),
            from_track_id: a.track.track_id.clone(),
            to_track_id: b.track.track_id.clone(),
            outgoing_region_id: format!("tactic-{a0}-{a1}"),
            incoming_region_id: format!("tactic-{b0}-{b1}"),
            start_output_frame: start,
            end_output_frame: end,
            template: Template::ShortCrossfade,
            costs: BTreeMap::new(),
            fallback_reason: None,
        }],
        total_output_frames: total,
        crossover_hz: [200., 2500.],
        crossover_design: "complementary_one_pole_v1".into(),
        automation_interpolation: "smoothstep_linear_amplitude".into(),
        diagnostics: vec![format!(
            "{basis} timing candidate; full measured tactic admission still required"
        )],
    };
    plan.validate()?;
    Ok(Some(plan))
}

pub(crate) fn structural_candidates(
    store: &Store,
    request: &StitchRequest<'_>,
    policy: &MixingConfig,
    limit: usize,
    cancel: &Cancellation,
) -> Result<Vec<MixPlan>> {
    let a = load_analyzed(store, store.track(request.from)?)?;
    let b = load_analyzed(store, store.track(request.to)?)?;
    let signals = |t: &Analyzed| -> Result<Option<TacticTrackAnalysis>> {
        t.analysis
            .artifacts
            .get("tactic_analysis")
            .map(|r| store.read_ref(r))
            .transpose()
    };
    let (Some(sa), Some(sb)) = (signals(&a)?, signals(&b)?) else {
        return Ok(vec![]);
    };
    let events = |s: &TacticTrackAnalysis, kind: &str| -> Vec<u64> {
        s.signal_evidence
            .get("events")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter(|e| e.get("kind").and_then(serde_json::Value::as_str) == Some(kind))
            .filter_map(|e| e.get("frame").and_then(serde_json::Value::as_u64))
            .collect()
    };
    let mut windows = Vec::new();
    // Start B at a measured beginning after A has naturally decayed. The saved
    // A tail supplies real silence; no padding or fictitious beat is inserted.
    for end in events(&sa, "natural_ending") {
        for attack in events(&sb, "audible_start") {
            let b0 = attack.saturating_sub(u64::from(b.track.format.sample_rate_hz) / 20);
            // An ambient bridge may carry A's decay over B's intro, with both
            // real structural endpoints preserved and no beat-grid invention.
            for seconds in [4., 8.] {
                let frames = (seconds * a.track.format.sample_rate_hz as f64).round() as u64;
                if end > frames {
                    windows.push((
                        (end - frames, end),
                        (
                            b0,
                            b0 + (seconds * b.track.format.sample_rate_hz as f64).round() as u64,
                        ),
                    ));
                }
            }
            let seconds = ((a.track.format.frames - end) as f64
                / a.track.format.sample_rate_hz as f64)
                .min(1.);
            if seconds < 0.04 {
                continue;
            }
            windows.push((
                (
                    end,
                    end + (seconds * a.track.format.sample_rate_hz as f64).round() as u64,
                ),
                (
                    b0,
                    b0 + (seconds * b.track.format.sample_rate_hz as f64).round() as u64,
                ),
            ));
        }
    }
    // Sparse structural windows may have no reliable pulse. Only save timing
    // alternatives here; complete phrases, absence of drums and tonality are
    // independently established by candidate_facts and the chosen tactic.
    let sparse = |s: &TacticTrackAnalysis| -> Vec<(u64, u64)> {
        ["segments", "unmetered_windows"]
            .into_iter()
            .flat_map(|key| {
                s.signal_evidence
                    .get(key)
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .filter(|v| {
                v.get("roles")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|roles| {
                        roles.iter().any(|r| {
                            matches!(
                                r.as_str(),
                                Some("low_percussion_bed" | "rest" | "sustained_texture")
                            )
                        })
                    })
            })
            .filter_map(|v| {
                Some((
                    v.get("start_frame")?.as_u64()?,
                    v.get("end_frame")?.as_u64()?,
                ))
            })
            .collect()
    };
    let spread = |values: Vec<(u64, u64)>| -> Vec<_> {
        let step = values.len().div_ceil(64).max(1);
        values.into_iter().step_by(step).collect()
    };
    let aa = spread(sparse(&sa));
    let bb = spread(sparse(&sb));
    for aw in &aa {
        for bw in &bb {
            let da = (aw.1 - aw.0) as f64 / a.track.format.sample_rate_hz as f64;
            let db = (bw.1 - bw.0) as f64 / b.track.format.sample_rate_hz as f64;
            if (da - db).abs() / da.max(db) < 0.04 {
                windows.push((*aw, *bw))
            }
        }
    }
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for (aw, bw) in windows {
        cancel.check()?;
        if aw.0 < request.outgoing_after_frame || !seen.insert((aw, bw)) {
            continue;
        }
        if let Some(plan) = native_pair(
            [&a, &b],
            [aw, bw],
            request.context_beats,
            policy,
            "structural",
        )? {
            result.push(plan)
        }
        if result.len() >= limit {
            break;
        }
    }
    Ok(result)
}
