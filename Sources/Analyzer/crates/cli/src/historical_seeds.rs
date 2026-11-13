//! Reuse promising acoustic windows across admission-code generations.
//! Old approvals and winners never cross the namespace boundary: these are
//! stripped pair proposals which must pass current measurement and selection.
use super::*;
use crate::{duration_search::Measurement, search_cache::Cache, tactics::MeasuredEdge};
use serde_json::json;
use std::collections::{BTreeMap, VecDeque};
use std::time::Instant;

const MAX_SEEDS: usize = 256;

#[cfg(feature = "cp-search")]
pub(super) struct ArchivedSet {
    pub plan: MixPlan,
    pub windows: Vec<MixPlan>,
}

#[cfg(feature = "cp-search")]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct JoinKey<'a> {
    from: &'a str,
    to: &'a str,
    exit: u64,
    entry: u64,
    overlap: u64,
}

#[cfg(feature = "cp-search")]
impl<'a> JoinKey<'a> {
    fn saved(join: &'a serde_json::Value, transition: &Transition) -> Option<Self> {
        Some(Self {
            from: join["from"].as_str()?,
            to: join["to"].as_str()?,
            exit: join["outgoing_cue_frame"].as_u64()?,
            entry: join["incoming_cue_frame"].as_u64()?,
            overlap: transition
                .end_output_frame
                .checked_sub(transition.start_output_frame)?,
        })
    }

    fn measured(edge: &'a serde_json::Value) -> Option<Self> {
        if edge["last_resort"].as_bool()? {
            return None;
        }
        let [a, b] = edge["plan"]["placements"].as_array()?.as_slice() else {
            return None;
        };
        let [transition] = edge["plan"]["transitions"].as_array()?.as_slice() else {
            return None;
        };
        Some(Self {
            from: a["track_id"].as_str()?,
            to: b["track_id"].as_str()?,
            exit: edge["exit"].as_u64()?,
            entry: b["source_start_frame"].as_u64()?,
            overlap: transition["end_output_frame"]
                .as_u64()?
                .checked_sub(transition["start_output_frame"].as_u64()?)?,
        })
    }
}

fn checked_payload(report: &Report) -> Result<Option<&serde_json::Value>> {
    let Some(payload) = report.header.extensions.get("payload") else {
        return Ok(None);
    };
    Ok((report.kind == "set_search_cache"
        && report.header.extensions.get("digest") == Some(&json!(canonical_hash(payload)?)))
    .then_some(payload))
}

