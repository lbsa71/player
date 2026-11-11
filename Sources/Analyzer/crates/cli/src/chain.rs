use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn stitch_chain(
    store: &Store,
    tracks: &[&str],
    policy: &MixingConfig,
    context_beats: usize,
    separator: Option<&Path>,
    default_stretch: bool,
    band_joins: &[usize],
    cancel: &Cancellation,
) -> Result<MixPlan> {
    require(
        (3..=8).contains(&tracks.len())
            && tracks
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == tracks.len(),
        "CHAIN_TRACKS",
        "choose three to eight distinct tracks",
    )?;
    require(
        band_joins
            .iter()
            .all(|join| *join > 0 && *join < tracks.len()),
        "CHAIN_BAND_JOIN",
        "band join numbers run from 1 through the number of joins",
    )?;
    if let Some(python) = separator {
        let ids = tracks.iter().map(|id| (*id).to_owned()).collect::<Vec<_>>();
        let report = tactic_analysis::ensure_tactic_analysis(store, &ids, python, cancel)?;
        require(
            report.counts.get("failed").copied().unwrap_or(0) == 0,
            "TACTIC_ANALYSIS",
            "tactic analysis failed; inspect the saved report",
        )?;
    }
    let mut chain: Option<MixPlan> = None;
    let mut joins = vec![];
    let mut chain_tactics = Vec::new();
    for (index, names) in tracks.windows(2).enumerate() {
        cancel.check()?;
        let outgoing_after_frame = if let Some(p) = chain.as_ref() {
            let middle = p.placements.last().unwrap();
            let entry_end = middle
                .time_map
                .source_at(p.transitions.last().unwrap().end_output_frame as f64)
                .round() as u64;
            mixlib_planner::read_track_pacing(store, &middle.track_id, cancel)?
                .earliest_exit(entry_end)
        } else {
            0
        };
        let request = mixlib_planner::StitchRequest {
            from: names[0],
            to: names[1],
            overlap_beats: 8,
            context_beats,
            outgoing_after_frame,
        };
        let choice = if band_joins.contains(&(index + 1)) {
            tactics::PairSelection {
                selected: None,
                missing_analysis: false,
                examined: vec![],
            }
        } else {
            tactics::choose_pair(
                store,
                &request,
                policy,
                tracks.get(index + 2).copied(),
                cancel,
            )?
        };
        require(!choice.missing_analysis,"TACTIC_ANALYSIS_REQUIRED",
            "automatic chain selection requires full tactic analysis; run analyze-tactics or configure MIXLIB_SEPARATOR_PYTHON")?;
        let linger = if band_joins.contains(&(index + 1)) || choice.selected.is_some() {
            None
        } else if let Some(python) = separator {
            try_quiet_linger(
                store,
                &request,
                tracks.get(index + 2).copied(),
                policy,
                python,
                cancel,
            )?
        } else {
            None
        };
        let mut pair = if band_joins.contains(&(index + 1)) {
            mixlib_planner::band_stitch(store, &request, policy, cancel)?
        } else if let Some(plan) = choice.selected {
            plan
        } else if let Some(plan) = linger {
            plan
        } else if let Some(mut band_plan) =
            mixlib_planner::preferred_band_stitch(store, &request, policy, cancel)?
        {
            band_plan.diagnostics.push(
                "automatic selection: aligned band exchange of at least 12 beats within the configured stretch limit"
                    .into(),
            );
            band_plan
        } else if let Some(python) = separator {
            automatic_stitch(store, &request, policy, default_stretch, python, cancel)?
        } else {
            mixlib_planner::stitch(store, &request, policy, cancel)?
        };
        let pair_tactics = read_tactics(&pair)?;
        pair.header.extensions.remove(TACTICS_EXTENSION);
        let mut patches = vec![];
        if let Some(intro) = pair.drum_intro.take() {
            for (_, placement, audio, source_offset) in intro.assets() {
                let frames = mixlib_audio::WavReader::open(&store.path(&audio.path)?)?
                    .format
                    .frames;
                patches.push(PreparedPatch {
                    placement,
                    audio: audio.clone(),
                    source_offset,
                    source_end: source_offset + frames,
                });
            }
        }
        let local_start = pair.transitions[0].start_output_frame;
        let objective = pair.objective.clone();
        let vocal_handoff = pair.header.extensions.get("vocal_handoff").cloned();
        let mashup = pair.header.extensions.get("mashup").cloned();
        let phrase_entry = pair.header.extensions.get("phrase_entry").cloned();
        let stem_evidence = pair.header.extensions.get("stem_evidence").cloned();
        let prepared_tactic_id = pair.header.extensions.get("prepared_tactic_id").cloned();
        let outgoing_cue = pair.placements[0].time_map.anchors[1].source_frame;
        let bar_bpm = pair.header.extensions.get("bar_bpm").cloned();
        let incoming_cue = pair.placements[1].source_start_frame;
        let offset = if let Some(plan) = &mut chain {
            let index = plan.placements.len() - 1;
            let offset = append_pair(plan, pair)?;
            for patch in &mut patches {
                patch.placement += index;
            }
            plan.prepared_patches.extend(patches);
            offset
        } else {
            pair.prepared_patches = patches;
            pair.plan_id = id();
            chain = Some(pair);
            0
        };
        for tactic in pair_tactics {
            chain_tactics.push(tactic.translated(offset, index)?);
        }
        joins.push(serde_json::json!({
            "from": names[0], "to": names[1], "choice": objective,
            "outgoing_cue_frame": outgoing_cue, "incoming_cue_frame": incoming_cue,
            "start_output_frame": local_start + offset, "vocal_handoff_local": vocal_handoff,
            "pair_output_offset": offset, "bar_bpm": bar_bpm, "mashup_local": mashup, "phrase_entry": phrase_entry,
            "stem_evidence": stem_evidence, "prepared_tactic_id": prepared_tactic_id,
            "tactic_search": choice.examined,
        }));
    }
    finish_chain(
        store,
        chain.expect("validated chain has joins"),
        joins,
        chain_tactics,
        cancel,
    )
}

