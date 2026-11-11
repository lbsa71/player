//! Apply the analyzer's leakage vetoes to audible dry stems. A projection
//! coefficient is not a measured bleed level in dB. Unknown/suspected bleed
//! matters when its source vocal is suppressed, not when the same line is
//! already playing on the same clock through that deck's vocal lane.
use crate::tactics::{curve_value, CandidateFacts};
use mixlib_types::*;

pub(crate) fn validate(plan: &mut TacticPlan, f: &CandidateFacts) -> Result<()> {
    let mut checked = false;
    for passage in [&f.a, &f.b] {
        let Some(windows) = &passage.stem_leakage else {
            continue;
        };
        for lane in plan.lanes.iter().filter(|l| {
            l.placement == passage.placement
                && matches!(l.lane, StemLane::Drums | StemLane::Bass | StemLane::Other)
        }) {
            if lane.gain.iter().all(|p| p.value == 0.) {
                continue;
            }
            checked = true;
            require(
                !plan
                    .source_edits
                    .iter()
                    .any(|e| e.placement == lane.placement && e.lane == lane.lane)
                    && !plan
                        .effects
                        .iter()
                        .any(|e| e.placement == lane.placement && e.lane == lane.lane),
                "TACTIC_STEM_LEAKAGE",
                "edited/effected stem needs capture-specific leakage evidence",
            )?;
            let voice = plan
                .lanes
                .iter()
                .find(|l| l.placement == passage.placement && l.lane == StemLane::Vocals);
            let own_voice_dry = !plan
                .source_edits
                .iter()
                .any(|e| e.placement == lane.placement && e.lane == StemLane::Vocals)
                && !plan
                    .effects
                    .iter()
                    .any(|e| e.placement == lane.placement && e.lane == StemLane::Vocals);
            let mut covered = plan.start_frame;
            for window in windows.iter().filter(|w| w.lane == lane.lane) {
                require(
                    window.start_frame <= covered && window.end_frame > window.start_frame,
                    "TACTIC_STEM_LEAKAGE",
                    "stem leakage evidence has a coverage gap",
                )?;
                covered = covered.max(window.end_frame);
                let unknown = [[window.start_frame, window.end_frame]];
                for &[a, b] in window.suspect_spans.as_deref().unwrap_or(&unknown) {
                    let a = a.max(plan.start_frame);
                    let b = b.min(plan.end_frame);
                    if a >= b {
                        continue;
                    }
                    let quiet = passage.vocal_quiet.as_deref().unwrap_or(&[]);
                    let mut points = vec![a, b];
                    points.extend(
                        lane.gain
                            .iter()
                            .map(|p| p.output_frame)
                            .filter(|p| *p > a && *p < b),
                    );
                    if let Some(voice) = voice {
                        points.extend(
                            voice
                                .gain
                                .iter()
                                .map(|p| p.output_frame)
                                .filter(|p| *p > a && *p < b),
                        );
                    }
                    points.extend(quiet.iter().flatten().copied().filter(|p| *p > a && *p < b));
                    points.sort_unstable();
                    points.dedup();
                    for w in points.windows(2) {
                        if w.iter().all(|at| curve_value(&lane.gain, *at) == 0.) {
                            continue;
                        }
                        let quiet = quiet.iter().any(|q| q[0] <= w[0] && q[1] >= w[1])
                            || passage.vocal_phrases.as_ref().is_some_and(|phrases| {
                                phrases
                                    .iter()
                                    .all(|p| p.start_frame >= w[1] || p.tail_end_frame <= w[0])
                            });
                        let same_line = own_voice_dry
                            && voice.is_some_and(|v| {
                                w.iter().all(|at| curve_value(&v.gain, *at) >= 0.999)
                            });
                        require(quiet || same_line, "TACTIC_STEM_LEAKAGE",
                            format!("audible {:?} has suspected or unassessed bleed while its own vocal is suppressed", lane.lane))?;
                    }
                }
            }
            require(
                covered >= plan.end_frame,
                "TACTIC_STEM_LEAKAGE",
                "stem leakage evidence ends before the schedule",
            )?;
        }
    }
    if checked {
        plan.evidence.push("dry-stem leakage check: mapped analyzer veto spans on audible lanes; unassessed/suspected bleed allowed only in verified quiet spans or under its own unedited fully audible vocal; projection ratios are not dB measurements".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tactics::{
        candidate_tactic_schedule, constant, tests::facts, StemLeakageWindow, TacticPolicy,
    };

    fn fixture() -> (TacticPlan, CandidateFacts) {
        let mut f = facts(8);
        let mut plan =
            candidate_tactic_schedule(TacticId::M01, &f, &TacticPolicy::default()).unwrap();
        plan.lanes.retain(|l| {
            l.placement == f.a.placement && matches!(l.lane, StemLane::Other | StemLane::Vocals)
        });
        for lane in &mut plan.lanes {
            lane.gain = constant(plan.start_frame, plan.end_frame, 1.);
        }
        f.a.vocal_phrases = None;
        f.a.stem_leakage = Some(vec![StemLeakageWindow {
            lane: StemLane::Other,
            start_frame: plan.start_frame,
            end_frame: plan.end_frame,
            suspect_spans: None,
        }]);
        (plan, f)
    }

    #[test]
    fn own_audible_line_allows_its_bleed_but_muting_the_voice_requires_isolation() {
        let (mut plan, f) = fixture();
        validate(&mut plan, &f).unwrap();
        plan.lanes
            .iter_mut()
            .find(|l| l.lane == StemLane::Vocals)
            .unwrap()
            .gain = constant(plan.start_frame, plan.end_frame, 0.);
        assert!(validate(&mut plan, &f).is_err());
        plan.lanes
            .iter_mut()
            .find(|l| l.lane == StemLane::Other)
            .unwrap()
            .gain = constant(plan.start_frame, plan.end_frame, 0.);
        validate(&mut plan, &f).unwrap();
    }

    #[test]
    fn suspect_spans_only_veto_the_audible_part_and_quiet_proof_covers_unknown() {
        let (mut plan, mut f) = fixture();
        let mid = plan.end_frame / 2;
        plan.lanes
            .iter_mut()
            .find(|l| l.lane == StemLane::Vocals)
            .unwrap()
            .gain = constant(plan.start_frame, plan.end_frame, 0.);
        f.a.stem_leakage.as_mut().unwrap()[0].suspect_spans = Some(vec![[plan.start_frame, mid]]);
        plan.lanes
            .iter_mut()
            .find(|l| l.lane == StemLane::Other)
            .unwrap()
            .gain =
            crate::tactics::transfer(plan.start_frame, plan.end_frame, mid, mid + 100, true);
        validate(&mut plan, &f).unwrap();
        f.a.stem_leakage.as_mut().unwrap()[0].suspect_spans = None;
        assert!(validate(&mut plan, &f).is_err());
        f.a.vocal_quiet = Some(vec![[mid, plan.end_frame]]);
        validate(&mut plan, &f).unwrap();
    }

    #[test]
    fn missing_leakage_coverage_cannot_claim_clean_isolation() {
        let (mut plan, mut f) = fixture();
        f.a.stem_leakage.as_mut().unwrap()[0].start_frame += 1;
        assert!(validate(&mut plan, &f).is_err());
    }
}