/// A previous winner supplies an ordered set of timing requests. Each
/// returned window has stripped tactics and must pass current admission.
#[cfg(feature = "cp-search")]
pub(super) fn load_set(
    store: &Store,
    policy: &MixingConfig,
    pool: &[Track],
    cache: &Cache,
    max_windows: usize,
    deadline: Instant,
    cancel: &Cancellation,
) -> Result<Option<ArchivedSet>> {
    let set_key = Cache::set_key(policy)?;
    let roots = cache.previous_roots(store, usize::MAX)?;
    let pool: BTreeMap<_, _> = pool.iter().map(|t| (t.track_id.as_str(), t)).collect();
    for root in roots {
        cancel.check()?;
        if Instant::now() >= deadline {
            break;
        }
        let Ok(report) = store.read::<Report>(&format!("{root}/sets/{set_key}.json")) else {
            continue;
        };
        let Some(reference) = checked_payload(&report)? else {
            continue;
        };
        let Some(plan_id) = reference["plan_id"]
            .as_str()
            .filter(|id| id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'))
        else {
            continue;
        };
        let Ok(plan) = store.read::<MixPlan>(&format!("plans/{plan_id}.json")) else {
            continue;
        };
        if plan.transitions.is_empty() || plan.transitions.len() > max_windows || !plan.complete {
            continue;
        }
        let Some(pins) = reference["sources"].as_object() else {
            continue;
        };
        if !plan.placements.iter().all(|p| {
            pins.get(&p.track_id).and_then(serde_json::Value::as_str)
                == cache.sources.get(&p.track_id).map(String::as_str)
                && cache.sources.contains_key(&p.track_id)
        }) {
            continue;
        }
        let Some(joins) = plan
            .header
            .extensions
            .get("chain_joins")
            .and_then(serde_json::Value::as_array)
        else {
            continue;
        };
        if joins.len() != plan.transitions.len() {
            continue;
        }
        let Some(targets) = joins
            .iter()
            .zip(&plan.transitions)
            .enumerate()
            .map(|(i, (join, transition))| Some((JoinKey::saved(join, transition)?, i)))
            .collect::<Option<BTreeMap<_, _>>>()
        else {
            continue;
        };
        if targets.len() != joins.len() {
            continue;
        }
        let mut windows = vec![None; joins.len()];
        for entry in std::fs::read_dir(store.path(&format!("{root}/windows"))?)? {
            cancel.check()?;
            if Instant::now() >= deadline {
                return Ok(None);
            }
            let entry = entry?;
            if !entry.file_type()?.is_file() || entry.path().extension().is_none_or(|e| e != "json")
            {
                continue;
            }
            let Ok(report) = store.read::<Report>(&format!(
                "{root}/windows/{}",
                entry.file_name().to_string_lossy()
            )) else {
                continue;
            };
            // Check exact cue coordinates before hashing or decoding bulky
            // unrelated tactic evidence from an older cache.
            let Some(edges) = report
                .header
                .extensions
                .get("payload")
                .and_then(|v| v["edges"].as_array())
            else {
                continue;
            };
            let matches = edges
                .iter()
                .enumerate()
                .filter_map(|(i, edge)| {
                    let &slot = targets.get(&JoinKey::measured(edge)?)?;
                    let score = edge["score"].as_f64()?;
                    let saved_score = joins[slot]["score"].as_f64()?;
                    (windows[slot].is_none() && (score - saved_score).abs() < 1e-8)
                        .then_some((i, slot))
                })
                .collect::<Vec<_>>();
            if matches.is_empty() || checked_payload(&report)?.is_none() {
                continue;
            }
            let Ok(sources) = serde_json::from_value::<BTreeMap<String, String>>(
                report.header.extensions["payload"]["sources"].clone(),
            ) else {
                continue;
            };
            for (i, slot) in matches {
                let Ok(edge) = serde_json::from_value::<MeasuredEdge>(edges[i].clone()) else {
                    continue;
                };
                if let Some((_, seed)) = prepare(edge, &sources, policy, &pool, cache)? {
                    windows[slot] = Some(seed.plan);
                }
            }
            if windows.iter().all(Option::is_some) {
                return Ok(Some(ArchivedSet {
                    plan,
                    windows: windows.into_iter().flatten().collect(),
                }));
            }
        }
    }
    Ok(None)
}

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
            sources
                .get(&p.track_id)
                .is_some_and(|signature| cache.sources.get(&p.track_id) == Some(signature))
                && p.audio_hash == t.audio_hash
                && p.audio_revision == t.audio_revision
                && t.active_analysis
                    .as_ref()
                    .is_some_and(|r| r.hash == p.analysis.hash && r.path == p.analysis.path)
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
        plan.placements
            .iter()
            .map(|p| &p.time_map)
            .collect::<Vec<_>>(),
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
    let duration =
        (t.end_output_frame - t.start_output_frame) as f64 / f64::from(plan.output.sample_rate_hz);
    Ok(Some((
        key,
        Seed {
            plan,
            quality,
            score: edge.score,
            duration,
        },
    )))
}

