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
        let minimum = match plan.tactic_id {
            TacticId::M01 => plan.bar_frames[6],
            TacticId::M02 => plan.hold_end_frame,
            _ => plan.bar_frames[(plan.bar_frames.len() - 1).min(4)],
        };
        let ramp = (u64::from(f.output_sample_rate_hz) / 200).max(1);
        let off = first_gap(a, (minimum + ramp).min(end), end, half).ok_or_else(|| {
            Error::new(
                "TACTIC_VOCALS",
                "no measured gap after the outgoing vocal hold",
            )
        })?;
        let earliest = possible_vocals([start, off], a)
            .last()
            .map_or(start, |last| (last[1] + guard).max(start));
        let on = first_gap(b, earliest, end, half).ok_or_else(|| {
            Error::new("TACTIC_VOCALS", "no measured gap for incoming vocal entry")
        })?;
        for lane in plan
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
