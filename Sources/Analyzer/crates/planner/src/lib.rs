//! Deterministic bounded beam search over concrete, directed region transitions.
use mixlib_analysis::apply_overrides;
use mixlib_audio::RENDERER;
use mixlib_catalog::{validate_analysis, Store};
use mixlib_types::*;
use std::collections::{BTreeMap, BTreeSet};

mod phrase;
mod short_fade;
mod stem_audibility;
mod stitch;
mod tactic_cues;
mod tactic_pair_evidence;
mod tactics;
mod vocal_audibility;
pub use tactics::*;
mod tactic_evidence;
pub use tactic_evidence::{candidate_facts, CandidateEvidenceCache};
mod cached_tactic_evidence;
pub use cached_tactic_evidence::load_tactic_track_analysis;
pub use stitch::{
    automatic_cues, band_stitch, beat_return_stitch, choose_chain_bookends, describe_bar_tiles,
    drum_stitch, pick_four_tracks, pick_prepared_tracks, pick_tracks, preferred_band_stitch,
    quiet_linger_candidates, quiet_linger_stitch, stitch, tactic_stitch_candidates, BeatReturnCue,
    ChainPick, DrumHandoff, StitchRequest, CHAIN_SOLO_SECONDS,
};

pub const PLANNER: &str = "region-beam/1";
#[derive(Clone, Default)]
pub struct Request {
    pub tracks: Vec<String>,
    pub seed: u64,
    pub target_seconds: Option<f64>,
    pub fixed_start: Option<String>,
    pub fixed_end: Option<String>,
}
struct Analyzed {
    track: Track,
    analysis: AnalysisManifest,
    beats: Beats,
    regions: Regions,
    overrides: Overrides,
}
#[derive(Clone)]
struct State {
    used: Vec<usize>,
    placements: Vec<Placement>,
    transitions: Vec<Transition>,
    cost: f64,
    tie: u64,
}
#[derive(Clone)]
struct Candidate {
    outgoing_end: u64,
    incoming_start: u64,
    map: TimeMap,
    transition: Transition,
    cost: f64,
}
pub trait TransitionScorer {
    fn identity(&self) -> &str;
    fn score(
        &self,
        a: &Region,
        b: &Region,
        timing: f64,
        uncertainty: f64,
    ) -> BTreeMap<String, Cost>;
}
pub struct BaselineScorer;
impl TransitionScorer for BaselineScorer {
    fn identity(&self) -> &str {
        "chroma-energy-heuristic/1"
    }
    fn score(
        &self,
        a: &Region,
        b: &Region,
        timing: f64,
        uncertainty: f64,
    ) -> BTreeMap<String, Cost> {
        let harmonic = a
            .chroma
            .iter()
            .zip(b.chroma)
            .map(|(a, b)| (a - b).abs())
            .sum::<f64>()
            / 2.;
        let energy = (a.energy - b.energy).abs() / (a.energy.max(b.energy) + 1e-9);
        [("timing_groove",timing,2.),("harmonic_conflict",harmonic,1.),("spectral_crowding",0.5,0.5),("bass_collision",0.25,0.5),("energy_discontinuity",energy,1.),("phrase_disruption",0.5,0.5),("uncertainty",uncertainty,2.),("processing_artifacts",timing,1.)].into_iter().map(|(n,value,weight)|(n.into(),Cost{value,weight,normalization:"bounded heuristic cost 0..1, uncalibrated; spectral/phrase terms are conservative unknown priors".into()})).collect()
    }
}
fn total(costs: &BTreeMap<String, Cost>) -> f64 {
    costs.values().map(|c| c.value * c.weight).sum()
}
fn native_map(track: &Track, start: u64, output_start: u64, c: &MixingConfig) -> TimeMap {
    let end = output_start
        + ((track.format.frames - start) as f64 / track.format.sample_rate_hz as f64
            * c.output_sample_rate_hz as f64)
            .round() as u64;
    TimeMap {
        header: Header::default(),
        map_id: format!("map-{}-{output_start}", track.track_id),
        source_sample_rate_hz: track.format.sample_rate_hz,
        output_sample_rate_hz: c.output_sample_rate_hz,
        interpolation: "monotone_cubic".into(),
        anchors: vec![
            Anchor {
                source_frame: start,
                output_frame: output_start,
            },
            Anchor {
                source_frame: track.format.frames,
                output_frame: end,
            },
        ],
        speed_ratio_limits: [1. - c.max_speed_deviation, 1. + c.max_speed_deviation],
        max_speed_change_per_second: c.max_speed_change_per_second,
        preserve_pitch: true,
    }
}
fn placement(t: &Analyzed, map: TimeMap) -> Placement {
    Placement {
        track_id: t.track.track_id.clone(),
        audio_hash: t.track.audio_hash.clone(),
        audio_revision: t.track.audio_revision,
        analysis: t.track.active_analysis.clone().unwrap(),
        override_revision: t.track.override_revision,
        source_start_frame: map.anchors[0].source_frame,
        source_end_frame: map.anchors.last().unwrap().source_frame,
        time_map: map,
        automation: vec![],
    }
}
fn native_output(p: &Placement, source: u64) -> Option<u64> {
    let a = &p.time_map.anchors;
    let last = a.last()?;
    let before = &a[a.len() - 2];
    if source < before.source_frame || source > last.source_frame {
        return None;
    }
    Some(
        last.output_frame
            - ((last.source_frame - source) as f64 / p.time_map.source_sample_rate_hz as f64
                * p.time_map.output_sample_rate_hz as f64)
                .round() as u64,
    )
}
fn eligible_region(t: &Analyzed, role: RegionRole) -> Vec<&Region> {
    t.regions
        .regions
        .iter()
        .filter(|r| r.role == role)
        .collect()
}
fn candidates(
    a: &Analyzed,
    b: &Analyzed,
    placement: &Placement,
    c: &MixingConfig,
) -> Vec<Candidate> {
    if a.overrides.operations.iter().any(|o|matches!(o,Correction::ExcludeTransition{to_track_id} if to_track_id==&b.track.track_id)){return vec![];}
    let mut result = vec![];
    let rate = c.output_sample_rate_hz;
    let min_play = (c.min_played_seconds * rate as f64).ceil() as u64;
    let astart = placement.time_map.anchors[0].output_frame;
    let exits = eligible_region(a, RegionRole::Exit);
    let entries = eligible_region(b, RegionRole::Entry);
    if c.template != Template::ShortCrossfade && c.template != Template::Boundary {
        for exit in exits.iter().filter(|r| r.reliable) {
            for entry in entries.iter().filter(|r| r.reliable) {
                let ab: Vec<&Beat> = a
                    .beats
                    .events
                    .iter()
                    .filter(|x| {
                        x.source_frame >= exit.start_frame
                            && x.source_frame <= exit.end_frame
                            && x.score >= 0.5
                    })
                    .collect();
                let bb: Vec<&Beat> = b
                    .beats
                    .events
                    .iter()
                    .filter(|x| {
                        x.source_frame >= entry.start_frame
                            && x.source_frame <= entry.end_frame
                            && x.score >= 0.5
                    })
                    .collect();
                for bars in [4usize, 8, 16] {
                    let meter_a = a.beats.meter_segments.iter().find(|m| {
                        m.start_frame <= exit.start_frame + a.track.format.sample_rate_hz as u64 * 2
                            && m.end_frame
                                >= exit
                                    .end_frame
                                    .saturating_sub(a.track.format.sample_rate_hz as u64 * 2)
                    });
                    let meter_b = b.beats.meter_segments.iter().find(|m| {
                        m.start_frame
                            <= entry.start_frame + b.track.format.sample_rate_hz as u64 * 2
                            && m.end_frame
                                >= entry
                                    .end_frame
                                    .saturating_sub(b.track.format.sample_rate_hz as u64 * 2)
                    });
                    let (Some(ma), Some(mb)) = (meter_a, meter_b) else {
                        continue;
                    };
                    if ma.pulses_per_bar != mb.pulses_per_bar || ma.pulse_unit != mb.pulse_unit {
                        continue;
                    }
                    let intervals = bars * ma.pulses_per_bar as usize;
                    if ab.len() <= intervals || bb.len() <= intervals {
                        continue;
                    }
                    let aa = &ab[ab.len() - intervals - 1..];
                    let bb = &bb[..intervals + 1];
                    let Some(start) = native_output(placement, aa[0].source_frame) else {
                        continue;
                    };
                    let Some(end) = native_output(placement, aa[intervals].source_frame) else {
                        continue;
                    };
                    if end <= start
                        || end - start > (c.overlap_seconds * rate as f64) as u64
                        || start < astart + min_play
                    {
                        continue;
                    }
                    let incoming_start = bb[0].source_frame;
                    let incoming_end = bb[intervals].source_frame;
                    if a.beats.unreliable_intervals.iter().any(|x| {
                        x.start_frame < aa[intervals].source_frame
                            && x.end_frame > aa[0].source_frame
                    }) || b
                        .beats
                        .unreliable_intervals
                        .iter()
                        .any(|x| x.start_frame < incoming_end && x.end_frame > incoming_start)
                    {
                        continue;
                    }
                    let mut map = native_map(&b.track, incoming_start, start, c);
                    map.anchors = aa
                        .iter()
                        .zip(bb.iter())
                        .filter_map(|(a, b)| {
                            native_output(placement, a.source_frame).map(|out| Anchor {
                                source_frame: b.source_frame,
                                output_frame: out,
                            })
                        })
                        .collect();
                    if map.anchors.len() != aa.len() {
                        continue;
                    }
                    let last_speed = (bb[intervals].source_frame - bb[intervals - 1].source_frame)
                        as f64
                        / b.track.format.sample_rate_hz as f64
                        / ((aa[intervals].source_frame - aa[intervals - 1].source_frame) as f64
                            / a.track.format.sample_rate_hz as f64);
                    let recovery_seconds =
                        (last_speed - 1.).abs() / c.max_speed_change_per_second * 4. + 4.;
                    let recovery_out = (recovery_seconds * rate as f64).round() as u64;
                    let recovery_source = (recovery_seconds
                        * (last_speed + 1.)
                        * 0.5
                        * b.track.format.sample_rate_hz as f64)
                        .round() as u64;
                    let native_settle = b.track.format.sample_rate_hz as u64 * 2;
                    // Hold the terminal speed for one second so the cubic's boundary
                    // derivative does not pull the final beat interval abruptly toward native.
                    let hold_source =
                        (last_speed * b.track.format.sample_rate_hz as f64).round() as u64;
                    if incoming_end + hold_source + recovery_source + native_settle
                        >= b.track.format.frames
                    {
                        continue;
                    }
                    map.anchors.push(Anchor {
                        source_frame: incoming_end + hold_source,
                        output_frame: end + rate as u64,
                    });
                    map.anchors.push(Anchor {
                        source_frame: incoming_end + hold_source + recovery_source,
                        output_frame: end + rate as u64 + recovery_out,
                    });
                    map.anchors.push(Anchor {
                        source_frame: incoming_end + hold_source + recovery_source + native_settle,
                        output_frame: end + recovery_out + 3 * rate as u64,
                    });
                    let last = map.anchors.last().unwrap();
                    map.anchors.push(Anchor {
                        source_frame: b.track.format.frames,
                        output_frame: last.output_frame
                            + ((b.track.format.frames - last.source_frame) as f64
                                / b.track.format.sample_rate_hz as f64
                                * rate as f64)
                                .round() as u64,
                    });
                    if map.validate().is_err() {
                        continue;
                    }
                    let timing = map
                        .slopes()
                        .iter()
                        .map(|s| (s - 1.).abs() / c.max_speed_deviation.max(1e-6))
                        .fold(0., f64::max)
                        .min(1.);
                    let costs = BaselineScorer.score(exit, entry, timing, 0.25);
                    let cost = total(&costs);
                    let transition = Transition {
                        transition_id: format!(
                            "{}-{}-{start}-{bars}",
                            a.track.track_id, b.track.track_id
                        ),
                        from_track_id: a.track.track_id.clone(),
                        to_track_id: b.track.track_id.clone(),
                        outgoing_region_id: exit.region_id.clone(),
                        incoming_region_id: entry.region_id.clone(),
                        start_output_frame: start,
                        end_output_frame: end,
                        template: c.template.clone(),
                        costs,
                        fallback_reason: None,
                    };
                    result.push(Candidate {
                        outgoing_end: aa[intervals].source_frame,
                        incoming_start,
                        map,
                        transition,
                        cost,
                    });
                }
            }
        }
    }
    if c.allow_fallback
        || c.template == Template::ShortCrossfade
        || c.template == Template::Boundary
    {
        if let (Some(exit), Some(entry)) = (exits.last(), entries.first()) {
            let outgoing_end = exit.end_frame.min(placement.source_end_frame);
            let Some(end) = native_output(placement, outgoing_end) else {
                return result;
            };
            let incoming_start = entry.start_frame;
            let bframes = ((b.track.format.frames - incoming_start) as f64
                / b.track.format.sample_rate_hz as f64
                * rate as f64)
                .round() as u64;
            let duration = end - astart;
            let overlap = if c.template == Template::Boundary {
                0
            } else {
                (2. * rate as f64)
                    .min(c.overlap_seconds * rate as f64)
                    .min(duration as f64 / 4.)
                    .min(bframes as f64 / 4.)
                    .min(
                        (exit.end_frame - exit.start_frame) as f64
                            / a.track.format.sample_rate_hz as f64
                            * rate as f64,
                    )
                    .min(
                        (entry.end_frame - entry.start_frame) as f64
                            / b.track.format.sample_rate_hz as f64
                            * rate as f64,
                    ) as u64
            };
            if duration >= min_play + overlap && bframes >= min_play + overlap {
                let start = end - overlap;
                if native_output(
                    placement,
                    outgoing_end.saturating_sub(
                        (overlap as f64 / rate as f64 * a.track.format.sample_rate_hz as f64)
                            as u64,
                    ),
                )
                .is_some()
                {
                    let map = native_map(&b.track, incoming_start, start, c);
                    let costs = BaselineScorer.score(exit, entry, 0., 1.);
                    let cost = total(&costs) + 2.;
                    let fallback = if c.template == Template::ShortCrossfade
                        || c.template == Template::Boundary
                    {
                        None
                    } else {
                        Some("no accepted beat/meter/time-map candidate; explicit short crossfade fallback".into())
                    };
                    result.push(Candidate {
                        outgoing_end,
                        incoming_start,
                        map,
                        transition: Transition {
                            transition_id: format!(
                                "{}-{}-fade",
                                a.track.track_id, b.track.track_id
                            ),
                            from_track_id: a.track.track_id.clone(),
                            to_track_id: b.track.track_id.clone(),
                            outgoing_region_id: exit.region_id.clone(),
                            incoming_region_id: entry.region_id.clone(),
                            start_output_frame: start,
                            end_output_frame: end,
                            template: if overlap == 0 {
                                Template::Boundary
                            } else {
                                Template::ShortCrossfade
                            },
                            costs,
                            fallback_reason: fallback,
                        },
                        cost,
                    });
                }
            }
        }
    }
    result.sort_by(|a, b| {
        a.cost
            .total_cmp(&b.cost)
            .then(a.transition.transition_id.cmp(&b.transition.transition_id))
    });
    result.truncate(3);
    result
}
fn gain(t: &Analyzed) -> f64 {
    (0.08 / t.analysis.rms.max(1e-6))
        .min(0.35 / t.analysis.peak.max(1e-6))
        .min(2.)
}
fn automation_for(i: usize, s: &State, track: &Analyzed, rate: u32) -> Vec<AutomationPoint> {
    let p = &s.placements[i];
    let start = p.time_map.anchors[0].output_frame;
    let end = p.time_map.anchors.last().unwrap().output_frame;
    let g = gain(track);
    let mut points: BTreeMap<u64, AutomationPoint> = BTreeMap::new();
    let mut add = |frame: u64, scale: f64, bands: [f64; 3]| {
        points.insert(
            frame,
            AutomationPoint {
                output_frame: frame,
                gain_linear: g * scale,
                bands_linear: bands,
                mute: scale == 0.,
            },
        );
    };
    add(start, 1., [1.; 3]);
    add(end, 1., [1.; 3]);
    if i == 0 || s.transitions[i - 1].template == Template::Boundary {
        add(start, 0., [1.; 3]);
        add((start + rate as u64 / 100).min(end - 1), 1., [1.; 3]);
    } else {
        let t = &s.transitions[i - 1];
        let mid = t.start_output_frame + (t.end_output_frame - t.start_output_frame) / 2;
        add(start, 0., [1.; 3]);
        match t.template {
            Template::BassHandoff => {
                add(start, 0., [0., 1., 1.]);
                add(mid, 0.5, [0.5, 1., 1.]);
            }
            Template::ThreeBandExchange => {
                add(start, 0., [0., 0., 1.]);
                add(mid, 0.5, [0.5, 0.5, 1.]);
            }
            _ => {}
        }
        add(t.end_output_frame, 1., [1.; 3]);
    }
    if i == s.placements.len() - 1 || s.transitions[i].template == Template::Boundary {
        add(
            end.saturating_sub(rate as u64 / 100).max(start + 1),
            1.,
            [1.; 3],
        );
        add(end, 0., [1.; 3]);
    } else {
        let t = &s.transitions[i];
        let mid = t.start_output_frame + (t.end_output_frame - t.start_output_frame) / 2;
        add(t.start_output_frame, 1., [1.; 3]);
        match t.template {
            Template::BassHandoff => add(mid, 0.5, [0.5, 1., 1.]),
            Template::ThreeBandExchange => add(mid, 0.5, [0.5, 0.5, 0.]),
            _ => {}
        }
        add(end, 0., [1.; 3]);
    }
    points.into_values().collect()
}