fn finish_chain(
    store: &Store,
    mut plan: MixPlan,
    joins: Vec<serde_json::Value>,
    chain_tactics: Vec<TacticPlan>,
    cancel: &Cancellation,
) -> Result<MixPlan> {
    plan.objective = "stitch_chain".into();
    plan.planner_version = "beat-stitch-chain/1".into();
    plan.renderer_version = if plan.prepared_patches.is_empty() {
        mixlib_audio::RENDERER
    } else {
        mixlib_audio::CHAIN_RENDERER
    }
    .into();
    plan.header.extensions.remove("vocal_handoff");
    plan.header.extensions.remove("mashup");
    plan.header.extensions.remove("phrase_entry");
    plan.header.extensions.remove("stem_evidence");
    plan.header.extensions.remove("prepared_tactic_id");
    plan.header
        .extensions
        .insert("chain_joins".into(), serde_json::json!(joins));
    let intro_shift = apply_bookends(store, &mut plan, cancel)?;
    if !chain_tactics.is_empty() {
        let translated = chain_tactics
            .iter()
            .map(|t| t.translated(intro_shift, 0))
            .collect::<Result<Vec<_>>>()?;
        write_tactics(&mut plan, translated)?;
        plan.renderer_version = mixlib_audio::TACTIC_RENDERER.into();
    }
    plan.policy_hash = canonical_hash(&plan.policy)?;
    plan.validate()?;
    Ok(plan)
}

/// Extend a measured partial route without reopening cue or tactic selection.
/// Validate the joined time map and translated schedules before it enters the beam.
pub(super) fn extend_measured(
    prefix: Option<&MixPlan>,
    pair: &MixPlan,
    score: f64,
    last_resort: bool,
) -> Result<MixPlan> {
    let mut pair_tactics = read_tactics(pair)?;
    require(
        pair_tactics.len() == 1,
        "TACTIC_PLAN",
        "each beam join needs a measured tactic",
    )?;
    let transition = &pair.transitions[0];
    let start = transition.start_output_frame;
    let mut join = serde_json::json!({
        "from": transition.from_track_id,
        "to": transition.to_track_id,
        "outgoing_cue_frame": pair.placements[0].time_map.source_at(start as f64).round() as u64,
        "incoming_cue_frame": pair.placements[1].source_start_frame,
        "choice": pair_tactics[0].tactic_id,
        "score": score,
        "last_resort": last_resort,
        "dense_quality_seconds": pair.header.extensions.get("dense_quality_seconds").and_then(serde_json::Value::as_f64).unwrap_or(0.),
        "handoff_quality_seconds": pair.header.extensions.get("handoff_quality_seconds").and_then(serde_json::Value::as_f64).unwrap_or(0.),
    });
    let mut plan = prefix.unwrap_or(pair).clone();
    let mut joins = plan
        .header
        .extensions
        .remove("chain_joins")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    let offset = if prefix.is_some() {
        let mut existing = read_tactics(&plan)?;
        let index = plan.placements.len() - 1;
        let offset = append_pair(&mut plan, pair.clone())?;
        for tactic in pair_tactics {
            existing.push(tactic.translated(offset, index)?);
        }
        pair_tactics = existing;
        offset
    } else {
        0
    };
    join["start_output_frame"] = serde_json::json!(start + offset);
    join["pair_output_offset"] = serde_json::json!(offset);
    joins.push(join);
    plan.header
        .extensions
        .insert("chain_joins".into(), serde_json::json!(joins));
    plan.policy_hash = canonical_hash(&plan.policy)?;
    write_tactics(&mut plan, pair_tactics)?;
    plan.validate()?;
    Ok(plan)
}

