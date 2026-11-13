//! Revalidate a previous incumbent through today's pair oracle and composer.
use super::*;

pub(super) fn recover(
    r: &mut Refiner<'_>,
    measurement_limit: usize,
    deadline: Instant,
    cancel: &Cancellation,
) -> Result<Option<Value>> {
    if r.cp.is_none()
        || r.search.incumbent.is_some()
        || r.search.cache.mode != crate::set_config::CacheMode::Resume
    {
        return Ok(None);
    }
    let started = Instant::now();
    let remaining = deadline.saturating_duration_since(started);
    let scan_deadline = started + (remaining / 4).min(std::time::Duration::from_secs(12));
    let Some(archive) = crate::historical_seeds::load_set(
        r.search.store,
        r.search.policy,
        &r.search.pool,
        &r.search.cache,
        measurement_limit,
        scan_deadline,
        cancel,
    )?
    else {
        return Ok(None);
    };
    if !r.search.required.satisfied(&archive.plan) || !r.search.schedules_plan(&archive.plan) {
        return Ok(None);
    }
    let expected = archive.windows.len();
    let mut results = Vec::new();
    let before = r.search.stats.measured_windows;
    eprintln!("set search: revalidating {expected} joins from an archived set");
    for batch in archive.windows.chunks(4) {
        if Instant::now() >= deadline {
            break;
        }
        let measured = r.search.measure_batch(batch, cancel)?;
        for measurement in &measured {
            r.search.queue_continuations(
                &measurement.sources,
                measurement.continuations.clone(),
                false,
                false,
            )?;
        }
        results.extend(measured);
    }
    r.sync_edges()?;
    let old_tactics = read_tactics(&archive.plan).unwrap_or_default();
    let admitted = results
        .iter()
        .enumerate()
        .map(|(i, measurement)| {
            let preferred = old_tactics.get(i).map(|t| t.tactic_id);
            measurement
                .edges
                .iter()
                .filter(|edge| !edge.last_resort)
                .max_by(|a, b| {
                    let same_tactic = |edge: &MeasuredEdge| {
                        preferred.is_some_and(|id| {
                            read_tactics(&edge.plan).is_ok_and(|tactics| {
                                tactics.first().is_some_and(|t| t.tactic_id == id)
                            })
                        })
                    };
                    same_tactic(a)
                        .cmp(&same_tactic(b))
                        .then(a.score.total_cmp(&b.score))
                })
        })
        .collect::<Vec<_>>();
    let admitted_joins = admitted.iter().flatten().count();
    let mut status = "join_unresolved";
    if admitted_joins == expected && Instant::now() < deadline {
        let lookup = r
            .edges
            .iter()
            .enumerate()
            .map(|(i, _)| Ok((r.persistent_key(i)?, i)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let indices = admitted
            .iter()
            .flatten()
            .map(|edge| Ok(lookup.get(&measured_edge_key(edge)?).copied()))
            .collect::<Result<Option<Vec<_>>>>()?;
        if let Some(indices) = indices {
            status = close_reverified(r, &archive.plan, &indices, deadline, cancel)?;
        }
    }
    Ok(Some(json!({
        "archived_plan_id": archive.plan.plan_id,
        "expected_joins": expected,
        "attempted_windows": results.len(),
        "fresh_windows": r.search.stats.measured_windows - before,
        "admitted_joins": admitted_joins,
        "status": status,
        "elapsed_seconds": started.elapsed().as_secs_f64(),
    })))
}

fn close_reverified(
    r: &mut Refiner<'_>,
    archived: &MixPlan,
    edges: &[usize],
    deadline: Instant,
    cancel: &Cancellation,
) -> Result<&'static str> {
    let Ok((_, raw)) = r.assemble_measured_for_closing(edges)? else {
        return Ok("assembly_rejected");
    };
    if !r.search.required.satisfied(&raw) {
        return Ok("required_pairs");
    }
    let keys = r.closing_route_keys(edges)?;
    r.cp.as_mut()
        .unwrap()
        .closing
        .queue_assembled(edges, keys, raw.clone(), None)?;
    // Reuse the old bookend coordinates only if they appear in the current
    // domain. Never copy its replay or its full-set score.
    let mut plan = raw;
    let start = archived.placements[0].source_start_frame;
    if r.search.composer.opening_choices(&plan).contains(&start)
        && chain::open_measured(&mut plan, start).is_ok()
    {
        let (_, endings) = r
            .search
            .composer
            .ending_choices(&r.search.snapshot, &plan, cancel)?;
        let end = archived.placements.last().unwrap().source_end_frame;
        for ending in endings.into_iter().filter(|ending| ending.end == end) {
            let finished = match chain::finish_measured_at_bookends(plan.clone(), ending) {
                Ok(plan) => plan,
                Err(error) if measured_beam::route_local(&error) => continue,
                Err(error) => return Err(error),
            };
            if r.search.save(finished, "reverified_archived_set", cancel)? {
                return Ok("approved");
            }
        }
    }
    let closing_deadline = deadline.min(Instant::now() + std::time::Duration::from_secs(2));
    while r.search.incumbent.is_none()
        && Instant::now() < closing_deadline
        && r.close_cp_routes(closing_deadline, cancel)?
    {}
    Ok(if r.search.incumbent.is_some() {
        "approved"
    } else {
        "closing_pending"
    })
}
