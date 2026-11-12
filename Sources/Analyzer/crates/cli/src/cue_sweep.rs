//! A bounded, library-wide measurement pass with a comparable old-policy inventory.
use crate::duration_search::{measure_candidate, window_key, Measurement};
use crate::search_cache::Cache;
use crate::tactics::{cue_key, MeasuredEdge};
use crate::*;
use mixlib_planner::{CandidateEvidenceCache, EventCueIndex, StitchRequest};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};

const CONTEXT: usize = 12;
type Pair = (String, String);
type Window = (Pair, [u64; 4]);
type Join = (Window, TacticId, bool);

struct Prior {
    joins: BTreeSet<Join>,
    windows: BTreeMap<Window, MixPlan>,
    filter_suspects: usize,
    filter_matched: usize,
}

fn window(plan: &MixPlan) -> Window {
    (
        (
            plan.placements[0].track_id.clone(),
            plan.placements[1].track_id.clone(),
        ),
        cue_key(plan),
    )
}

fn joins(edge: &MeasuredEdge) -> Result<Vec<Join>> {
    Ok(read_tactics(&edge.plan)?
        .into_iter()
        .map(|t| (window(&edge.plan), t.tactic_id, edge.last_resort))
        .collect())
}

fn current(plan: &MixPlan, pool: &BTreeMap<&str, &Track>) -> bool {
    plan.placements.len() == 2
        && plan.placements.iter().all(|p| {
            pool.get(p.track_id.as_str()).is_some_and(|t| {
                p.audio_hash == t.audio_hash
                    && p.audio_revision == t.audio_revision
                    && t.active_analysis
                        .as_ref()
                        .is_some_and(|a| a.path == p.analysis.path && a.hash == p.analysis.hash)
                    && p.override_revision == t.override_revision
            })
        })
}

fn records(
    store: &Store,
    folder: &str,
    cancel: &Cancellation,
    mut each: impl FnMut(&str, Value) -> Result<()>,
) -> Result<()> {
    let mut paths = std::fs::read_dir(store.path(folder)?)?.collect::<std::io::Result<Vec<_>>>()?;
    paths.sort_by_key(|e| e.file_name());
    for entry in paths {
        cancel.check()?;
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|s| s.ends_with(".json")) else {
            continue;
        };
        let mut r: Report = store.read(&format!("{folder}/{name}"))?;
        if let Some(payload) = r.header.extensions.remove("payload") {
            each(name.trim_end_matches(".json"), payload)?;
        }
    }
    Ok(())
}

fn prior_seeds(
    store: &Store,
    prior: &str,
    cache: &Cache,
    pool: &[Track],
    cancel: &Cancellation,
) -> Result<Prior> {
    let tracks: BTreeMap<_, _> = pool.iter().map(|t| (t.track_id.as_str(), t)).collect();
    let mut old = BTreeSet::new();
    let mut seeds = BTreeMap::new();
    let mut wanted = BTreeMap::new();
    let mut continuation_seeds = vec![];
    let mut matched = BTreeSet::new();
    records(store, &format!("{prior}/windows"), cancel, |key, value| {
        let measured: Measurement = serde_json::from_value(value)?;
        let suspect = measured.evidence.iter().any(|w| {
            w["rejected"].as_array().into_iter().flatten().any(|r| {
                r["tactic_id"] == "S08"
                    && r["reasons"].as_array().is_some_and(|reasons| {
                        reasons.len() == 1
                            && reasons[0]
                                == "filter transfer requires measured crowded frequency occupancy"
                    })
            })
        });
        if suspect {
            for w in &measured.evidence {
                wanted
                    .entry(w["cue"].to_string())
                    .or_insert_with(BTreeSet::new)
                    .insert(key.to_owned());
            }
        }
        for edge in measured
            .edges
            .into_iter()
            .filter(|e| current(&e.plan, &tracks))
        {
            old.extend(joins(&edge)?);
            if !edge.last_resort || suspect {
                if suspect {
                    matched.insert(key.to_owned());
                }
                seeds.entry(window(&edge.plan)).or_insert(edge.plan);
            }
        }
        continuation_seeds.extend(
            measured
                .continuations
                .into_iter()
                .filter(|p| current(p, &tracks)),
        );
        Ok(())
    })?;
    let mut match_seed = |seed: MixPlan| -> Result<()> {
        let marker = seed
            .header
            .extensions
            .get("tactic_cue_candidate")
            .unwrap_or(&Value::Null)
            .to_string();
        if let Some(keys) = wanted.get(&marker) {
            if current(&seed, &tracks) {
                let key = window_key(cache, &seed)?;
                if keys.contains(&key) {
                    matched.insert(key);
                    seeds.entry(window(&seed)).or_insert(seed);
                }
            }
        }
        Ok(())
    };
    for seed in continuation_seeds {
        match_seed(seed)?;
    }
    records(store, &format!("{prior}/proposals"), cancel, |_, value| {
        for seed in value.as_array().into_iter().flatten() {
            if wanted.contains_key(&seed["extensions"]["tactic_cue_candidate"].to_string()) {
                match_seed(serde_json::from_value(seed.clone())?)?;
            }
        }
        Ok(())
    })?;
    Ok(Prior {
        joins: old,
        windows: seeds,
        filter_suspects: wanted.values().map(BTreeSet::len).sum(),
        filter_matched: matched.len(),
    })
}

