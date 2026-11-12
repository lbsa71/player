//! Human-readable evidence for the exact saved schedule, without reselecting it.
use super::*;
use mixlib_planner::{CandidateEvidenceCache, PairEvidence, PairProfile};
use serde_json::{json, Value};

pub(super) fn sidecar(output: &Path) -> PathBuf {
    let mut name = output.file_stem().unwrap_or_default().to_os_string();
    name.push("-timetable.md");
    output.with_file_name(name)
}

pub(super) fn preflight(store: &Store, path: &Path, overwrite: bool) -> Result<()> {
    protect_output(store, path)?;
    require(
        overwrite || !path.exists(),
        "OUTPUT_EXISTS",
        path.display().to_string(),
    )
}

pub(super) fn write(path: &Path, text: &str, overwrite: bool) -> Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(text.as_bytes())?;
    temp.as_file().sync_all()?;
    let result = if overwrite {
        temp.persist(path)
    } else {
        temp.persist_noclobber(path)
    };
    result.map_err(|e| Error::new("TIMETABLE_WRITE", e.to_string()))?;
    Ok(())
}

fn selected_profile(
    pair: &PairEvidence,
    id: TacticId,
    exit: Option<PhraseExitKind>,
) -> Option<&PairProfile> {
    match exit {
        Some(PhraseExitKind::RhythmicHandoff) => pair.rhythmic_profiles.get(&id),
        Some(_) => pair.swift_profiles.get(&id),
        None => pair.profiles.get(&id),
    }
}

fn clock(seconds: f64) -> String {
    // Round before splitting, so 59.99 is rendered as 1:00.0.
    let tenths = (seconds * 10.).round() as u64;
    format!("{}:{:02}.{}", tenths / 600, tenths / 10 % 60, tenths % 10)
}
fn cell(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('|', "&#124;")
        .replace(['\r', '\n'], " ")
}
fn roles(value: &Value) -> String {
    value
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| format!("{} {:.2}", k.replace('_', " "), v.as_f64().unwrap_or(0.)))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}
fn metric(value: &Value, key: &str) -> String {
    value[key]
        .as_f64()
        .map(|v| format!("{v:.3}"))
        .unwrap_or_else(|| "unknown".into())
}
fn range(value: &Value, key: &str) -> String {
    let values: Vec<_> = value[key]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_f64)
        .collect();
    if values.is_empty() {
        return "unknown".into();
    }
    format!(
        "{:.3}–{:.3}",
        values.iter().copied().fold(f64::INFINITY, f64::min),
        values.iter().copied().fold(f64::NEG_INFINITY, f64::max)
    )
}
fn held_bars(t: &TacticPlan) -> f64 {
    t.bar_frames
        .windows(2)
        .map(|w| t.hold_end_frame.min(w[1]).saturating_sub(w[0]) as f64 / (w[1] - w[0]) as f64)
        .sum()
}

