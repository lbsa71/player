//! Validate dry vocal ownership from the schedule and affirmative quiet spans.
//! An unclassified line can play through a join on one deck. It cannot be
//! cut through, started halfway, or mixed with another possible vocal.
use crate::tactics::{constant, transfer, CandidateFacts};
use mixlib_types::*;

fn quiet_at(spans: &[[u64; 2]], frame: u64, half: u64) -> bool {
    spans
        .iter()
        .any(|&[a, b]| a <= frame.saturating_sub(half) && b >= frame + half)
}

pub(crate) fn quiet_landing(f: &CandidateFacts, frame: u64) -> bool {
    let half = (u64::from(f.output_sample_rate_hz) * 15).div_ceil(100);
    [&f.a, &f.b].iter().all(|p| {
        p.vocal_quiet
            .as_ref()
            .is_some_and(|q| quiet_at(q, frame, half))
    })
}

fn audible(lane: &LaneSchedule) -> Vec<[u64; 2]> {
    let mut ranges: Vec<[u64; 2]> = vec![];
    for points in lane.gain.windows(2) {
        if points.iter().any(|point| point.value > 0.) {
            let [a, b] = [points[0].output_frame, points[1].output_frame];
            if let Some(last) = ranges.last_mut().filter(|last| last[1] == a) {
                last[1] = b;
            } else {
                ranges.push([a, b]);
            }
        }
    }
    ranges
}

fn possible_vocals([start, end]: [u64; 2], quiet: &[[u64; 2]]) -> Vec<[u64; 2]> {
    let mut spans = vec![];
    let mut cursor = start;
    for &[a, b] in quiet {
        if a >= end {
            break;
        }
        if a > cursor {
            spans.push([cursor, a]);
        }
        cursor = cursor.max(b);
    }
    if cursor < end {
        spans.push([cursor, end]);
    }
    spans
}

fn first_gap(quiet: &[[u64; 2]], after: u64, end: u64, half: u64) -> Option<u64> {
    quiet.iter().find_map(|&[a, b]| {
        let at = after.max(a + half);
        (at <= end && at + half <= b).then_some(at)
    })
}

