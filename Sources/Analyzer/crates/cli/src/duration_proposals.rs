//! Ranked cue requests and their proposal cache.
use super::*;

impl Search<'_> {
    pub(super) fn proposals(
        &mut self,
        from: usize,
        to: usize,
        after: u64,
        limit: usize,
        cancel: &Cancellation,
    ) -> Result<Vec<MixPlan>> {
        if !self.schedules_transition(from, to) {
            return Ok(vec![]);
        }
        let key = self.cache.pair_key(
            &self.pool[from].track_id,
            &self.pool[to].track_id,
            &json!({"after":after,"context":self.context,"limit":limit,"baseline_generator":if limit==1 {2}else{0}}),
        )?;
        if let Some(mut plans) = self
            .cache
            .read::<Vec<MixPlan>>(self.store, "proposals", &key)?
        {
            for p in &mut plans {
                crate::search_cache::rebind(p, self.policy)?;
            }
            self.stats.cached_proposals += 1;
            return Ok(plans);
        }
        let request = StitchRequest {
            from: &self.pool[from].track_id,
            to: &self.pool[to].track_id,
            overlap_beats: 8,
            context_beats: self.context,
            outgoing_after_frame: after,
        };
        let plans = if limit == 1 {
            self.baseline_cues
                .candidates(&self.snapshot, &request, self.policy, 4, cancel)?
        } else if limit == 0 {
            self.event_cues
                .candidates(&self.snapshot, &request, self.policy, 2, cancel)?
        } else {
            match mixlib_planner::tactic_stitch_candidates(
                &self.snapshot,
                &request,
                self.policy,
                limit,
                cancel,
            ) {
                Ok(plans) => plans,
                Err(e) if matches!(e.code.as_str(), "STITCH_INFEASIBLE" | "STITCH_EXCLUDED") => {
                    vec![]
                }
                Err(e) => return Err(e),
            }
        };
        self.stats.proposed_pairs += 1;
        self.stats.proposed_windows += plans.len();
        self.cache.write(self.store, "proposals", &key, &plans)?;
        Ok(plans)
    }
}