fn load_analyzed(store: &Store, track: Track) -> Result<Analyzed> {
    store.source(&track)?;
    let analysis = store.analysis(&track)?;
    validate_analysis(store, &analysis)?;
    let mut beats = store.read_ref(&analysis.artifacts["beats"])?;
    let mut regions = store.read_ref(&analysis.artifacts["regions"])?;
    let overrides = store.overrides(&track)?;
    apply_overrides(
        &track,
        track.active_analysis.as_ref().unwrap(),
        &mut beats,
        &mut regions,
        &overrides,
    )?;
    Ok(Analyzed {
        track,
        analysis,
        beats,
        regions,
        overrides,
    })
}

pub fn plan(
    store: &Store,
    request: &Request,
    policy: &MixingConfig,
    cancel: &Cancellation,
) -> Result<MixPlan> {
    policy.validate()?;
    if let Some(target) = request.target_seconds {
        require(
            target.is_finite() && target > 0.,
            "DURATION",
            "target duration must be positive",
        )?;
    }
    let all = store.tracks()?;
    let selected: BTreeSet<&String> = request.tracks.iter().collect();
    for wanted in &selected {
        require(
            all.iter().any(|t| &t.track_id == *wanted),
            "TRACK_MISSING",
            *wanted,
        )?;
    }
    let mut tracks = vec![];
    let mut diagnostics = vec![];
    for t in all {
        if !selected.is_empty() && !selected.contains(&t.track_id) {
            continue;
        }
        if !matches!(t.lifecycle, Lifecycle::Available) {
            if selected.contains(&t.track_id) {
                return Err(Error::new(
                    "PLAN_INFEASIBLE",
                    format!("required track {} is unavailable", t.track_id),
                ));
            }
            diagnostics.push(format!("{} unavailable, outside eligible pool", t.track_id));
            continue;
        }
        let analyzed = load_analyzed(store, t)?;
        require(
            !tracks
                .iter()
                .any(|a: &Analyzed| a.track.audio_hash == analyzed.track.audio_hash),
            "DUPLICATE_AUDIO",
            "selected pool contains duplicate audio identities",
        )?;
        tracks.push(analyzed);
    }
    require(
        !tracks.is_empty(),
        "PLAN_EMPTY",
        "no analyzed available tracks; scan and analyze first",
    )?;
    require(
        tracks.len() <= 10000,
        "POOL_LIMIT",
        "pool exceeds 10000 tracks",
    )?;
    for fixed in [&request.fixed_start, &request.fixed_end]
        .into_iter()
        .flatten()
    {
        require(
            tracks.iter().any(|t| &t.track.track_id == fixed),
            "FIXED_TRACK",
            "fixed track is outside eligible pool",
        )?;
    }
    let mut beam = vec![];
    for (i, t) in tracks.iter().enumerate() {
        if request
            .fixed_start
            .as_ref()
            .is_some_and(|s| s != &t.track.track_id)
        {
            continue;
        }
        if tracks.len() > 1 && request.fixed_end.as_ref() == Some(&t.track.track_id) {
            continue;
        }
        let map = native_map(&t.track, 0, 0, policy);
        beam.push(State {
            used: vec![i],
            placements: vec![placement(t, map)],
            transitions: vec![],
            cost: 0.,
            tie: tie(request.seed, &[i]),
        });
    }
    let target = request
        .target_seconds
        .map(|s| s * policy.output_sample_rate_hz as f64);
    let objective = |s: &State| {
        let duration = s
            .placements
            .last()
            .unwrap()
            .time_map
            .anchors
            .last()
            .unwrap()
            .output_frame as f64;
        target.map_or(s.cost, |t| {
            (duration - t).abs() / policy.output_sample_rate_hz as f64 + s.cost * 0.1
        })
    };
    let mut best: Option<State> = None;
    let mut expansions = 0;
    let mut exhausted = false;
    for _depth in 1..=tracks.len() {
        cancel.check()?;
        for state in &beam {
            let allowed_end = request
                .fixed_end
                .as_ref()
                .is_none_or(|id| id == &tracks[*state.used.last().unwrap()].track.track_id);
            if allowed_end
                && (target.is_some() || state.used.len() == tracks.len())
                && best.as_ref().is_none_or(|b| {
                    objective(state) < objective(b)
                        || (objective(state) == objective(b) && state.tie < b.tie)
                })
            {
                best = Some(state.clone());
            }
        }
        let mut next = vec![];
        for state in &beam {
            let a = *state.used.last().unwrap();
            let current = state.placements.last().unwrap();
            if request.fixed_end.as_ref() == Some(&tracks[a].track.track_id) {
                continue;
            }
            for b in 0..tracks.len() {
                if state.used.contains(&b) {
                    continue;
                }
                if target.is_none()
                    && request.fixed_end.as_ref() == Some(&tracks[b].track.track_id)
                    && state.used.len() + 1 != tracks.len()
                {
                    continue;
                }
                expansions += 1;
                if expansions > policy.max_expansions {
                    exhausted = true;
                    break;
                }
                for candidate in candidates(&tracks[a], &tracks[b], current, policy) {
                    let mut s = state.clone();
                    let last = s.placements.last_mut().unwrap();
                    last.source_end_frame = candidate.outgoing_end;
                    let anchor = last.time_map.anchors.last_mut().unwrap();
                    anchor.source_frame = candidate.outgoing_end;
                    anchor.output_frame = candidate.transition.end_output_frame;
                    if last.time_map.validate().is_err() {
                        continue;
                    }
                    let mut incoming = placement(&tracks[b], candidate.map);
                    incoming.source_start_frame = candidate.incoming_start;
                    s.placements.push(incoming);
                    s.transitions.push(candidate.transition);
                    s.used.push(b);
                    s.cost += candidate.cost;
                    s.tie = tie(request.seed, &s.used);
                    next.push(s);
                }
            }
            if exhausted {
                break;
            }
        }
        if next.is_empty() || exhausted {
            break;
        }
        next.sort_by(|a, b| {
            objective(a)
                .total_cmp(&objective(b))
                .then(a.tie.cmp(&b.tie))
        });
        next.truncate(policy.beam_width);
        beam = next;
    }
    let mut best=best.ok_or_else(||Error::new("PLAN_INFEASIBLE",format!("no complete sequence within constraints/budget; blocking pool: {}; permit fades, shorten minimum play duration or increase search budget",tracks.iter().map(|t|t.track.track_id.as_str()).collect::<Vec<_>>().join(", "))))?;
    if exhausted {
        diagnostics.push("search budget exhausted; best feasible result returned".into());
    }
    for i in 0..best.placements.len() {
        best.placements[i].automation = automation_for(
            i,
            &best,
            &tracks[best.used[i]],
            policy.output_sample_rate_hz,
        );
    }
    diagnostics.extend(best.transitions.iter().filter_map(|t| {
        t.fallback_reason
            .as_ref()
            .map(|s| format!("{} → {}: {s}", t.from_track_id, t.to_track_id))
    }));
    diagnostics.push(format!("{expansions} directed expansions; beam width {}; costs are unvalidated heuristics; vocal activity unknown",policy.beam_width));
    let duration = best
        .placements
        .last()
        .unwrap()
        .time_map
        .anchors
        .last()
        .unwrap()
        .output_frame;
    if let Some(target) = request.target_seconds {
        diagnostics.push(format!("requested {target:.3}s; feasible subset is {:.3}s; target is a nearest-duration objective",duration as f64/policy.output_sample_rate_hz as f64));
    }
    let plan = MixPlan {
        header: Header::default(),
        plan_id: id(),
        planner_version: PLANNER.into(),
        renderer_version: RENDERER.into(),
        policy_hash: canonical_hash(policy)?,
        policy: policy.clone(),
        seed: request.seed,
        objective: if target.is_some() {
            "target_duration"
        } else {
            "order_all"
        }
        .into(),
        requested_duration_seconds: request.target_seconds,
        complete: true,
        output: OutputFormat {
            sample_rate_hz: policy.output_sample_rate_hz,
            channels: 2,
            encoding: "float32".into(),
            dither: "seeded_tpdf_1_lsb".into(),
        },
        drum_intro: None,
        prepared_patches: vec![],
        placements: best.placements,
        transitions: best.transitions,
        total_output_frames: duration,
        crossover_hz: [200., 2500.],
        crossover_design: "complementary_one_pole_v1".into(),
        automation_interpolation: "smoothstep_linear_amplitude".into(),
        diagnostics,
    };
    plan.validate()?;
    Ok(plan)
}
fn tie(seed: u64, sequence: &[usize]) -> u64 {
    let mut x = seed ^ 0xcbf29ce484222325;
    for i in sequence {
        x ^= *i as u64;
        x = x.wrapping_mul(0x100000001b3);
    }
    x
}
