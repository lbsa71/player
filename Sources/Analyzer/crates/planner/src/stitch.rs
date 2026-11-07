//! A short, ordered transition between two locally steady passages.
use super::*;

const MAX_BEAT_SKEW_SECONDS: f64 = 0.030;

pub struct StitchRequest<'a> {
    pub from: &'a str,
    pub to: &'a str,
    pub overlap_beats: usize,
    pub context_beats: usize,
}

struct Passage {
    start: u64,
    end: u64,
    context: u64,
    seconds: f64,
    beat_phases: Vec<f64>,
    downbeats: Vec<usize>,
    grid_error: f64,
}

fn passages(
    track: &Analyzed,
    overlap: usize,
    context: usize,
    outgoing: bool,
    cancel: &Cancellation,
) -> Result<Vec<Passage>> {
    let beats = &track.beats.events;
    let downbeats: BTreeSet<_> = track.beats.downbeat_ids.iter().collect();
    let rate = track.track.format.sample_rate_hz as f64;
    let mut passages = vec![];
    for (i, beat) in beats.iter().enumerate() {
        cancel.check()?;
        if !downbeats.contains(&beat.event_id) || (outgoing && i < context) {
            continue;
        }
        let j = i + overlap;
        let k = if outgoing { i - context } else { j + context };
        if j.max(k) >= beats.len() || !downbeats.contains(&beats[j].event_id) {
            continue;
        }
        let excerpt = &beats[i.min(k)..=j.max(k)];
        let start = beat.source_frame;
        let end = beats[j].source_frame;
        let seconds = (end - start) as f64 / rate;
        let pulse = seconds / overlap as f64;
        if excerpt.iter().any(|b| b.score < 0.5)
            || excerpt.windows(2).any(|w| {
                let interval = (w[1].source_frame - w[0].source_frame) as f64 / rate;
                !(0.8 * pulse..=1.2 * pulse).contains(&interval)
            })
            || track.beats.unreliable_intervals.iter().any(|r| {
                r.start_frame < excerpt.last().unwrap().source_frame
                    && r.end_frame > excerpt[0].source_frame
            })
        {
            continue;
        }
        let accents: Vec<_> = beats[i..=j]
            .iter()
            .enumerate()
            .filter_map(|(n, b)| downbeats.contains(&b.event_id).then_some(n))
            .collect();
        if accents[1] < 2 || accents.windows(2).any(|w| w[1] - w[0] != accents[1]) {
            continue;
        }
        let beat_phases: Vec<_> = beats[i..=j]
            .iter()
            .map(|b| (b.source_frame - start) as f64 / (end - start) as f64)
            .collect();
        let grid_error = beat_phases
            .iter()
            .enumerate()
            .map(|(n, phase)| (phase - n as f64 / overlap as f64).abs() * seconds)
            .fold(0., f64::max);
        passages.push(Passage {
            start,
            end,
            context: beats[k].source_frame,
            seconds,
            beat_phases,
            downbeats: accents,
            grid_error,
        });
    }
    Ok(passages)
}