pub(crate) fn apply(plan: &mut TacticPlan, f: &CandidateFacts) -> Result<bool> {
    let [Some(a), Some(b)] = [&f.a.vocal_quiet, &f.b.vocal_quiet] else {
        return Ok(false);
    };
    let vocal_lane = |lane| matches!(lane, StemLane::Stereo | StemLane::Vocals);
    if plan.permission == TacticPermission::EmergencyOnly
        || matches!(
            plan.tactic_id,
            TacticId::M03 | TacticId::M04 | TacticId::M06 | TacticId::C08 | TacticId::C11
        )
        || plan.source_edits.iter().any(|e| vocal_lane(e.lane))
        || plan
            .effects
            .iter()
            .any(|fx| vocal_lane(fx.lane) && !matches!(fx.kind, EffectKind::Filter { .. }))
    {
        // Captured or retimed lines require positive phrase and tail evidence.
        return Ok(false);
    }
    let start = plan.start_frame;
    let end = plan.end_frame;
    let guard = plan.vocal_guard_frames;
    let half = guard.div_ceil(2);
    let stem_mode = plan.lanes.iter().any(|lane| lane.lane == StemLane::Vocals);
    if stem_mode && !matches!(plan.tactic_id, TacticId::C13 | TacticId::C14) {
        // Preserve complete transcript-backed schedules when available.
        if f.a.vocal_phrases.is_some() && f.b.vocal_phrases.is_some() {
            return Ok(false);
        }
        let minimum = if plan.phrase_exit.is_some() {
            plan.hold_end_frame
        } else {
            match plan.tactic_id {
                TacticId::M01 | TacticId::M02 => plan.hold_end_frame,
                _ => plan.bar_frames[(plan.bar_frames.len() - 1).min(4)],
            }
        };
        let ramp = (u64::from(f.output_sample_rate_hz) / 200).max(1);
        let mut alternatives = vec![];
        for &[a, b] in a {
            let first = (minimum + ramp).min(end).max(a + half);
            let last = plan
                .phrase_exit
                .as_ref()
                .map_or(end, |e| e.end_frame)
                .min(b.saturating_sub(half));
            if first <= last {
                alternatives.push(first);
                if first != last {
                    alternatives.push(last);
                }
            }
        }
        let mut failure = Error::new(
            "TACTIC_VOCALS",
            "no measured gap after the outgoing vocal hold",
        );
        let mut accepted = false;
        for off in alternatives {
            let earliest = possible_vocals([start, off], a)
                .last()
                .map_or(start, |last| (last[1] + guard).max(start));
            let Some(on) = first_gap(b, earliest, end, half) else {
                failure = Error::new("TACTIC_VOCALS", "no measured gap for incoming vocal entry");
                continue;
            };
            let mut trial = plan.clone();
            for lane in trial
                .lanes
                .iter_mut()
                .filter(|lane| lane.lane == StemLane::Vocals)
            {
                lane.gain = if lane.placement == f.a.placement {
                    transfer(
                        start,
                        end,
                        off.saturating_sub(ramp).max(minimum),
                        off,
                        false,
                    )
                } else if on == end {
                    constant(start, end, 0.)
                } else {
                    transfer(start, end, on, (on + ramp).min(end), true)
                };
            }
            // A silent pocket is not necessarily the end of vocal ownership:
            // retained accompaniment can carry the next line's bleed. Try a
            // later supported pocket before discarding the entire long hold.
            match crate::stem_audibility::validate(&mut trial, f) {
                Ok(()) => {
                    plan.lanes = trial.lanes;
                    accepted = true;
                    break;
                }
                Err(error) => failure = error,
            }
        }
        if !accepted {
            return Err(failure);
        }
    }
    let mut occupancy: Vec<VocalOccupancy> = vec![];
    for (passage, quiet, outgoing) in [(&f.a, a, true), (&f.b, b, false)] {
        let lane = plan
            .lanes
            .iter()
            .find(|lane| lane.placement == passage.placement && vocal_lane(lane.lane));
        let mut ranges = lane.map(audible).unwrap_or_default();
        let mut gates = vec![];
        if outgoing && ranges.first().is_none_or(|r| r[0] != start) {
            gates.push(start);
        }
        if !outgoing && ranges.last().is_none_or(|r| r[1] != end) {
            gates.push(end);
        }
        for &[a, b] in &ranges {
            if !outgoing || a > start {
                gates.push(a);
            }
            if outgoing || b < end {
                gates.push(b);
            }
        }
        let continuation = if outgoing {
            [
                start
                    .saturating_sub(guard)
                    .max(passage.source.output_start_frame),
                start,
            ]
        } else {
            [end, (end + guard).min(passage.source.output_end_frame)]
        };
        if outgoing {
            ranges.insert(0, continuation);
        } else {
            ranges.push(continuation);
        }
        for range in ranges.into_iter().filter(|[a, b]| a < b) {
            for [a, b] in possible_vocals(range, quiet) {
                if let Some(previous) = occupancy
                    .last_mut()
                    .filter(|v| v.placement == passage.placement && v.end_frame == a)
                {
                    previous.end_frame = b;
                    previous.tail_end_frame = b;
                    continue;
                }
                occupancy.push(VocalOccupancy {
                    placement: passage.placement,
                    start_frame: a,
                    end_frame: b,
                    tail_end_frame: b,
                    supported_entry: true,
                    supported_exit: true,
                });
            }
        }
        require(
            gates.into_iter().all(|at| quiet_at(quiet, at, half)),
            "TACTIC_VOCALS",
            "a vocal gate or cut has no verified between-lines gap",
        )?;
    }
    require(
        occupancy.iter().all(|a| {
            occupancy.iter().all(|b| {
                a.placement == b.placement
                    || a.start_frame > b.start_frame
                    || a.tail_end_frame + guard <= b.start_frame
            })
        }),
        "TACTIC_VOCALS",
        "potential vocals overlap on audible lanes",
    )?;
    plan.vocal_occupancy = occupancy;
    plan.evidence.push("audible-lane vocal check: PCM-verified gaps at gates, conservative possible activity elsewhere; no lyric transcription inferred".into());
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tactics::{
        candidate_tactic_schedule, tests::facts, StemLeakageWindow, TacticPolicy,
    };

    #[test]
    fn baseline_checks_actual_voice_entry_and_exit_not_continuing_boundaries() {
        let mut f = facts(4);
        let bar = f.bar_frames[1];
        let end = *f.bar_frames.last().unwrap();
        let source_end = f.a.source.output_end_frame;
        f.a.vocal_phrases = None;
        f.b.vocal_phrases = None;
        // A's line is already playing when the fade starts. B's new line
        // continues after it ends. Only B's entry and A's exit are gates.
        f.a.vocal_quiet = Some(vec![[bar, source_end]]);
        f.b.vocal_quiet = Some(vec![[0, 3 * bar]]);
        let plan = candidate_tactic_schedule(TacticId::S13, &f, &TacticPolicy::default())
            .expect("continuing vocals need no artificial gap at A start or B end");
        assert!(plan
            .vocal_occupancy
            .iter()
            .any(|v| { v.placement == f.a.placement && v.start_frame == plan.start_frame }));
        assert!(plan
            .vocal_occupancy
            .iter()
            .any(|v| { v.placement == f.b.placement && v.end_frame > plan.end_frame }));

        let mut missing_exit = f.clone();
        missing_exit.a.vocal_quiet = Some(vec![[bar, end - 1]]);
        missing_exit.b.vocal_quiet = Some(vec![[0, source_end]]);
        let error =
            candidate_tactic_schedule(TacticId::S13, &missing_exit, &TacticPolicy::default())
                .unwrap_err();
        assert_eq!(error.code, "TACTIC_VOCALS");
        assert!(error.message.contains("between-lines gap"));

        let mut missing_entry = f.clone();
        missing_entry.a.vocal_quiet = Some(vec![[0, source_end]]);
        missing_entry.b.vocal_quiet = Some(vec![[bar / 2, 3 * bar]]);
        let error =
            candidate_tactic_schedule(TacticId::S13, &missing_entry, &TacticPolicy::default())
                .unwrap_err();
        assert_eq!(error.code, "TACTIC_VOCALS");
        assert!(error.message.contains("between-lines gap"));

        // Quiet gates alone do not authorize overlapping lines in the body.
        let mut collision = f;
        collision.a.vocal_quiet = Some(vec![[3 * bar, source_end]]);
        collision.b.vocal_quiet = Some(vec![[0, 2 * bar]]);
        let error = candidate_tactic_schedule(TacticId::S13, &collision, &TacticPolicy::default())
            .unwrap_err();
        assert_eq!(error.code, "TACTIC_VOCALS");
        assert!(error.message.contains("potential vocals overlap"));
    }

    #[test]
    fn later_vocal_pocket_preserves_a_long_hold_with_audible_bleed() {
        let mut f = facts(8);
        let mut plan =
            candidate_tactic_schedule(TacticId::M01, &f, &TacticPolicy::default()).unwrap();
        let bar = plan.bar_frames[1];
        f.a.vocal_phrases = None;
        f.b.vocal_phrases = None;
        f.a.vocal_quiet = Some(vec![[0, bar * 6 + bar / 3], [bar * 7, bar * 9]]);
        f.b.vocal_quiet = Some(vec![[0, bar * 9]]);
        f.a.stem_leakage = Some(
            [StemLane::Bass, StemLane::Other]
                .into_iter()
                .map(|lane| StemLeakageWindow {
                    lane,
                    start_frame: 0,
                    end_frame: plan.end_frame,
                    suspect_spans: None,
                })
                .collect(),
        );
        let exit = plan.exit_start_frame;
        apply(&mut plan, &f).unwrap();
        crate::stem_audibility::validate(&mut plan, &f).unwrap();
        let voice = plan
            .lanes
            .iter()
            .find(|l| l.placement == f.a.placement && l.lane == StemLane::Vocals)
            .unwrap();
        assert_eq!(crate::tactics::curve_value(&voice.gain, bar * 7), 1.);
        assert_eq!(
            plan.exit_start_frame, exit,
            "vocal pocket does not move the accompaniment exit"
        );
        f.a.vocal_quiet = Some(vec![[0, bar * 6 + bar / 3]]);
        assert!(
            apply(&mut plan, &f).is_err(),
            "missing final safe pocket remains a rejection"
        );
    }
}