pub(super) fn finish_measured(
    store: &Store,
    mut plan: MixPlan,
    cancel: &Cancellation,
) -> Result<MixPlan> {
    let tactics = read_tactics(&plan)?;
    let joins = plan
        .header
        .extensions
        .remove("chain_joins")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    plan.plan_id = id();
    finish_chain(store, plan, joins, tactics, cancel)
}

fn apply_bookends(store: &Store, plan: &mut MixPlan, cancel: &Cancellation) -> Result<u64> {
    let cues = mixlib_planner::choose_chain_bookends(
        store,
        &plan.placements[0],
        plan.placements.last().unwrap(),
        cancel,
    )?;
    let first = &mut plan.placements[0];
    let rate = u64::from(plan.output.sample_rate_hz);
    let intro_frames = ((first.source_start_frame - cues.start) as f64 * rate as f64
        / f64::from(first.time_map.source_sample_rate_hz)
        / first.time_map.slopes()[0])
        .round() as u64;
    if intro_frames > 0 {
        let (gain, bands) = mixlib_audio::automation(&first.automation, rate / 25);
        first.source_start_frame = cues.start;
        first.time_map.anchors[0].source_frame = cues.start;
        for anchor in first.time_map.anchors.iter_mut().skip(1) {
            anchor.output_frame += intro_frames;
        }
        let fade = (rate / 50).min(intro_frames);
        first
            .automation
            .retain(|point| point.output_frame > rate / 25);
        for point in &mut first.automation {
            point.output_frame += intro_frames;
        }
        first.automation.splice(
            0..0,
            [
                AutomationPoint {
                    output_frame: 0,
                    gain_linear: 0.,
                    bands_linear: bands,
                    mute: true,
                },
                AutomationPoint {
                    output_frame: fade,
                    gain_linear: gain,
                    bands_linear: bands,
                    mute: false,
                },
            ],
        );
        for placement in plan.placements.iter_mut().skip(1) {
            for anchor in &mut placement.time_map.anchors {
                anchor.output_frame += intro_frames;
            }
            for point in &mut placement.automation {
                point.output_frame += intro_frames;
            }
        }
        for transition in &mut plan.transitions {
            transition.start_output_frame += intro_frames;
            transition.end_output_frame += intro_frames;
        }
        let joins = plan
            .header
            .extensions
            .get_mut("chain_joins")
            .and_then(serde_json::Value::as_array_mut)
            .expect("chain has join metadata");
        for join in joins {
            for key in ["start_output_frame", "pair_output_offset"] {
                join[key] = serde_json::json!(join[key].as_u64().unwrap() + intro_frames);
            }
        }
        plan.total_output_frames += intro_frames;
    }
    let last = plan.placements.last_mut().unwrap();
    let map = &mut last.time_map;
    let solo = map.anchors[map.anchors.len() - 2].clone();
    let tail_speed = *map.slopes().last().unwrap();
    let output_per_source = rate as f64 / f64::from(map.source_sample_rate_hz) / tail_speed;
    let end = solo.output_frame
        + ((cues.end - solo.source_frame) as f64 * output_per_source).round() as u64;
    let fade = ((cues.outro_fade_frames as f64 * output_per_source).round() as u64)
        .min((end - solo.output_frame) / 2);
    let (gain, bands) = mixlib_audio::automation(&last.automation, solo.output_frame);
    last.source_end_frame = cues.end;
    *map.anchors.last_mut().unwrap() = Anchor {
        source_frame: cues.end,
        output_frame: end,
    };
    last.automation
        .retain(|point| point.output_frame <= solo.output_frame);
    last.automation.extend([
        AutomationPoint {
            output_frame: end - fade,
            gain_linear: gain,
            bands_linear: bands,
            mute: false,
        },
        AutomationPoint {
            output_frame: end,
            gain_linear: 0.,
            bands_linear: bands,
            mute: true,
        },
    ]);
    plan.total_output_frames = end;
    plan.header.extensions.insert(
        "bookends".into(),
        serde_json::json!({
            "intro_source_frame": cues.start, "outro_source_frame": cues.end,
            "outro_fade_start_output_frame": end - fade, "ending": cues.ending,
        }),
    );
    Ok(intro_frames)
}

