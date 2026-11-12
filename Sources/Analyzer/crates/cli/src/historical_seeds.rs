//! Reuse promising acoustic windows across admission-code generations.
//! Old approvals and winners never cross the namespace boundary: these are
//! stripped pair proposals which must pass current measurement and selection.
use super::*;
use crate::{duration_search::Measurement, search_cache::Cache, tactics::MeasuredEdge};
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};

const MAX_SEEDS: usize = 256;

struct Seed {
    plan: MixPlan,
    quality: f64,
    score: f64,
    duration: f64,
}

fn prepare(
    edge: MeasuredEdge,
    sources: &BTreeMap<String, String>,
    policy: &MixingConfig,
    pool: &BTreeMap<&str, &Track>,
    cache: &Cache,
) -> Result<Option<(String, Seed)>> {
    let quality = edge.dense_quality_seconds + edge.handoff_quality_seconds;
    let mut plan = edge.plan;
    if edge.last_resort
        || !quality.is_finite()
        || quality < 0.
        || !edge.score.is_finite()
        || plan.placements.len() != 2
        || plan.transitions.len() != 1
        || plan.drum_intro.is_some()
        || !plan.prepared_patches.is_empty()
    {
        return Ok(None);
    }
    if !plan.placements.iter().all(|p| {
        pool.get(p.track_id.as_str()).is_some_and(|t| {
            sources.get(&p.track_id).is_some_and(|signature| {
                cache.sources.get(&p.track_id) == Some(signature)
            }) && p.audio_hash == t.audio_hash
                && p.audio_revision == t.audio_revision
                && t.active_analysis.as_ref().is_some_and(|r| {
                    r.hash == p.analysis.hash && r.path == p.analysis.path
                })
                && t.override_revision == p.override_revision
                && p.time_map.source_sample_rate_hz == t.format.sample_rate_hz
                && p.source_end_frame <= t.format.frames
        })
    }) {
        return Ok(None);
    }
    // Keep only timing intent needed to reconstruct the acoustic proposal.
    // In particular, neither historical scores nor executable tactic lanes
    // may be mistaken for current approval by automatic selection.
    plan.header
        .extensions
        .retain(|k, _| matches!(k.as_str(), "tactic_cue_candidate" | "bar_bpm"));
    plan.renderer_version = mixlib_audio::RENDERER.into();
    plan.diagnostics.clear();
    for transition in &mut plan.transitions {
        transition.costs.clear();
    }
    if crate::search_cache::rebind(&mut plan, policy).is_err() {
        return Ok(None);
    }
    let t = &plan.transitions[0];
    // Different old tactic schedules and random IDs for the same acoustic
    // window need just one fresh measurement. Tempo ramps remain distinct.
    let mut maps = serde_json::to_value(
        plan.placements.iter().map(|p| &p.time_map).collect::<Vec<_>>(),
    )?;
    for map in maps.as_array_mut().unwrap() {
        map.as_object_mut().unwrap().remove("map_id");
    }
    let key = cache.pair_key(
        &plan.placements[0].track_id,
        &plan.placements[1].track_id,
        &json!({"maps":maps,"start":t.start_output_frame,"end":t.end_output_frame,
            "timing_basis":plan.header.extensions.get("tactic_cue_candidate")
                .and_then(|v|v.get("timing_basis")),
            "pulse_ratio":plan.header.extensions.get("tactic_cue_candidate")
                .and_then(|v|v.get("pulse_ratio"))}),
    )?;
    let duration = (t.end_output_frame - t.start_output_frame) as f64
        / f64::from(plan.output.sample_rate_hz);
    Ok(Some((key, Seed { plan, quality, score: edge.score, duration })))
}

fn rank(a: &Seed, b: &Seed) -> std::cmp::Ordering {
    b.quality.total_cmp(&a.quality)
        .then_with(|| b.score.total_cmp(&a.score))
        .then_with(|| b.duration.total_cmp(&a.duration))
        .then_with(|| a.plan.plan_id.cmp(&b.plan.plan_id))
}

/// Spread the bounded remeasurement budget over pairs before second/third cues.
fn diverse(mut seeds: Vec<Seed>) -> VecDeque<MixPlan> {
    seeds.sort_by(rank);
    let mut pairs: BTreeMap<(String, String), VecDeque<Seed>> = BTreeMap::new();
    let mut order = vec![];
    for seed in seeds {
        let pair = (seed.plan.placements[0].track_id.clone(), seed.plan.placements[1].track_id.clone());
        if !pairs.contains_key(&pair) {
            order.push(pair.clone());
        }
        pairs.entry(pair).or_default().push_back(seed);
    }
    let mut result = VecDeque::new();
    loop {
        let previous = result.len();
        for pair in &order {
            if let Some(seed) = pairs.get_mut(pair).unwrap().pop_front() {
                result.push_back(seed.plan);
            }
        }
        if result.len() == previous {
            return result;
        }
    }
}

pub(super) fn load(
    store: &Store,
    policy: &MixingConfig,
    pool: &[Track],
    cache: &Cache,
    cancel: &Cancellation,
) -> Result<Vec<MixPlan>> {
    let pool: BTreeMap<_, _> = pool.iter().map(|t| (t.track_id.as_str(), t)).collect();
    let roots = cache.previous_roots(store, 2)?;
    let mut seeds: BTreeMap<String, Seed> = BTreeMap::new();
    let mut records = 0;
    for root in &roots {
        for entry in std::fs::read_dir(store.path(&format!("{root}/windows"))?)? {
            cancel.check()?;
            let entry = entry?;
            if !entry.file_type()?.is_file()
                || entry.path().extension().is_none_or(|e| e != "json")
            {
                continue;
            }
            let path = format!("{root}/windows/{}", entry.file_name().to_string_lossy());
            let Ok(report) = store.read::<Report>(&path) else { continue };
            let Some(payload) = report.header.extensions.get("payload") else { continue };
            if report.kind != "set_search_cache"
                || report.header.extensions.get("digest") != Some(&json!(canonical_hash(payload)?))
            {
                continue;
            }
            let Ok(measurement) = serde_json::from_value::<Measurement>(payload.clone()) else { continue };
            records += 1;
            for edge in measurement.edges {
                if let Some((key, seed)) = prepare(edge, &measurement.sources, policy, &pool, cache)? {
                    match seeds.get_mut(&key) {
                        Some(previous) if rank(&seed, previous).is_lt() => *previous = seed,
                        None => { seeds.insert(key, seed); }
                        _ => {}
                    }
                }
            }
        }
    }
    let unique = seeds.len();
    let (quality, baseline): (Vec<_>, Vec<_>) = seeds.into_values().partition(|s| s.quality > 0.);
    let mut quality = diverse(quality);
    let mut baseline = diverse(baseline);
    let mut result = vec![];
    while result.len() < MAX_SEEDS && (!quality.is_empty() || !baseline.is_empty()) {
        if let Some(plan) = quality.pop_front().or_else(|| baseline.pop_front()) {
            result.push(plan);
        }
        if result.len() < MAX_SEEDS {
            if let Some(plan) = baseline.pop_front().or_else(|| quality.pop_front()) {
                result.push(plan);
            }
        }
    }
    eprintln!("set search: {} historical cue/time-map seeds from {} unique windows in {} checked records / {} prior namespaces; all require current measurement", result.len(), unique, records, roots.len());
    Ok(result)
}