pub(super) fn build(
    store: &Store,
    plan: &MixPlan,
    cancel: &Cancellation,
) -> Result<(Value, String)> {
    plan.validate()?;
    let snapshot = store.read_snapshot();
    let mut cache = CandidateEvidenceCache::new();
    let tactics = read_tactics(plan)?;
    let rate = f64::from(plan.output.sample_rate_hz);
    let arrangement = read_arrangement(plan)?;
    let on_clock = |frame| arrangement.as_ref().map_or(frame, |a| a.output_at(frame));
    let mut rows = Vec::new();
    for (index, placement) in plan.placements.iter().enumerate() {
        cancel.check()?;
        let track = snapshot.track(&placement.track_id)?;
        let metadata = snapshot.recording_metadata(&track)?;
        let label = match (metadata["artist"].as_str(), metadata["track"].as_str()) {
            (Some(a), Some(t)) => format!("{a} — {t}"),
            _ => snapshot
                .source(&track)?
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        };
        let source_rate = f64::from(placement.time_map.source_sample_rate_hz);
        let mut row = json!({"number":index+1,"track_id":placement.track_id,"track":label,
            "source_seconds":[placement.source_start_frame as f64/source_rate, placement.source_end_frame as f64/source_rate],
            "mix_seconds":[on_clock(placement.time_map.anchors.first().unwrap().output_frame) as f64/rate,
                on_clock(placement.time_map.anchors.last().unwrap().output_frame) as f64/rate]});
        if let Some(t) = tactics.iter().find(|t| t.incoming_placement == index) {
            let transition_index = plan
                .transitions
                .iter()
                .position(|join| {
                    join.from_track_id == plan.placements[t.outgoing_placement].track_id
                        && join.to_track_id == placement.track_id
                        && join.start_output_frame == t.start_frame
                })
                .ok_or_else(|| {
                    Error::new(
                        "TIMETABLE_TRANSITION",
                        "saved tactic has no matching transition",
                    )
                })?;
            eprintln!(
                "timetable: measuring saved join {} ({:?})",
                transition_index + 1,
                t.tactic_id
            );
            let facts = cache.candidate_facts(&snapshot, plan, transition_index, cancel)?;
            let profile = selected_profile(
                &facts.pair,
                t.tactic_id,
                t.phrase_exit.as_ref().map(|e| e.kind),
            );
            row["transition"] = json!({"tactic":t.tactic_id,"arrangement":t.arrangement,"character":t.character,
                "mix_seconds":[on_clock(t.start_frame) as f64/rate,on_clock(t.end_frame) as f64/rate],
                "outgoing_source_cue_seconds":plan.placements[t.outgoing_placement].time_map.source_at(t.start_frame as f64)/f64::from(plan.placements[t.outgoing_placement].time_map.source_sample_rate_hz),
                "incoming_source_cue_seconds":placement.time_map.source_at(t.start_frame as f64)/source_rate,
                "bars":t.bar_frames.len()-1,"timing_basis":t.timing_basis,"held_bars":held_bars(t),"phrase_exit":t.phrase_exit,
                "selection_reason":t.selection_reason,"saved_evidence":t.evidence,
                "outgoing_roles":facts.a.roles,"incoming_roles":facts.b.roles,"profile":profile,
                "endpoint_reasons":{"outgoing":facts.a.endpoint_reasons,"incoming":facts.b.endpoint_reasons},
                "beat_skew_ms":facts.pair.beat_skew_ms,"bar_bpm":t.bar_bpm,
                "vocal_occupancy":t.vocal_occupancy,"vocal_guard_frames":t.vocal_guard_frames,
                "route_contribution":plan.header.extensions.get("chain_joins").and_then(Value::as_array).and_then(|j|j.get(transition_index))});
        }
        rows.push(row);
    }
    snapshot.verify_read_snapshot(cancel)?;
    let comparisons = measured_beam::route_comparisons(plan);
    let data = json!({"version":1,"plan_id":plan.plan_id,"duration_seconds":plan.total_output_frames as f64/rate,
        "evidence_basis":"Exact saved schedule; profiles remeasured without cue/tactic selection. Heuristic scores, not probabilities.",
        "rows":rows,"set_config":plan.header.extensions.get("set_config"),"required_pairs":plan.header.extensions.get("required_pairs"),"set_arrangement":arrangement,"frame_coordinate_system":"base timeline; mix_seconds includes replays","set_composition":plan.header.extensions.get("set_composition"),"route_comparisons":comparisons,"search":plan.header.extensions.get("lazy_search").or_else(||plan.header.extensions.get("dense_anchor_search"))});
    let mut markdown = format!("# Mix timetable and evidence\n\nPlan `{}` · duration **{}**.\n\nRows describe the saved choices. Evidence is remeasured for those cues and the chosen exit variant; no tracks or tactics are reselected. Scores are heuristics, not probabilities.\n\n| # | Track | Mix presence | Incoming transition | Hold / resolution | Source excerpt | Evidence / reason |\n|---|---|---|---|---|---|---|\n", plan.plan_id, clock(plan.total_output_frames as f64/rate));
    for row in &rows {
        let t = &row["transition"];
        let interval = |v: &Value| {
            format!(
                "{}–{}",
                clock(v[0].as_f64().unwrap_or(0.)),
                clock(v[1].as_f64().unwrap_or(0.))
            )
        };
        let (tactic, length, evidence) = if t.is_null() {
            (
                "Opening".into(),
                "—".into(),
                "Selected opening passage; no incoming join.".into(),
            )
        } else {
            let length = if t["timing_basis"].as_str() != Some("bars") {
                format!(
                    "{:.2} seconds; {} timing",
                    t["mix_seconds"][1].as_f64().unwrap() - t["mix_seconds"][0].as_f64().unwrap(),
                    t["timing_basis"].as_str().unwrap_or("unknown")
                )
            } else if let Some(exit) = t["phrase_exit"].as_object() {
                format!(
                    "{:.2} held; {:.0} ms exchange",
                    t["held_bars"].as_f64().unwrap(),
                    (exit["end_frame"].as_u64().unwrap() - exit["start_frame"].as_u64().unwrap())
                        as f64
                        / rate
                        * 1000.
                )
            } else if t["tactic"].as_str() == Some("M04") {
                format!("{}-bar isolated motif overlay; no full-mix hold", t["bars"])
            } else {
                format!(
                    "{} bars; {:.2} held + {:.2} resolve",
                    t["bars"],
                    t["held_bars"].as_f64().unwrap(),
                    t["bars"].as_f64().unwrap() - t["held_bars"].as_f64().unwrap()
                )
            };
            let p = &t["profile"];
            let mut evidence = format!("A: {}; B: {}. Complementarity {}; harmonic distance {}; lead conflict {}; density {}; A/B layer stability {} / {}. {}", roles(&t["outgoing_roles"]),roles(&t["incoming_roles"]),metric(p,"complementarity"),metric(p,"harmonic_distance"),metric(p,"lead_conflict"),range(p,"selected_composite_density"),range(p,"outgoing_layer_stability"),range(p,"incoming_layer_stability"),t["selection_reason"].as_str().unwrap_or(""));
            if let Some(pacing) = t["route_contribution"].get("pacing") {
                evidence.push_str(&format!(
                    " Pacing: {:.2}s solo, {} complete bars; {:.2} quality-second penalty. {}",
                    pacing["solo_seconds"].as_f64().unwrap_or(0.),
                    pacing["complete_bars"],
                    pacing["penalty_quality_seconds"].as_f64().unwrap_or(0.),
                    pacing["evidence"].as_str().unwrap_or("")
                ));
            }
            (
                format!(
                    "{} · {} ({})",
                    t["tactic"].as_str().unwrap(),
                    t["arrangement"].as_str().unwrap_or(""),
                    interval(&t["mix_seconds"])
                ),
                length,
                evidence,
            )
        };
        markdown.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            row["number"],
            cell(row["track"].as_str().unwrap()),
            interval(&row["mix_seconds"]),
            cell(&tactic),
            cell(&length),
            interval(&row["source_seconds"]),
            cell(&evidence)
        ));
    }
    if let Some(config) = plan.header.extensions.get("set_config") {
        markdown.push_str(&format!("\nSet recipe: **{}**. The saved plan includes the effective configuration and its content hash.\n",cell(config["name"].as_str().unwrap_or("Unnamed set"))));
    }
    if let Some(required) = plan.header.extensions.get("required_pairs") {
        markdown.push_str(&format!(
            "\n## Required pairs\n\nMode: **{}**. All required pairs are satisfied.\n\n",
            required["mode"].as_str().unwrap_or("either")
        ));
        for pair in required["pairs"].as_array().into_iter().flatten() {
            let label = |index: usize| {
                rows.iter()
                    .find(|r| r["track_id"] == pair["track_ids"][index])
                    .and_then(|r| r["track"].as_str())
                    .unwrap_or("Unknown track")
            };
            markdown.push_str(&format!(
                "- {} / {} — Spotify `{}` / `{}`\n",
                cell(label(0)),
                cell(label(1)),
                pair["spotify_ids"][0].as_str().unwrap_or(""),
                pair["spotify_ids"][1].as_str().unwrap_or("")
            ));
        }
    }
    if let Some(a) = &arrangement {
        markdown.push_str("\n## Measured phrase replays\n\nRepeated time counts toward duration, but earns no new development credit. Every insertion resumes the original passage and preserves the selected joins.\n\n| Track | Replay in mix | Captured source | Extra cycles | Evidence |\n|---|---|---|---:|---|\n");
        for r in &a.replays {
            let p = &plan.placements[r.placement];
            let sr = f64::from(p.time_map.source_sample_rate_hz);
            let end = a.output_at(r.insert_before);
            markdown.push_str(&format!("| {} | {}–{} | {}–{} | {} × {} bars | {}. PCM seam {:.3}; tonal distance {:.3}; verified vocal absence. |\n",
                cell(rows[r.placement]["track"].as_str().unwrap()),
                clock((end-r.added_frames()) as f64/rate),clock(end as f64/rate),
                clock(p.time_map.source_at(r.capture_start as f64)/sr),clock(p.time_map.source_at(r.capture_end as f64)/sr),
                r.repetitions,r.bars,if r.insert_before == r.capture_end {"Loop extension"} else {"Earlier loop reused at a measured compatible boundary"},
                r.evidence["continuation_error"].as_f64().unwrap_or(0.),r.evidence["tonal_distance"].as_f64().unwrap_or(0.)));
        }
    }
    if let Some(c) = plan.header.extensions.get("set_composition") {
        markdown.push_str(&format!("\n## Set structure\n\n{} arc; {}–{} minutes required. Actual duration {}. Composition objective {:.3}; mean intensity mismatch {:.3}; development-shortage penalty {:.3}. These are acoustic heuristics, not emotion probabilities.\n\n| Track | Solo time | Development with hold credit | Minimum development | Preferred development | Intensity percentile | Arc mismatch |\n|---|---:|---:|---:|---:|---:|---:|\n",
            c["structure"].as_str().unwrap_or("unknown"), c["minimum_seconds"].as_f64().unwrap_or(0.) / 60., c["maximum_seconds"].as_f64().unwrap_or(0.) / 60.,
            clock(c["duration_seconds"].as_f64().unwrap_or(0.)), c["objective"].as_f64().unwrap_or(0.), c["arc_error"].as_f64().unwrap_or(0.), c["dwell_penalty"].as_f64().unwrap_or(0.)));
        for passage in c["passages"].as_array().into_iter().flatten() {
            let label = rows
                .iter()
                .find(|r| r["track_id"] == passage["track_id"])
                .map_or_else(
                    || passage["track_id"].as_str().unwrap_or("unknown").to_owned(),
                    |r| r["track"].as_str().unwrap_or("unknown").to_owned(),
                );
            markdown.push_str(&format!(
                "| {} | {} | {} | {} | {} | {:.3} | {:.3} |\n",
                cell(&label),
                clock(passage["solo_seconds"].as_f64().unwrap_or(0.)),
                clock(passage["development_seconds"].as_f64().unwrap_or(0.)),
                clock(passage["minimum_seconds"].as_f64().unwrap_or(0.)),
                clock(passage["preferred_seconds"].as_f64().unwrap_or(0.)),
                passage["intensity_percentile"].as_f64().unwrap_or(0.),
                passage["arc_error"].as_f64().unwrap_or(0.)
            ));
        }
    }
    if !plan.header.extensions.contains_key("set_composition") {
        markdown.push_str("\nHigher complementarity/stability and lower harmonic distance/lead conflict are preferable. Density and stability ranges cover the candidate bars; the saved JSON preserves their individual values and exact vocal schedules.\n\n## Full-set alternatives\n\nThe search minimizes fallbacks, then weak joins, then maximizes quality-weighted sustained seconds minus saved pacing penalties, weakest-join score and total score, in that order. These comparisons concern measured candidates within the saved search budget.\n\n| Anchor | Result | First deciding criterion | Problem joins |\n|---|---|---|---|\n");
        if let Some(comparisons) = comparisons.as_array() {
            for comparison in comparisons {
                let anchor = &comparison["anchor"];
                let label = |id: &Value| {
                    rows.iter()
                        .find(|r| r["track_id"] == *id)
                        .and_then(|r| r["track"].as_str())
                        .unwrap_or_else(|| id.as_str().unwrap_or("?"))
                        .to_owned()
                };
                let name = if anchor.is_null() {
                    "Unconstrained".into()
                } else {
                    format!("{} → {}", label(&anchor["from"]), label(&anchor["to"]))
                };
                let problems = comparison["problem_joins"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|join| {
                        format!(
                            "{} → {}: {}, score {:.2}{}",
                            label(&join["from"]),
                            label(&join["to"]),
                            join["choice"].as_str().unwrap_or("unknown tactic"),
                            join["score"].as_f64().unwrap_or(0.),
                            if join["last_resort"].as_bool().unwrap_or(false) {
                                ", fallback"
                            } else {
                                ""
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                markdown.push_str(&format!(
                    "| {} | {} | {} | {} |\n",
                    cell(&name),
                    cell(comparison["result"].as_str().unwrap()),
                    cell(comparison["reason"].as_str().unwrap()),
                    cell(&problems)
                ));
            }
        }
    }
    if let Some(search) = plan.header.extensions.get("lazy_search") {
        let stats = &search["stats"];
        markdown.push_str(&format!(
            "\n## Lazy search evidence\n\nStopped at `{}` after {:.1} seconds: {} measured windows, {} cached uses, {} cheap branch rejections, {} objective-bound rejections, and {} frontier drops. Frontier drops are budget decisions, not proofs of infeasibility.\n\n| Origin | Seconds | Measurements | Sustained quality after pacing | Composition score |\n|---|---:|---:|---:|---:|\n",
            search["status"].as_str().unwrap_or("unknown"),
            search["elapsed_seconds"].as_f64().unwrap_or(0.),
            stats["measured_windows"], stats["cached_window_uses"],
            stats["cheap_rejections"], stats["bound_rejections"], stats["frontier_dropped"]
        ));
        for entry in search["improvements"].as_array().into_iter().flatten() {
            markdown.push_str(&format!(
                "| {} | {:.1} | {} | {:.3} | {} |\n",
                cell(entry["origin"].as_str().unwrap_or("searched")),
                entry["elapsed_seconds"].as_f64().unwrap_or(0.),
                entry["measured_windows"],
                entry["sustained_after_pacing"].as_f64().unwrap_or(0.),
                entry["composition"]["objective"]
                    .as_f64()
                    .map(|v| format!("{v:.3}"))
                    .unwrap_or_else(|| "—".into())
            ));
        }
    }
    Ok((data, markdown))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exit_evidence_never_falls_back_to_a_different_variant() {
        let mut pair = PairEvidence::default();
        pair.profiles.insert(
            TacticId::M01,
            PairProfile {
                method: "normal".into(),
                ..Default::default()
            },
        );
        pair.swift_profiles.insert(
            TacticId::M01,
            PairProfile {
                method: "pause".into(),
                ..Default::default()
            },
        );
        assert!(
            selected_profile(&pair, TacticId::M01, Some(PhraseExitKind::RhythmicHandoff)).is_none()
        );
        pair.rhythmic_profiles.insert(
            TacticId::M01,
            PairProfile {
                method: "rhythmic".into(),
                ..Default::default()
            },
        );
        assert_eq!(
            selected_profile(&pair, TacticId::M01, Some(PhraseExitKind::RhythmicHandoff))
                .unwrap()
                .method,
            "rhythmic"
        );
        assert_eq!(
            selected_profile(&pair, TacticId::M01, None).unwrap().method,
            "normal"
        );
    }
}