pub(super) fn pick_chain(
    store: &Store,
    policy: &MixingConfig,
    context_beats: usize,
    track_count: usize,
    separator: Option<&Path>,
    cancel: &Cancellation,
) -> Result<MixPlan> {
    let pool = measured_beam::prepared_pool(&store.read_snapshot())?;
    let result = measured_beam::choose(
        store,
        policy,
        context_beats,
        track_count,
        pool.clone(),
        cancel,
    );
    let (Some(python), Err(error)) = (separator, &result) else {
        return result;
    };
    if !matches!(
        error.code.as_str(),
        "CHAIN_POOL" | "TACTIC_NO_COMPLETE_ROUTE"
    ) {
        return result;
    }
    let shortlist = mixlib_planner::pick_tracks(store, policy, context_beats, track_count, cancel)?;
    let mut seen: std::collections::BTreeSet<_> =
        pool.iter().map(|track| track.track_id.clone()).collect();
    let ids: Vec<_> = shortlist
        .orders
        .into_iter()
        .flatten()
        .filter(|id| seen.insert(id.clone()))
        .take(track_count.saturating_sub(pool.len()).max(4))
        .collect();
    if ids.is_empty() {
        return result;
    }
    eprintln!(
        "stitch: preparing {} additional recordings from the automatic shortlist",
        ids.len()
    );
    tactic_analysis::ensure_tactic_analysis(store, &ids, python, cancel)?;
    let expanded = measured_beam::prepared_pool(&store.read_snapshot())?;
    if expanded.len() == pool.len() {
        return result;
    }
    measured_beam::choose(store, policy, context_beats, track_count, expanded, cancel)
}

pub(super) fn try_quiet_linger(
    store: &Store,
    request: &mixlib_planner::StitchRequest<'_>,
    next_track: Option<&str>,
    policy: &MixingConfig,
    python: &Path,
    cancel: &Cancellation,
) -> Result<Option<MixPlan>> {
    let min_tail = next_track.map_or(0, |_| 16);
    let candidates =
        mixlib_planner::quiet_linger_candidates(store, request, policy, 3, min_tail, cancel)?;
    if candidates.is_empty() {
        return Ok(None);
    }
    let from = store.source(&store.track(request.from)?)?;
    let to = store.source(&store.track(request.to)?)?;
    let mut rejected = 0;
    let mut gradual_fallback = None;
    for (mut plan, handoff) in candidates {
        let requested_mashup = plan.objective == "drum_mashup";
        let bars = handoff.vocal_bars.expect("quiet linger has a vocal window");
        if let Some(next_track) = next_track {
            let map = &plan.placements[1].time_map;
            let entry_end = map
                .source_at(*bars.last().expect("vocal window has an end") as f64)
                .round() as u64;
            let after = mixlib_planner::read_track_pacing(store, request.to, cancel)?
                .earliest_exit(entry_end);
            let next = mixlib_planner::StitchRequest {
                from: request.to,
                to: next_track,
                overlap_beats: 8,
                context_beats: request.context_beats,
                outgoing_after_frame: after,
            };
            match mixlib_planner::stitch(store, &next, policy, cancel) {
                Ok(_) => (),
                Err(error) if error.code == "STITCH_INFEASIBLE" => continue,
                Err(error) => return Err(error),
            }
        }
        let intro =
            match prepare_vocal_handoff(store, &mut plan, [&from, &to], &bars, python, cancel) {
                Ok(intro) => intro,
                Err(error) if error.code == "STITCH_VOCAL_GAP" => {
                    rejected += 1;
                    continue;
                }
                Err(error) => return Err(error),
            };
        plan.drum_intro = Some(intro);
        plan.renderer_version = mixlib_audio::DRUM_SWAP_RENDERER.into();
        let tactic = if plan.objective == "drum_mashup" {
            let hold = plan.header.extensions["mashup"]["hold_bars"]
                .as_u64()
                .unwrap();
            let exit = plan.header.extensions["mashup"]["exit_bars"]
                .as_u64()
                .unwrap_or(1);
            format!("{hold}-bar mashup with a {exit}-bar layer exchange")
        } else {
            "two-bar drum lead with a gradual handoff".into()
        };
        plan.diagnostics.push(format!(
            "automatic selection: {tactic}; {rejected} vocal gaps rejected"
        ));
        if requested_mashup && plan.objective != "drum_mashup" {
            gradual_fallback.get_or_insert(plan);
            continue;
        }
        return Ok(Some(plan));
    }
    Ok(gradual_fallback)
}