#[derive(Default)]
struct Measured {
    event_joins: BTreeSet<Join>,
    filter_joins: BTreeSet<Join>,
    all: BTreeSet<Join>,
}

fn tally(joins: &BTreeSet<Join>) -> Value {
    let mut tactics: BTreeMap<TacticId, (usize, BTreeSet<&Pair>, usize)> = BTreeMap::new();
    let mut normal_pairs = BTreeSet::new();
    let mut fallback_windows = 0;
    for ((pair, _), tactic, fallback) in joins {
        let row = tactics.entry(*tactic).or_default();
        if *fallback {
            row.2 += 1;
            fallback_windows += 1;
        } else {
            row.0 += 1;
            row.1.insert(pair);
            normal_pairs.insert(pair);
        }
    }
    json!({"normal_windows":joins.len()-fallback_windows,"normal_pairs":normal_pairs.len(),
        "fallback_windows":fallback_windows,"by_tactic":tactics.into_iter().map(|(id,(normal,pairs,fallback))| {
            json!({"id":id,"normal_windows":normal,"normal_pairs":pairs.len(),"fallback_windows":fallback})
        }).collect::<Vec<_>>()})
}

pub(super) fn run(
    store: &Store,
    policy: &MixingConfig,
    prior: &str,
    limit: usize,
    cancel: &Cancellation,
) -> Result<Value> {
    let started = std::time::Instant::now();
    let snapshot = store.read_snapshot();
    let pool = measured_beam::prepared_pool(&snapshot)?;
    let cache = Cache::open(policy, &pool)?;
    let Prior {
        joins: old,
        windows: prior_windows,
        filter_suspects,
        filter_matched,
    } = prior_seeds(store, prior, &cache, &pool, cancel)?;
    let prior_count = prior_windows.len();
    eprintln!("cue audit: {} prepared tracks; {} archived windows to remeasure ({filter_matched}/{filter_suspects} filter near misses recovered)", pool.len(), prior_count);
    let mut index = EventCueIndex::new(CONTEXT);
    let mut event_pairs = 0;
    let mut checked_pairs = 0;
    let mut family_counts = BTreeMap::<String, usize>::new();
    let mut windows: BTreeMap<_, _> = prior_windows
        .into_iter()
        .map(|(key, plan)| (key, (false, plan)))
        .collect();
    for from in &pool {
        for to in pool.iter().filter(|t| t.track_id != from.track_id) {
            cancel.check()?;
            checked_pairs += 1;
            let request = StitchRequest {
                from: &from.track_id,
                to: &to.track_id,
                overlap_beats: 8,
                context_beats: CONTEXT,
                outgoing_after_frame: 0,
            };
            let candidates = index.candidates(&snapshot, &request, policy, limit, cancel)?;
            event_pairs += usize::from(!candidates.is_empty());
            for plan in candidates {
                let family = plan.header.extensions["tactic_cue_candidate"]["event_family"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_owned();
                *family_counts.entry(family).or_default() += 1;
                windows.insert(window(&plan), (true, plan));
            }
        }
        eprintln!(
            "cue audit: {checked_pairs} ordered pairs screened, {event_pairs} with event proposals"
        );
    }
    drop(index);
    let windows: Vec<_> = windows.into_values().collect();
    let total = windows.len();
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get().min(4));
    let finished = AtomicUsize::new(0);
    let next = AtomicUsize::new(0);
    let measured = std::thread::scope(|scope| -> Result<Measured> {
        let mut jobs = vec![];
        for _ in 0..workers {
            let (next, finished, windows, snapshot, cache) =
                (&next, &finished, &windows, &snapshot, &cache);
            jobs.push(scope.spawn(move || -> Result<Measured> {
                let mut facts = CandidateEvidenceCache::new();
                let mut result = Measured::default();
                while let Some((event, plan)) = windows.get(next.fetch_add(1, Ordering::Relaxed)) {
                    cancel.check()?;
                    let key = window_key(cache, plan)?;
                    let measured = measure_candidate(
                        snapshot, policy, cache, &mut facts, plan, CONTEXT, cancel,
                    )?;
                    let one_sided = measured.evidence.iter().any(|w| {
                        w["filter_crowding"][0]
                            .as_f64()
                            .zip(w["filter_crowding"][1].as_f64())
                            .is_some_and(|(a, b)| (a >= 0.5) != (b >= 0.5))
                    });
                    for join in measured
                        .edges
                        .iter()
                        .map(joins)
                        .collect::<Result<Vec<_>>>()?
                        .into_iter()
                        .flatten()
                    {
                        if *event {
                            result.event_joins.insert(join.clone());
                        }
                        if join.1 == TacticId::S08 && one_sided {
                            result.filter_joins.insert(join.clone());
                        }
                        result.all.insert(join);
                    }
                    cache.write(store, "windows", &key, &measured)?;
                    let count = finished.fetch_add(1, Ordering::Relaxed) + 1;
                    if count % 25 == 0 || count == total {
                        eprintln!(
                            "cue audit: measured {count}/{total} windows ({:.1}s)",
                            started.elapsed().as_secs_f64()
                        );
                    }
                }
                Ok(result)
            }));
        }
        let mut merged = Measured::default();
        for job in jobs {
            let result = job
                .join()
                .map_err(|_| Error::new("CUE_AUDIT_WORKER", "cue measurement worker failed"))??;
            merged.event_joins.extend(result.event_joins);
            merged.filter_joins.extend(result.filter_joins);
            merged.all.extend(result.all);
        }
        Ok(merged)
    })?;
    snapshot.verify_read_snapshot(cancel)?;
    let new_event: BTreeSet<_> = measured.event_joins.difference(&old).cloned().collect();
    let new: BTreeSet<_> = measured.all.difference(&old).cloned().collect();
    let new_event_without_filter: BTreeSet<_> = new_event
        .difference(&measured.filter_joins)
        .cloned()
        .collect();
    let result = json!({"prepared_tracks":pool.len(),"checked_ordered_pairs":checked_pairs,
        "event_pairs":event_pairs,"event_proposals_by_family":family_counts,"measured_windows":total,
        "archived_windows":prior_count,"archived_filter_near_misses":filter_suspects,"archived_filter_windows_recovered":filter_matched,
        "prior":tally(&old),"all_measured":tally(&measured.all),"new_joins":tally(&new),
        "new_event_joins_under_old_filter_rule":tally(&new_event_without_filter),
        "one_sided_filter_joins":tally(&measured.filter_joins),"elapsed_seconds":started.elapsed().as_secs_f64()});
    let mut report = Report::new("event_cue_audit");
    report
        .header
        .extensions
        .insert("audit".into(), result.clone());
    store.report(&report)?;
    Ok(json!({"report_id":report.report_id,"audit":result}))
}
