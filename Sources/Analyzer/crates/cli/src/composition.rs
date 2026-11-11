//! Set-level policy over admitted transitions. This layer never relaxes tactic
//! safety or edits audio to hit a clock: it chooses different source passages.
use super::*;
use crate::{measured_beam::route_quality, tactics::MeasuredEdge};
use mixlib_planner::TrackPacing;
use serde_json::{json, Value};
use std::{collections::BTreeMap, time::Instant};

#[derive(Clone, Copy, Debug)]
pub(super) struct Request {
    pub minutes: u64,
    pub shape: Shape,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Shape {
    Story,
    Wave,
    Ramp,
    Chapters,
}
impl Shape {
    fn name(self) -> &'static str {
        match self {
            Self::Story => "story",
            Self::Wave => "wave",
            Self::Ramp => "ramp",
            Self::Chapters => "chapters",
        }
    }
    fn knots(self) -> &'static [(f64, f64)] {
        match self {
            Self::Story => &[
                (0., 0.3),
                (0.18, 0.5),
                (0.32, 0.7),
                (0.43, 0.4),
                (0.62, 0.72),
                (0.82, 0.92),
                (0.91, 0.7),
                (1., 0.4),
            ],
            Self::Wave => &[
                (0., 0.4),
                (0.22, 0.8),
                (0.35, 0.42),
                (0.55, 0.85),
                (0.68, 0.45),
                (0.87, 0.9),
                (1., 0.5),
            ],
            Self::Ramp => &[(0., 0.3), (0.3, 0.45), (0.6, 0.65), (1., 0.9)],
            Self::Chapters => &[
                (0., 0.3),
                (0.18, 0.75),
                (0.28, 0.4),
                (0.47, 0.85),
                (0.57, 0.42),
                (0.8, 0.95),
                (0.9, 0.7),
                (1., 0.4),
            ],
        }
    }
    fn target(self, progress: f64) -> f64 {
        let p = progress.clamp(0., 1.);
        let knots = self.knots();
        let w = knots.windows(2).find(|w| p <= w[1].0).unwrap();
        w[0].1 + (w[1].1 - w[0].1) * (p - w[0].0) / (w[1].0 - w[0].0)
    }
}
impl Request {
    pub fn new(minutes: u64, shape: &str) -> Result<Self> {
        require(
            matches!(minutes, 60 | 120),
            "SET_DURATION",
            "sets must target 57–60 or 117–120 minutes",
        )?;
        let shape = match shape {
            "auto" if minutes == 120 => Shape::Chapters,
            "auto" | "story" => Shape::Story,
            "wave" => Shape::Wave,
            "ramp" => Shape::Ramp,
            "chapters" => Shape::Chapters,
            _ => return Err(Error::new("SET_STRUCTURE", "unknown set structure")),
        };
        Ok(Self { minutes, shape })
    }
    pub fn from_policy(policy: &MixingConfig) -> Result<Option<Self>> {
        policy
            .header
            .extensions
            .get("set_minutes")
            .map(|m| {
                let minutes = m
                    .as_u64()
                    .ok_or_else(|| Error::new("SET_DURATION", "set_minutes must be 60 or 120"))?;
                Self::new(
                    minutes,
                    policy
                        .header
                        .extensions
                        .get("set_structure")
                        .and_then(Value::as_str)
                        .unwrap_or("auto"),
                )
            })
            .transpose()
    }
    pub fn capacity_possible(self, source_seconds: f64, minimum_speed: f64) -> bool {
        source_seconds / minimum_speed.max(0.01) >= self.min()
    }
    fn min(self) -> f64 {
        (self.minutes - 3) as f64 * 60.
    }
    fn max(self) -> f64 {
        self.minutes as f64 * 60.
    }
    fn accepts(self, seconds: f64) -> bool {
        (self.min()..=self.max()).contains(&seconds)
    }
}
struct Profile {
    tiles: Vec<SegmentTile>,
    duration: f64,
    rate: f64,
    opening: mixlib_planner::ChainOpening,
}
pub(super) struct Context {
    request: Request,
    profiles: Vec<Profile>,
    ids: BTreeMap<String, usize>,
}
#[derive(Default)]
struct Passage {
    seconds: f64,
    error: f64,
    preferred: f64,
    minimum: f64,
    energy: f64,
    development: f64,
}
impl Context {
    pub fn load(
        store: &Store,
        pool: &[Track],
        request: Request,
        cancel: &Cancellation,
    ) -> Result<Self> {
        let mut profiles = vec![];
        for t in pool {
            let a = store.analysis(t)?;
            let reference = a
                .artifacts
                .get("segments")
                .ok_or_else(|| Error::new("SET_ANALYSIS", "segment analysis required"))?;
            let segments: SegmentAnalysis = store.read_ref(reference)?;
            profiles.push(Profile {
                tiles: segments.tiles.into_iter().filter(|t| t.bars == 1).collect(),
                duration: t.format.frames as f64 / f64::from(t.format.sample_rate_hz),
                rate: f64::from(t.format.sample_rate_hz),
                opening: mixlib_planner::read_chain_opening_cue(store, &t.track_id, cancel)?,
            });
        }
        // A library-relative acoustic intensity proxy, not emotion recognition.
        // Normalize across the whole pool, never separately within each song.
        let mut values: Vec<f64> = profiles
            .iter()
            .flat_map(|p| p.tiles.iter().map(intensity))
            .collect();
        values.sort_by(f64::total_cmp);
        require(
            !values.is_empty(),
            "SET_ANALYSIS",
            "no measured single-bar passages",
        )?;
        for profile in &mut profiles {
            for tile in &mut profile.tiles {
                let raw = intensity(tile);
                let lower = values.partition_point(|v| *v < raw);
                let upper = values.partition_point(|v| *v <= raw);
                tile.roles.insert(
                    "set_intensity_percentile".into(),
                    (lower + upper) as f64 / (2. * values.len() as f64),
                );
            }
        }
        Ok(Self {
            request,
            profiles,
            ids: pool
                .iter()
                .enumerate()
                .map(|(i, t)| (t.track_id.clone(), i))
                .collect(),
        })
    }
    fn passage(
        &self,
        index: usize,
        start: u64,
        end: u64,
        output_start: f64,
        output_seconds: f64,
    ) -> Passage {
        let p = &self.profiles[index];
        let mut result = Passage {
            seconds: output_seconds,
            ..Passage::default()
        };
        let mut weight = 0.;
        for t in &p.tiles {
            let overlap = t
                .end_frame
                .min(end)
                .saturating_sub(t.start_frame.max(start)) as f64;
            if overlap <= 0. {
                continue;
            }
            let fraction = ((t.start_frame.max(start) + t.end_frame.min(end)) as f64 / 2.
                - start as f64)
                / (end - start).max(1) as f64;
            let energy = t.roles["set_intensity_percentile"];
            result.error += overlap
                * (energy
                    - self
                        .request
                        .shape
                        .target((output_start + fraction * output_seconds) / self.request.max()))
                .abs();
            result.energy += overlap * energy;
            result.development += overlap
                * (t.roles
                    .get("sustained_texture")
                    .copied()
                    .unwrap_or(0.)
                    .max(t.roles.get("build").copied().unwrap_or(0.)))
                .max(t.harmonic_variation.min(1.));
            weight += overlap;
        }
        if weight > 0. {
            result.error /= weight;
            result.energy /= weight;
            result.development /= weight;
        } else {
            result.error = 0.5;
            result.energy = 0.5;
        }
        // Development-rich songs get a stronger preference for a longer stay.
        // Short recordings still get to contribute a substantial fraction.
        result.minimum = (60. + 30. * result.development).min(p.duration * 0.45);
        result.preferred = (150. + 120. * result.development).min(p.duration * 0.8);
        result
    }
    fn evaluate(&self, plan: &MixPlan, complete: bool) -> Option<Value> {
        let rate = f64::from(plan.output.sample_rate_hz);
        let duration = plan.total_output_frames as f64 / rate;
        if complete && !self.request.accepts(duration) {
            return None;
        }
        let mut error = 0.;
        let mut dwell = 0.;
        let mut seconds = 0.;
        let mut rows = vec![];
        // Score solo ownership; transitions retain their own measured objective.
        // The provisional last track is unscored until its exit/bookend is known.
        let count = plan.placements.len() - usize::from(!complete);
        for (i, p) in plan.placements.iter().take(count).enumerate() {
            let begin = if i == 0 {
                0
            } else {
                plan.transitions[i - 1].end_output_frame
            };
            let end = plan
                .transitions
                .get(i)
                .map_or(plan.total_output_frames, |t| t.start_output_frame);
            if end <= begin {
                return None;
            }
            let a = p.time_map.source_at(begin as f64).round() as u64;
            let b = p.time_map.source_at(end as f64).round() as u64;
            let index = self.ids[&p.track_id];
            let passage = self.passage(
                index,
                a,
                b,
                begin as f64 / rate,
                (end - begin) as f64 / rate,
            );
            // Bookends are checked only after choosing their musical endpoints.
            if passage.seconds + 0.05 < passage.minimum {
                return None;
            }
            error += passage.error * passage.seconds;
            seconds += passage.seconds;
            let shortage =
                ((passage.preferred - passage.seconds) / passage.preferred.max(1.)).max(0.);
            dwell += shortage * shortage * (1. + passage.development);
            rows.push(json!({"track_id":p.track_id,"solo_seconds":passage.seconds,"minimum_seconds":passage.minimum,
                "preferred_seconds":passage.preferred,"intensity_percentile":passage.energy,"arc_error":passage.error,"development":passage.development,
                "source_start_seconds":a as f64/self.profiles[index].rate,"source_end_seconds":b as f64/self.profiles[index].rate}));
        }
        let arc_error = error / seconds.max(1.);
        let dwell_penalty = dwell / count.max(1) as f64;
        let q = route_quality(plan);
        // Per-minute sustained value avoids rewarding extra joins just for count.
        let objective =
            q.sustained() / (duration / 60.).max(1.) - 8. * arc_error - 2. * dwell_penalty;
        Some(
            json!({"method":"set-composition/1","structure":self.request.shape.name(),"target_minutes":self.request.minutes,
            "minimum_seconds":self.request.min(),"maximum_seconds":self.request.max(),"duration_seconds":duration,
            "complete":complete,"objective":objective,"arc_error":arc_error,"dwell_penalty":dwell_penalty,"passages":rows,
            "semantics":"Acoustic intensity and development proxies; not measured emotion. Duration includes all overlaps, tempo maps and bookends."}),
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn assemble(
        &self,
        store: &Store,
        graph: &[Vec<MeasuredEdge>],
        pool: &[Track],
        pacing: &[TrackPacing],
        policy: &MixingConfig,
        deadline: Instant,
        cancel: &Cancellation,
    ) -> Result<Option<MixPlan>> {
        struct Route {
            used: u128,
            last: usize,
            entry: u64,
            plan: Option<MixPlan>,
        }
        let n = pool.len();
        let width = (policy.beam_width * 4).clamp(32, 256);
        let mut beam: Vec<_> = (0..n)
            .map(|i| Route {
                used: 1 << i,
                last: i,
                entry: 0,
                plan: None,
            })
            .collect();
        let mut best = None;
        let mut explored = 0usize;
        for _depth in 2..=n {
            let mut next = vec![];
            for route in &beam {
                if Instant::now() >= deadline {
                    return Ok(best);
                }
                cancel.check()?;
                for to in 0..n {
                    if route.used & (1 << to) != 0 {
                        continue;
                    }
                    for e in &graph[route.last * n + to] {
                        if explored % 64 == 0 {
                            cancel.check()?;
                            if Instant::now() >= deadline {
                                return Ok(best);
                            }
                        }
                        // Full sets do not buy length by adding emergency/weak joins.
                        if e.last_resort || e.score < 25. {
                            continue;
                        }
                        let Some(paced) = pacing[route.last].assess(route.entry, e.exit) else {
                            continue;
                        };
                        if route.entry > 0
                            && paced.solo_seconds
                                < self.profiles[route.last]
                                    .duration
                                    .mul_add(0.45, 0.)
                                    .min(60.)
                        {
                            continue;
                        }
                        let mut plan = match chain::extend_measured(
                            route.plan.as_ref(),
                            &e.plan,
                            e.score,
                            e.last_resort,
                        ) {
                            Ok(p) => p,
                            Err(err) if measured_beam::route_local(&err) => continue,
                            Err(err) => return Err(err),
                        };
                        if route.plan.is_none() {
                            let opening = self.profiles[route.last]
                                .opening
                                .before(plan.placements[0].source_start_frame);
                            if let Err(err) = chain::open_measured(&mut plan, opening) {
                                if measured_beam::route_local(&err) {
                                    continue;
                                }
                                return Err(err);
                            }
                        }
                        explored += 1;
                        if let Some(j) = plan
                            .header
                            .extensions
                            .get_mut("chain_joins")
                            .and_then(Value::as_array_mut)
                            .and_then(|j| j.last_mut())
                        {
                            j["pacing"] = serde_json::to_value(paced)?;
                        }
                        let join_end = plan.transitions.last().unwrap().end_output_frame as f64
                            / f64::from(plan.output.sample_rate_hz);
                        if join_end >= self.request.max() {
                            continue;
                        }
                        let Some(evidence) = self.evaluate(&plan, false) else {
                            continue;
                        };
                        plan.header
                            .extensions
                            .insert("set_composition".into(), evidence);
                        // Optimistic remaining source time: never prune a possible
                        // completion using average song length or a fixed song count.
                        let remaining: f64 = self
                            .profiles
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| route.used & (1 << i) == 0)
                            .map(|(_, p)| p.duration)
                            .sum();
                        if join_end + remaining / (1. - policy.max_speed_deviation).max(0.01)
                            < self.request.min()
                        {
                            continue;
                        }
                        if join_end
                            + self.profiles[to].duration
                                / (1. - policy.max_speed_deviation).max(0.01)
                            >= self.request.min()
                        {
                            let mut finished =
                                match chain::finish_measured(store, plan.clone(), cancel) {
                                    Ok(p) => p,
                                    Err(err) if measured_beam::route_local(&err) => continue,
                                    Err(err) => return Err(err),
                                };
                            if let Some(evidence) = self.evaluate(&finished, true) {
                                finished
                                    .header
                                    .extensions
                                    .insert("set_composition".into(), evidence);
                                if best.as_ref().is_none_or(|p| better(&finished, p)) {
                                    best = Some(finished);
                                }
                            }
                        }
                        next.push(Route {
                            used: route.used | (1 << to),
                            last: to,
                            entry: e.entry_end,
                            plan: Some(plan),
                        });
                        if next.len() >= width * 16 {
                            prune_routes(
                                &mut next,
                                width * 4,
                                |r| r.plan.as_ref().unwrap(),
                                |r| (r.last, r.entry),
                            );
                        }
                    }
                }
            }
            prune_routes(
                &mut next,
                width,
                |r| r.plan.as_ref().unwrap(),
                |r| (r.last, r.entry),
            );
            if next.is_empty() {
                break;
            }
            beam = next;
        }
        if let Some(p) = &mut best {
            p.header.extensions.get_mut("set_composition").unwrap()["assembled_candidates"] =
                json!(explored);
        }
        Ok(best)
    }
}
fn intensity(t: &SegmentTile) -> f64 {
    0.45 * t.band_occupancy.iter().sum::<f64>() / 3.
        + 0.25 * (t.onset_rate_hz / 8.).min(1.)
        + 0.15 * t.band_occupancy[2]
        + 0.15 * (t.rms * 5.).min(1.)
}
fn prune_routes<T>(
    routes: &mut Vec<T>,
    limit: usize,
    plan: impl Fn(&T) -> &MixPlan,
    identity: impl Fn(&T) -> (usize, u64),
) {
    routes.sort_by(|a, b| rank(plan(b)).total_cmp(&rank(plan(a))));
    let mut regions = BTreeMap::new();
    let mut kept = vec![];
    for r in routes.drain(..) {
        let p = plan(&r);
        let (last, entry) = identity(&r);
        let key = (
            last,
            entry / (30 * u64::from(p.placements.last().unwrap().time_map.source_sample_rate_hz)),
            p.total_output_frames / (120 * u64::from(p.output.sample_rate_hz)),
        );
        let count = regions.entry(key).or_insert(0usize);
        if *count < 2 {
            *count += 1;
            kept.push(r);
            if kept.len() == limit {
                break;
            }
        }
    }
    *routes = kept;
}
fn rank(p: &MixPlan) -> f64 {
    p.header
        .extensions
        .get("set_composition")
        .and_then(|v| v["objective"].as_f64())
        .unwrap_or(f64::NEG_INFINITY)
}
pub(super) fn better(a: &MixPlan, b: &MixPlan) -> bool {
    if a.header.extensions.contains_key("set_composition") {
        rank(a) > rank(b)
    } else {
        route_quality(a).best_first(route_quality(b)).is_lt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn long_plan(count: usize) -> (Context, MixPlan) {
        let mut plan: MixPlan =
            serde_json::from_str(include_str!("../../../examples/mix-plan.json")).unwrap();
        let template = plan.placements[0].clone();
        let transition = plan.transitions[0].clone();
        let rate = u64::from(plan.output.sample_rate_hz);
        plan.placements.clear();
        plan.transitions.clear();
        let mut profiles = vec![];
        let mut ids = BTreeMap::new();
        for i in 0..count {
            let mut p = template.clone();
            p.track_id = format!("song-{i}");
            ids.insert(p.track_id.clone(), i);
            let start = i as u64 * 170 * rate;
            p.source_start_frame = 0;
            p.source_end_frame = 180 * rate;
            p.time_map.anchors = vec![
                Anchor {
                    source_frame: 0,
                    output_frame: start,
                },
                Anchor {
                    source_frame: 180 * rate,
                    output_frame: start + 180 * rate,
                },
            ];
            plan.placements.push(p);
            if i > 0 {
                let mut t = transition.clone();
                t.start_output_frame = start;
                t.end_output_frame = start + 10 * rate;
                plan.transitions.push(t);
            }
            profiles.push(Profile {
                tiles: vec![],
                duration: 180.,
                rate: rate as f64,
                opening: Default::default(),
            });
        }
        plan.total_output_frames = (count as u64 * 170 + 10) * rate;
        (
            Context {
                request: Request::new(if count > 30 { 120 } else { 60 }, "auto").unwrap(),
                profiles,
                ids,
            },
            plan,
        )
    }
    #[test]
    fn full_sets_have_variable_counts_and_use_overlap_adjusted_output_duration() {
        for count in [21, 42] {
            let (context, mut plan) = long_plan(count);
            let evidence = context.evaluate(&plan, true).unwrap();
            assert_eq!(evidence["passages"].as_array().unwrap().len(), count);
            assert_eq!(
                evidence["duration_seconds"].as_f64().unwrap(),
                (count * 170 + 10) as f64
            );
            plan.total_output_frames =
                (context.request.max() as u64 + 1) * u64::from(plan.output.sample_rate_hz);
            assert!(
                context.evaluate(&plan, true).is_none(),
                "an extended closing passage cannot exceed the clock"
            );
        }
        let (context, plan) = long_plan(20);
        assert!(
            context.evaluate(&plan, true).is_none(),
            "source lengths sum to 60 minutes, but overlaps make this undersized"
        );
    }
    #[test]
    fn cli_duration_modes_reject_conflicting_track_count() {
        assert!(Args::try_parse_from([
            "mixlib",
            "search-set",
            "--set-minutes",
            "120",
            "--structure",
            "chapters"
        ])
        .is_ok());
        assert!(Args::try_parse_from([
            "mixlib",
            "search-set",
            "--set-minutes",
            "60",
            "--tracks",
            "8"
        ])
        .is_err());
        assert!(Args::try_parse_from(["mixlib", "search-set", "--set-minutes", "90"]).is_err());
    }
    #[test]
    fn only_requested_duration_windows_are_complete() {
        for minutes in [60, 120] {
            let r = Request::new(minutes, "auto").unwrap();
            assert!(r.accepts(r.min()));
            assert!(r.accepts(r.max()));
            assert!(!r.accepts(r.min() - 0.01));
            assert!(!r.accepts(r.max() + 0.01));
        }
        assert!(Request::new(90, "story").is_err());
        assert!(!Request::new(120, "auto")
            .unwrap()
            .capacity_possible(3600., 0.94));
        assert!(Request::new(60, "auto")
            .unwrap()
            .capacity_possible(3600., 0.94));
    }
    #[test]
    fn narrative_shapes_have_contrast_and_resolution() {
        for shape in [Shape::Story, Shape::Wave, Shape::Chapters] {
            assert!(shape.target(0.82) > shape.target(0.));
            assert!(shape.target(1.) < shape.target(0.82));
        }
        assert!(Shape::Ramp.target(0.8) > Shape::Ramp.target(0.2));
        assert_eq!(Request::new(120, "auto").unwrap().shape, Shape::Chapters);
    }
    #[test]
    fn later_development_is_weighted_without_requiring_a_whole_song() {
        let t = SegmentTile {
            start_frame: 0,
            end_frame: 10000,
            bars: 1,
            pulses_per_bar: 4,
            pulse_score_min: 1.,
            local_bpm: 120.,
            rms: 0.2,
            peak: 0.5,
            crest_factor_db: 3.,
            band_occupancy: [0.5; 3],
            onset_rate_hz: 2.,
            chroma: [0.; 12],
            level_variation: 0.,
            harmonic_variation: 0.,
            roles: BTreeMap::from([
                ("set_intensity_percentile".into(), 0.5),
                ("sustained_texture".into(), 1.),
            ]),
            vocal_activity: None,
            separation_quality: None,
        };
        let c = Context {
            request: Request::new(60, "story").unwrap(),
            profiles: vec![Profile {
                tiles: vec![t],
                duration: 400.,
                rate: 100.,
                opening: Default::default(),
            }],
            ids: BTreeMap::new(),
        };
        let p = c.passage(0, 0, 10000, 0., 100.);
        assert_eq!(p.minimum, 90.);
        assert_eq!(p.preferred, 270.);
        let mut c = c;
        c.profiles[0].tiles[0]
            .roles
            .insert("set_intensity_percentile".into(), 0.95);
        let mut later = c.profiles[0].tiles[0].clone();
        later.start_frame = 20000;
        later.end_frame = 30000;
        later.roles.insert("set_intensity_percentile".into(), 0.3);
        c.profiles[0].tiles.push(later);
        assert!(c.passage(0,20000,30000,0.,100.).error < c.passage(0,0,10000,0.,100.).error,
            "the same song's later passage can be a better opening; whole-track energy cannot decide this");
    }
}