fn append_pair(chain: &mut MixPlan, mut pair: MixPlan) -> Result<u64> {
    let transition = &mut pair.transitions[0];
    let local_start = transition.start_output_frame;
    let departing = &pair.placements[0];
    let middle = chain.placements.last_mut().unwrap();
    let map = &mut middle.time_map;
    let incoming_speed = *map.slopes().last().unwrap();
    let outgoing_speed = departing.time_map.slopes()[0];
    map.anchors.pop();
    let previous = map.anchors.last().unwrap().clone();
    let next_source = departing.time_map.anchors[1].source_frame;
    let gap = next_source
        .checked_sub(previous.source_frame)
        .ok_or_else(|| Error::new("CHAIN_CUES", "middle track's exit is before its entry"))?;
    require(
        gap > 0,
        "CHAIN_CUES",
        "middle track needs time between joins",
    )?;

    // Three long intervals keep the two overlap speeds at the ends and make
    // room for the existing monotone time map to change speed in between.
    let scale = f64::from(map.output_sample_rate_hz) / f64::from(map.source_sample_rate_hz);
    let mut output = previous.output_frame;
    let mut source = previous.source_frame;
    for (index, speed) in [
        incoming_speed,
        (incoming_speed + outgoing_speed) / 2.,
        outgoing_speed,
    ]
    .into_iter()
    .enumerate()
    {
        let end = previous.source_frame + gap * (index as u64 + 1) / 3;
        output += ((end - source) as f64 * scale / speed).round() as u64;
        source = end;
        map.anchors.push(Anchor {
            source_frame: source,
            output_frame: output,
        });
    }
    let offset = output
        .checked_sub(local_start)
        .ok_or_else(|| Error::new("CHAIN_CUES", "invalid pair output offset"))?;
    map.anchors
        .extend(departing.time_map.anchors.iter().skip(2).map(|a| Anchor {
            source_frame: a.source_frame,
            output_frame: a.output_frame + offset,
        }));
    map.speed_ratio_limits[0] =
        map.speed_ratio_limits[0].min(departing.time_map.speed_ratio_limits[0]);
    map.speed_ratio_limits[1] =
        map.speed_ratio_limits[1].max(departing.time_map.speed_ratio_limits[1]);
    map.validate()?;
    middle.source_end_frame = departing.source_end_frame;
    middle
        .automation
        .retain(|point| point.output_frame <= previous.output_frame);
    let (gain_linear, bands_linear) = mixlib_audio::automation(&departing.automation, local_start);
    middle.automation.push(AutomationPoint {
        output_frame: output,
        gain_linear,
        bands_linear,
        mute: gain_linear == 0.,
    });
    middle.automation.extend(
        departing
            .automation
            .iter()
            .filter(|p| p.output_frame > local_start)
            .cloned()
            .map(|mut p| {
                p.output_frame += offset;
                p
            }),
    );

    transition.start_output_frame += offset;
    transition.end_output_frame += offset;
    let mut incoming = pair.placements.pop().unwrap();
    for anchor in &mut incoming.time_map.anchors {
        anchor.output_frame += offset;
    }
    for point in &mut incoming.automation {
        point.output_frame += offset;
    }
    chain.total_output_frames = pair.total_output_frames + offset;
    chain.policy.max_speed_deviation = chain
        .policy
        .max_speed_deviation
        .max(pair.policy.max_speed_deviation);
    chain.transitions.append(&mut pair.transitions);
    chain.placements.push(incoming);
    chain.diagnostics.append(&mut pair.diagnostics);
    Ok(offset)
}