fn rank(a: &Seed, b: &Seed) -> std::cmp::Ordering {
    b.quality
        .total_cmp(&a.quality)
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
        let pair = (
            seed.plan.placements[0].track_id.clone(),
            seed.plan.placements[1].track_id.clone(),
        );
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
    deadline: Instant,
    cancel: &Cancellation,
) -> Result<Vec<MixPlan>> {
    if Instant::now() >= deadline {
        return Ok(vec![]);
    }
    let pool: BTreeMap<_, _> = pool.iter().map(|t| (t.track_id.as_str(), t)).collect();
    let roots = cache.previous_roots(store, 2)?;
    let mut seeds: BTreeMap<String, Seed> = BTreeMap::new();
    let mut records = 0;
    'roots: for root in &roots {
        for entry in std::fs::read_dir(store.path(&format!("{root}/windows"))?)? {
            cancel.check()?;
            if Instant::now() >= deadline {
                break 'roots;
            }
            let entry = entry?;
            if !entry.file_type()?.is_file() || entry.path().extension().is_none_or(|e| e != "json")
            {
                continue;
            }
            let path = format!("{root}/windows/{}", entry.file_name().to_string_lossy());
            let Ok(report) = store.read::<Report>(&path) else {
                continue;
            };
            let Some(payload) = checked_payload(&report)? else {
                continue;
            };
            let Ok(measurement) = serde_json::from_value::<Measurement>(payload.clone()) else {
                continue;
            };
            records += 1;
            for edge in measurement.edges {
                if let Some((key, seed)) =
                    prepare(edge, &measurement.sources, policy, &pool, cache)?
                {
                    match seeds.get_mut(&key) {
                        Some(previous) if rank(&seed, previous).is_lt() => *previous = seed,
                        None => {
                            seeds.insert(key, seed);
                        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (MixPlan, Vec<Track>) {
        let mut plan: MixPlan =
            serde_json::from_str(include_str!("../../../examples/mix-plan.json")).unwrap();
        plan.placements.truncate(2);
        plan.transitions.truncate(1);
        plan.total_output_frames = plan.placements[1]
            .time_map
            .anchors
            .last()
            .unwrap()
            .output_frame;
        plan.validate().unwrap();
        let pool = plan
            .placements
            .iter()
            .map(|p| Track {
                header: Header::default(),
                track_id: p.track_id.clone(),
                audio_revision: p.audio_revision,
                audio_hash: p.audio_hash.clone(),
                format: AudioFormat {
                    sample_rate_hz: p.time_map.source_sample_rate_hz,
                    channels: 2,
                    encoding: Encoding::IeeeFloat,
                    container_bits: 32,
                    valid_bits: 32,
                    channel_mask: 3,
                    frames: p.source_end_frame + 48000,
                },
                source_locations: vec![],
                active_analysis: Some(p.analysis.clone()),
                active_overrides: None,
                override_revision: p.override_revision,
                lifecycle: Lifecycle::Available,
            })
            .collect();
        (plan, pool)
    }

    fn edge(plan: MixPlan) -> MeasuredEdge {
        MeasuredEdge {
            plan,
            exit: 0,
            entry_end: 0,
            score: 100.,
            last_resort: false,
            dense_quality_seconds: 16.,
            handoff_quality_seconds: 0.,
        }
    }

    fn save_old(store: &Store, cache: &Cache, value: &Measurement) -> String {
        cache.write(store, "windows", "current", value).unwrap();
        let current = store
            .root
            .join("search-cache/v1")
            .join(env!("MIXLIB_ADMISSION_RULES_HASH"));
        let policy = std::fs::read_dir(current)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .file_name();
        let path = format!(
            "search-cache/v1/{}/{}/windows/old.json",
            "a".repeat(64),
            policy.to_string_lossy()
        );
        let payload = serde_json::to_value(value).unwrap();
        let mut report = Report::new("set_search_cache");
        report
            .header
            .extensions
            .insert("digest".into(), json!(canonical_hash(&payload).unwrap()));
        report.header.extensions.insert("payload".into(), payload);
        store.write(&path, &report).unwrap();
        path
    }

    #[test]
    fn old_approval_is_stripped_deduplicated_and_rebound_before_remeasurement() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(dir.path()).unwrap();
        let (mut plan, pool) = fixture();
        // This old executable tactic is deliberately invalid under today's
        // schema. It must not be validated/imported as an approved schedule.
        plan.header.extensions.insert(
            TACTICS_EXTENSION.into(),
            json!([{"obsolete_approval":true}]),
        );
        plan.header
            .extensions
            .insert("dense_quality_seconds".into(), json!(999));
        plan.header
            .extensions
            .insert("tactic_selection".into(), json!({"approved":true}));
        let mut policy = plan.policy.clone();
        policy
            .header
            .extensions
            .insert("set_minutes".into(), json!(120));
        let cache = Cache::open(&policy, &pool).unwrap();
        let mut duplicate = plan.clone();
        duplicate.plan_id = id();
        duplicate.placements[0].time_map.map_id = id();
        let mut nil = edge(plan.clone());
        nil.last_resort = true;
        let measurement = Measurement {
            sources: cache.sources.clone(),
            edges: vec![edge(plan), edge(duplicate), nil],
            ..Measurement::default()
        };
        save_old(&store, &cache, &measurement);
        let loaded = load(
            &store,
            &policy,
            &pool,
            &cache,
            Instant::now() + std::time::Duration::from_secs(5),
            &Cancellation::default(),
        )
        .unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(read_tactics(&loaded[0]).unwrap().is_empty());
        assert!(!loaded[0]
            .header
            .extensions
            .contains_key("dense_quality_seconds"));
        assert!(!loaded[0].header.extensions.contains_key("tactic_selection"));
        assert_eq!(
            loaded[0].policy.header.extensions["set_minutes"],
            json!(120)
        );
        assert_eq!(loaded[0].renderer_version, mixlib_audio::RENDERER);
        loaded[0].validate().unwrap();
    }

    #[test]
    fn changed_sources_pins_and_missing_signatures_are_not_reused() {
        let (plan, pool) = fixture();
        let cache = Cache::open(&plan.policy, &pool).unwrap();
        let indexed = pool.iter().map(|t| (t.track_id.as_str(), t)).collect();
        let mut sources = cache.sources.clone();
        sources.insert(
            pool[0].track_id.clone(),
            "changed audio or analysis or override".into(),
        );
        assert!(
            prepare(edge(plan.clone()), &sources, &plan.policy, &indexed, &cache)
                .unwrap()
                .is_none()
        );
        assert!(prepare(
            edge(plan.clone()),
            &BTreeMap::new(),
            &plan.policy,
            &indexed,
            &cache
        )
        .unwrap()
        .is_none());
        for mismatch in 0..4 {
            let mut old = plan.clone();
            match mismatch {
                0 => old.placements[0].audio_revision += 1,
                1 => old.placements[0].audio_hash = hash_bytes(b"different audio"),
                2 => old.placements[0].analysis.hash = hash_bytes(b"different analysis"),
                _ => old.placements[0].override_revision += 1,
            }
            assert!(
                prepare(edge(old), &cache.sources, &plan.policy, &indexed, &cache)
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn corrupted_historical_checksum_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(dir.path()).unwrap();
        let (plan, pool) = fixture();
        let policy = plan.policy.clone();
        let cache = Cache::open(&policy, &pool).unwrap();
        let measurement = Measurement {
            sources: cache.sources.clone(),
            edges: vec![edge(plan)],
            ..Measurement::default()
        };
        let path = save_old(&store, &cache, &measurement);
        let mut report = store.read::<Report>(&path).unwrap();
        report.header.extensions.get_mut("payload").unwrap()["edges"][0]["score"] = json!(1000);
        store.write(&path, &report).unwrap();
        assert!(load(
            &store,
            &policy,
            &pool,
            &cache,
            Instant::now() + std::time::Duration::from_secs(5),
            &Cancellation::default()
        )
        .unwrap()
        .is_empty());
    }

    #[cfg(feature = "cp-search")]
    #[test]
    fn archived_set_requires_every_exact_join_and_current_source_pins() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(dir.path()).unwrap();
        let (mut plan, pool) = fixture();
        let policy = plan.policy.clone();
        let cache = Cache::open(&policy, &pool).unwrap();
        let pair = plan.clone();
        let a = &pair.placements[0];
        let b = &pair.placements[1];
        let mut measured_edge = edge(pair.clone());
        measured_edge.exit = a
            .time_map
            .source_at(pair.transitions[0].start_output_frame as f64)
            .round() as u64;
        plan.header.extensions.insert(
            "chain_joins".into(),
            json!([{
                "from": a.track_id, "to": b.track_id, "outgoing_cue_frame": measured_edge.exit,
                "incoming_cue_frame": b.source_start_frame, "score": measured_edge.score,
            }]),
        );
        store
            .write(&format!("plans/{}.json", plan.plan_id), &plan)
            .unwrap();
        let mut measurement = Measurement {
            sources: cache.sources.clone(),
            edges: vec![measured_edge],
            ..Measurement::default()
        };
        let old = save_old(&store, &cache, &measurement);
        let root = old.rsplit_once("/windows/").unwrap().0;
        let payload = json!({"plan_id": plan.plan_id, "sources": cache.sources});
        let mut reference = Report::new("set_search_cache");
        reference
            .header
            .extensions
            .insert("digest".into(), json!(canonical_hash(&payload).unwrap()));
        reference
            .header
            .extensions
            .insert("payload".into(), payload);
        store
            .write(
                &format!("{root}/sets/{}.json", Cache::set_key(&policy).unwrap()),
                &reference,
            )
            .unwrap();
        let load = |cache: &Cache, allowance| {
            load_set(
                &store,
                &policy,
                &pool,
                cache,
                allowance,
                Instant::now() + std::time::Duration::from_secs(2),
                &Cancellation::default(),
            )
            .unwrap()
        };
        assert!(load(&cache, 0).is_none());
        let recovered = load(&cache, 1).expect("exact archived timing request");
        assert_eq!(recovered.plan.plan_id, plan.plan_id);
        assert_eq!(recovered.windows.len(), 1);
        assert!(read_tactics(&recovered.windows[0]).unwrap().is_empty());
        measurement.edges[0].exit += 1;
        save_old(&store, &cache, &measurement);
        assert!(load(&cache, 1).is_none());
        measurement.edges[0].exit -= 1;
        save_old(&store, &cache, &measurement);
        let mut changed = Cache::open(&policy, &pool).unwrap();
        changed
            .sources
            .insert(a.track_id.clone(), "different generation".into());
        assert!(load(&changed, 1).is_none());
    }
}
