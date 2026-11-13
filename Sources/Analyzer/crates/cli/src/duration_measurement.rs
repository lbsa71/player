//! Durable pair-window identity, measurement, and admission.
use super::*;
use crate::tactics::CandidateQueue;

#[derive(Clone, Default, Serialize, Deserialize)]
pub(in super::super) struct Measurement {
    #[serde(default)]
    pub(in super::super) sources: BTreeMap<String, String>,
    pub(in super::super) edges: Vec<MeasuredEdge>,
    pub(in super::super) continuations: Vec<MixPlan>,
    pub(in super::super) admitted: BTreeMap<String, usize>,
    pub(in super::super) rejected: BTreeMap<String, usize>,
    pub(in super::super) evidence: Vec<Value>,
}

pub(in super::super) fn window_key(cache: &Cache, seed: &MixPlan) -> Result<String> {
    // Time maps (including all tempo ramps), source intervals and cue facts
    // define the measured window. Random plan/map IDs do not define sound.
    let mut acoustic = serde_json::to_value(seed)?;
    remove_ids(&mut acoustic);
    acoustic.as_object_mut().unwrap().remove("policy");
    acoustic.as_object_mut().unwrap().remove("policy_hash");
    if let Some(e) = acoustic
        .get_mut("extensions")
        .and_then(Value::as_object_mut)
    {
        e.remove("set_config");
    }
    cache.pair_key(
        &seed.placements[0].track_id,
        &seed.placements[1].track_id,
        &acoustic,
    )
}

pub(in super::super) fn measure_candidate(
    snapshot: &Store,
    policy: &MixingConfig,
    cache: &Cache,
    facts: &mut CandidateEvidenceCache,
    seed: &MixPlan,
    context: usize,
    cancel: &Cancellation,
) -> Result<Measurement> {
    let mut admission = tactics::policy(
        policy
            .header
            .extensions
            .get("allow_expressive_tactics")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    );
    admission.normal_speed_deviation = policy.max_speed_deviation;
    let mut queue = CandidateQueue::expansion_only(std::slice::from_ref(seed), 24);
    let mut admitted = BTreeMap::new();
    let mut rejected = BTreeMap::new();
    let mut evidence = vec![];
    let request = StitchRequest {
        from: &seed.placements[0].track_id,
        to: &seed.placements[1].track_id,
        overlap_beats: 8,
        context_beats: context,
        outgoing_after_frame: 0,
    };
    let edges = tactics::measure_window(
        snapshot,
        &request,
        policy,
        facts,
        seed,
        &mut queue,
        &admission,
        &mut admitted,
        &mut rejected,
        &mut evidence,
        cancel,
    )?;
    require(
        edges.iter().all(|e| {
            e.dense_quality_seconds + e.handoff_quality_seconds
                <= mixlib_planner::MAX_TACTIC_CONTINUATION_SECONDS + 1e-6
        }),
        "LAZY_BOUND",
        "measured quality exceeds future-join bound",
    )?;
    Ok(Measurement {
        sources: seed
            .placements
            .iter()
            .map(|p| (p.track_id.clone(), cache.sources[&p.track_id].clone()))
            .collect(),
        edges,
        continuations: queue.take_discovered(),
        admitted,
        rejected,
        evidence,
    })
}

pub(in super::super) fn remove_ids(value: &mut Value) {
    match value {
        Value::Object(m) => {
            for k in ["plan_id", "map_id", "transition_id", "created_at"] {
                m.remove(k);
            }
            for v in m.values_mut() {
                remove_ids(v)
            }
        }
        Value::Array(a) => {
            for v in a {
                remove_ids(v)
            }
        }
        _ => {}
    }
}