pub fn stitch(
    store: &Store,
    request: &StitchRequest<'_>,
    policy: &MixingConfig,
    cancel: &Cancellation,
) -> Result<MixPlan> {
    policy.validate()?;
    require(
        (2..=64).contains(&request.overlap_beats) && (1..=64).contains(&request.context_beats),
        "STITCH_BEATS",
        "choose 2–64 overlap beats and 1–64 context beats",
    )?;
    require(
        request.from != request.to,
        "DUPLICATE_AUDIO",
        "choose two different tracks",
    )?;
    let tracks = [
        load_analyzed(store, store.track(request.from)?)?,
        load_analyzed(store, store.track(request.to)?)?,
    ];
    require(
        tracks[0].track.audio_hash != tracks[1].track.audio_hash,
        "DUPLICATE_AUDIO",
        "choose two different recordings",
    )?;
    require(
        !tracks[0].overrides.operations.iter().any(|o| {
            matches!(o, Correction::ExcludeTransition { to_track_id } if to_track_id == request.to)
        }),
        "STITCH_EXCLUDED",
        "a correction excludes this transition",
    )?;
    let outgoing = passages(
        &tracks[0],
        request.overlap_beats,
        request.context_beats,
        true,
        cancel,
    )?;
    let incoming = passages(
        &tracks[1],
        request.overlap_beats,
        request.context_beats,
        false,
        cancel,
    )?;

    // The mean duration gives the two sources equal and opposite speed changes.
    // Compare every paired beat after stretching; matching BPM alone can hide drift.
    let mut best = None;
    let mut best_cost = f64::INFINITY;
    for a in &outgoing {
        cancel.check()?;
        for b in &incoming {
            let duration = (a.seconds + b.seconds) * 0.5;
            let deviation = (a.seconds / duration - 1.).abs();
            if a.downbeats != b.downbeats
                || duration > policy.overlap_seconds
                || deviation > policy.max_speed_deviation
            {
                continue;
            }
            let skew = a
                .beat_phases
                .iter()
                .zip(&b.beat_phases)
                .map(|(a, b)| (a - b).abs() * duration)
                .fold(0., f64::max);
            if skew > MAX_BEAT_SKEW_SECONDS {
                continue;
            }
            let cost = skew + 0.5 * (a.grid_error + b.grid_error) + 0.2 * deviation;
            if cost < best_cost {
                best_cost = cost;
                best = Some((a, b, duration, skew));
            }
        }
    }
    let (a, b, duration, skew) = best.ok_or_else(|| Error::new(
        "STITCH_INFEASIBLE",
        "no reliable passages with matching downbeats, at most 30 ms beat skew, and speed changes within the configured limits",
    ))?;

    let rate = policy.output_sample_rate_hz;
    let overlap = (duration * rate as f64).round() as u64;
    let scaled = |frames, passage: &Passage| {
        (frames as f64 * overlap as f64 / (passage.end - passage.start) as f64).round() as u64
    };
    let start = scaled(a.start - a.context, a);
    let end = start + overlap;
    let total = end + scaled(b.context - b.end, b);
    let map = |i: usize, points: [(u64, u64); 3]| TimeMap {
        header: Header::default(),
        map_id: format!("stitch-{}", tracks[i].track.track_id),
        source_sample_rate_hz: tracks[i].track.format.sample_rate_hz,
        output_sample_rate_hz: rate,
        interpolation: "monotone_cubic".into(),
        anchors: points
            .into_iter()
            .map(|(source_frame, output_frame)| Anchor {
                source_frame,
                output_frame,
            })
            .collect(),
        speed_ratio_limits: [
            1. - policy.max_speed_deviation,
            1. + policy.max_speed_deviation,
        ],
        max_speed_change_per_second: policy.max_speed_change_per_second,
        preserve_pitch: true,
    };
    let mut state = State {
        used: vec![0, 1],
        placements: vec![
            placement(
                &tracks[0],
                map(0, [(a.context, 0), (a.start, start), (a.end, end)]),
            ),
            placement(
                &tracks[1],
                map(1, [(b.start, start), (b.end, end), (b.context, total)]),
            ),
        ],
        transitions: vec![Transition {
            transition_id: id(),
            from_track_id: request.from.into(),
            to_track_id: request.to.into(),
            outgoing_region_id: format!("beats-{}-{}", a.start, a.end),
            incoming_region_id: format!("beats-{}-{}", b.start, b.end),
            start_output_frame: start,
            end_output_frame: end,
            template: Template::ShortCrossfade,
            costs: BTreeMap::new(),
            fallback_reason: None,
        }],
        cost: best_cost,
        tie: 0,
    };
    for i in 0..state.placements.len() {
        state.placements[i].automation = automation_for(i, &state, &tracks, rate);
    }
    let mut policy = policy.clone();
    policy.allow_fallback = false;
    policy.template = Template::ShortCrossfade;
    let plan = MixPlan {
        header: Header::default(),
        plan_id: id(),
        planner_version: "beat-stitch/1".into(),
        renderer_version: RENDERER.into(),
        policy_hash: canonical_hash(&policy)?,
        policy,
        seed: 42,
        objective: "beat_stitch".into(),
        requested_duration_seconds: None,
        complete: true,
        output: OutputFormat {
            sample_rate_hz: rate,
            channels: 2,
            encoding: "float32".into(),
            dither: "seeded_tpdf_1_lsb".into(),
        },
        placements: state.placements,
        transitions: state.transitions,
        total_output_frames: total,
        crossover_hz: [200., 2500.],
        crossover_design: "complementary_one_pole_v1".into(),
        automation_interpolation: "smoothstep_linear_amplitude".into(),
        diagnostics: vec![
            format!(
                "{}-beat overlap at {:.2} BPM, {} context beats per side",
                request.overlap_beats,
                request.overlap_beats as f64 * 60. / duration,
                request.context_beats
            ),
            format!(
                "source speed multipliers {:.5} and {:.5}; predicted maximum beat skew {:.1} ms",
                a.seconds / duration,
                b.seconds / duration,
                skew * 1000.
            ),
        ],
    };
    plan.validate()?;
    Ok(plan)
}
