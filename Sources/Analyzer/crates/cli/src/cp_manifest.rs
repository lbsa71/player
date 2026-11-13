//! Versioned source, policy, and enumeration scope for the CP search.

use super::*;
use mixlib_set_search::objective::{
    ARC_WEIGHT, DEVELOPMENT_WEIGHT, PREFERRED_TRANSITION_FIT, REPLAY_WEIGHT,
};

#[derive(Serialize)]
pub(super) struct SearchDomainManifest {
    pub(super) id: String,
    pub(super) declaration: Value,
}

impl SearchDomainManifest {
    pub(super) fn from_search(search: &Search<'_>) -> Result<Self> {
        let pool = search
            .pool
            .iter()
            .map(|track| {
                json!({
                    "id": track.track_id,
                    "audio_hash": track.audio_hash,
                    "audio_revision": track.audio_revision,
                    "analysis": track.active_analysis,
                    "overrides": track.active_overrides,
                    "override_revision": track.override_revision,
                    "frames": track.format.frames,
                    "sample_rate_hz": track.format.sample_rate_hz,
                })
            })
            .collect::<Vec<_>>();
        let expressive = search
            .policy
            .header
            .extensions
            .get("allow_expressive_tactics")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut admission = crate::tactics::policy(expressive);
        admission.normal_speed_deviation = search.policy.max_speed_deviation;
        let declaration = json!({
            "version": 1,
            "scope": "experimental CP duration search",
            "domain_complete": false,
            "pool": pool,
            "pool_selection": "Prepared automatic song pool, deduplicated by source audio and recording identity",
            "admission_policy_hash": crate::search_cache::policy_key(search.policy)?,
            "admission_implementation_hash": env!("MIXLIB_ADMISSION_RULES_HASH"),
            "search_implementation_hash": env!("MIXLIB_MUSICAL_RULES_HASH"),
            "set_policy_hash": Cache::set_key(search.policy)?,
            "required_pairs": search.required.evidence(),
            "excluded_directed_transitions": search.excluded_transitions.iter().map(|&(from, to)| {
                [&search.pool[from].track_id, &search.pool[to].track_id]
            }).collect::<Vec<_>>(),
            "timing": {
                "output_sample_rate_hz": search.policy.output_sample_rate_hz,
                "minimum_seconds": search.request.min(),
                "maximum_seconds": search.request.max(),
                "context_beats": search.context,
                "max_speed_deviation": search.policy.max_speed_deviation,
                "max_speed_change_per_second": search.policy.max_speed_change_per_second,
                "coordinate_system": "integer output frames",
            },
            "families": {
                "pinned_bar_windows": {
                    "profile": mixlib_planner::BAR_CUE_PROFILE,
                    "enumeration": "reliable downbeat pairs; 1–16 outgoing bars; 1:1, 2:1, and 1:2 pulse mappings",
                    "coverage": "finite timing cells with template and row interval pruning; musical realizations require further enumeration",
                },
                "pinned_structural_windows": {
                    "profile": mixlib_planner::STRUCTURAL_CUE_PROFILE,
                    "enumeration": "natural tail/attack windows and all measured sparse-window pairs under native-clock timing",
                    "coverage": "finite timing cells; missing structural evidence remains unresolved and musical admission is separate",
                },
                "detected_endpoint_extensions": {
                    "profile": continuation::ENDPOINT_PROFILE,
                    "coverage": "all supported detected later endpoints for each enumerated measured cue pair and pulse mapping; windows still require tactic admission",
                    "maximum_pulses_per_side": continuation::MAX_ENDPOINT_PULSES,
                    "maximum_overlap_seconds": mixlib_planner::MAX_TACTIC_CONTINUATION_SECONDS,
                },
                "tactics": {
                    "policy_version": TACTICS_POLICY_VERSION,
                    "policy": admission,
                    "primary_catalogue": TacticId::ALL.into_iter().filter(|id| !id.is_modifier()).collect::<Vec<_>>(),
                    "selector_profile": mixlib_planner::SELECTOR_REALIZATION_PROFILE,
                    "selector_variants": mixlib_planner::selector_realization_domain().collect::<Vec<_>>(),
                    "coverage": "standard modes, every qualifying 20 ms accompaniment exit, and every detected rhythmic handoff pulse in the final half-bar; unmeasured cues and other timing choices remain unresolved",
                },
                "joined_tempo": {
                    "selection": "three-interval bridge followed by the first valid fixed-clock smoothstep fallback",
                    "fallback_interval_counts": chain::JOINED_TEMPO_RAMP_INTERVALS,
                    "coverage": "deterministic assembly of each measured arrival/departure pair, at fixed overlap anchors and output frames",
                },
                "openings": {
                    "profile": "measured-opening-cues/1",
                    "coverage": "preferred opening, first audible attack and every clear accented entry before the first measured context",
                },
                "endings": { "coverage": "enumerated natural tails and clear phrase fades for fixed measured routes and openings" },
                "replay": {
                    "coverage": "verified clear phrase regions and destinations for fixed measured routes and endings",
                    "max_regions_per_placement": 1,
                    "max_extra_cycles_per_region": 3,
                    "max_fraction_of_base_clock": MAX_REPLAY_FRACTION,
                },
            },
            "objective": {
                "version": "set-composition/1",
                "preferred_transition_fit": PREFERRED_TRANSITION_FIT,
                "arc_weight": ARC_WEIGHT,
                "development_weight": DEVELOPMENT_WEIGHT,
                "replay_weight": REPLAY_WEIGHT,
                "tie_policy": "Retain the verified incumbent unless the score strictly increases",
            },
        });
        let id = canonical_hash(&declaration)?.digest;
        Ok(Self { id, declaration })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_identity_tracks_sources_and_semantics_but_ignores_budgets() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(directory.path()).unwrap();
        let policy = MixingConfig::default();
        let mut r = super::super::tests::fixture(&store, &policy, &[1200; 3]);
        let first = SearchDomainManifest::from_search(&r.search).unwrap();
        assert_eq!(first.declaration["domain_complete"], false);
        assert_eq!(first.declaration["pool"].as_array().unwrap().len(), 3);
        let mut changed = policy.clone();
        changed
            .header
            .extensions
            .insert("lazy_search_seconds".into(), json!(999));
        changed
            .header
            .extensions
            .insert("lazy_measurement_budget".into(), json!(888));
        r.search.policy = &changed;
        assert_eq!(
            first.id,
            SearchDomainManifest::from_search(&r.search).unwrap().id
        );
        r.search.pool[1].override_revision += 1;
        assert_ne!(
            first.id,
            SearchDomainManifest::from_search(&r.search).unwrap().id
        );
    }
}
