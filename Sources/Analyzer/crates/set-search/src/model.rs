//! Routing master for a materialized domain of passage and transition choices.
//! Passage durations include both overlaps. The clock rows subtract each
//! selected join once. Callers supply compatibility and frame envelopes.

use crate::scip::{BinaryModel, Row};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug)]
pub struct Frames {
    pub min: u64,
    pub max: u64,
}

impl Frames {
    fn valid(self) -> bool {
        self.min <= self.max && self.max <= (1 << 53)
    }
}

#[derive(Clone, Debug)]
pub struct Passage {
    pub track: usize,
    pub frames: Frames,
    pub can_open: bool,
    pub can_close: bool,
}

#[derive(Clone, Debug)]
pub struct Join {
    pub from_passage: usize,
    pub to_passage: usize,
    pub overlap: Frames,
    pub optimistic_surplus: f64,
}

#[derive(Clone, Debug)]
pub struct RequiredPair {
    pub first_track: usize,
    pub second_track: usize,
    pub either_order: bool,
}

#[derive(Clone, Debug)]
pub struct Domain {
    pub passages: Vec<Passage>,
    pub joins: Vec<Join>,
    pub required_pairs: Vec<RequiredPair>,
    pub clock: Frames,
    pub minimum_tracks: usize,
}

#[derive(Debug)]
pub struct InvalidDomain;

/// A rejected exact sequence starting at its first join's outgoing passage.
/// The same joins remain available inside a route with a different opening.
#[derive(Debug)]
pub struct PrefixConflict {
    pub joins: Vec<usize>,
}

/// An exact opening-to-closing join assignment already owned by another
/// search stage. Its edges may still appear inside another assignment.
#[derive(Debug)]
pub struct RouteExclusion {
    pub joins: Vec<usize>,
}

impl Domain {
    /// Passage variables come first, then join variables, followed by bookend
    /// and position variables. The caller must validate the decoded route.
    pub fn master(&self) -> Result<BinaryModel, InvalidDomain> {
        self.master_with_conflicts(&[])
    }

    pub fn master_with_conflicts(
        &self,
        conflicts: &[PrefixConflict],
    ) -> Result<BinaryModel, InvalidDomain> {
        self.master_with_restrictions(conflicts, &[])
    }

    pub fn master_with_restrictions(
        &self,
        conflicts: &[PrefixConflict],
        excluded_routes: &[RouteExclusion],
    ) -> Result<BinaryModel, InvalidDomain> {
        if !self.clock.valid()
            || self.minimum_tracks == 0
            || self.passages.iter().any(|p| !p.frames.valid())
            || self.joins.iter().any(|j| {
                match (
                    self.passages.get(j.from_passage),
                    self.passages.get(j.to_passage),
                ) {
                    (Some(from), Some(to)) => {
                        from.track == to.track
                            || !j.overlap.valid()
                            || !j.optimistic_surplus.is_finite()
                    }
                    _ => true,
                }
            })
        {
            return Err(InvalidDomain);
        }

        let n = self.passages.len();
        let mut model = BinaryModel {
            objective: vec![0.; n],
            rows: Vec::new(),
        };
        model
            .objective
            .extend(self.joins.iter().map(|j| j.optimistic_surplus));
        let mut incoming = vec![Vec::new(); n];
        let mut outgoing = vec![Vec::new(); n];
        let mut by_track = BTreeMap::<usize, Vec<_>>::new();
        let mut openings = Vec::new();
        let mut closings = Vec::new();
        let mut opening_for = vec![None; n];
        let mut closing_for = vec![None; n];
        for (p, passage) in self.passages.iter().enumerate() {
            by_track.entry(passage.track).or_default().push((p, 1.));
            if passage.can_open {
                let variable = model.add_zero_variable();
                openings.push((variable, 1.));
                incoming[p].push((variable, 1.));
                opening_for[p] = Some(variable);
            }
            if passage.can_close {
                let variable = model.add_zero_variable();
                closings.push((variable, 1.));
                outgoing[p].push((variable, 1.));
                closing_for[p] = Some(variable);
            }
        }
        model.rows.push(exactly(openings, 1.));
        model.rows.push(exactly(closings, 1.));
        for conflict in conflicts {
            let (head, _) = self.route_endpoints(&conflict.joins)?;
            if let Some(opening) = opening_for[head] {
                let mut terms = conflict
                    .joins
                    .iter()
                    .map(|&j| (n + j, 1.))
                    .collect::<Vec<_>>();
                terms.push((opening, 1.));
                model.rows.push(at_most(terms, conflict.joins.len() as f64));
            }
        }
        for excluded in excluded_routes {
            let (head, tail) = self.route_endpoints(&excluded.joins)?;
            if let (Some(opening), Some(closing)) = (opening_for[head], closing_for[tail]) {
                let mut terms = excluded
                    .joins
                    .iter()
                    .map(|&j| (n + j, 1.))
                    .collect::<Vec<_>>();
                terms.extend([(opening, 1.), (closing, 1.)]);
                model
                    .rows
                    .push(at_most(terms, excluded.joins.len() as f64 + 1.));
            }
        }
        model.rows.push(Row {
            terms: (0..n).map(|p| (p, 1.)).collect(),
            lower: Some(self.minimum_tracks as f64),
            upper: None,
        });

        // A position on a route of k distinct songs needs ceil(log2(k)) bits.
        // A selected join requires a strict increase in position. Flow can
        // therefore contain one start-to-finish path and no detached cycles.
        let count = by_track.len().max(2);
        let bits = (count - 1).ilog2() + 1;
        let big_m = count as f64;
        let positions: Vec<Vec<_>> = (0..n)
            .map(|_| {
                (0..bits)
                    .map(|bit| (model.add_zero_variable(), (1_u64 << bit) as f64))
                    .collect()
            })
            .collect();
        for (p, position) in positions.iter().enumerate() {
            let mut terms = position.clone();
            terms.push((p, 1. - big_m));
            if let Some(opening) = opening_for[p] {
                terms.push((opening, big_m - 1.));
            }
            // Unused passages and the selected opening have position zero.
            model.rows.push(at_most(terms, 0.));
        }

        let mut parallel = BTreeMap::<_, Vec<_>>::new();
        for (e, join) in self.joins.iter().enumerate() {
            let variable = n + e;
            incoming[join.to_passage].push((variable, 1.));
            outgoing[join.from_passage].push((variable, 1.));
            parallel
                .entry((join.from_passage, join.to_passage))
                .or_default()
                .push((variable, big_m));
        }
        for ((from, to), mut terms) in parallel {
            // Flow allows only one join between these two passages. Enforce
            // the ordering once for the sum of the parallel choices.
            terms.extend(positions[from].iter().copied());
            terms.extend(positions[to].iter().map(|&(i, w)| (i, -w)));
            model.rows.push(at_most(terms, big_m - 1.));
        }
        for (p, (mut ins, mut outs)) in incoming.into_iter().zip(outgoing).enumerate() {
            ins.push((p, -1.));
            outs.push((p, -1.));
            model.rows.push(exactly(ins, 0.));
            model.rows.push(exactly(outs, 0.));
        }
        for passages in by_track.into_values() {
            model.rows.push(at_most(passages, 1.));
        }
        for pair in &self.required_pairs {
            let terms = self
                .joins
                .iter()
                .enumerate()
                .filter_map(|(e, join)| {
                    let from = self.passages[join.from_passage].track;
                    let to = self.passages[join.to_passage].track;
                    let forward = (from, to) == (pair.first_track, pair.second_track);
                    let reverse = (to, from) == (pair.first_track, pair.second_track);
                    (forward || (pair.either_order && reverse)).then_some((n + e, 1.))
                })
                .collect();
            model.rows.push(exactly(terms, 1.));
        }
        model
            .rows
            .push(at_most(self.clock_terms(false), self.clock.max as f64));
        model.rows.push(Row {
            terms: self.clock_terms(true),
            lower: Some(self.clock.min as f64),
            upper: None,
        });
        Ok(model)
    }

    fn route_endpoints(&self, choices: &[usize]) -> Result<(usize, usize), InvalidDomain> {
        let mut joins = choices.iter().map(|&i| self.joins.get(i));
        let first = joins.next().flatten().ok_or(InvalidDomain)?;
        let mut tail = first.to_passage;
        for join in joins {
            tail = join
                .filter(|j| j.from_passage == tail)
                .ok_or(InvalidDomain)?
                .to_passage;
        }
        Ok((first.from_passage, tail))
    }

    fn clock_terms(&self, use_upper: bool) -> Vec<(usize, f64)> {
        let spans = self.passages.iter().enumerate().map(|(i, passage)| {
            let frames = if use_upper {
                passage.frames.max
            } else {
                passage.frames.min
            };
            (i, frames as f64)
        });
        let overlaps = self.joins.iter().enumerate().map(|(i, join)| {
            let frames = if use_upper {
                join.overlap.min
            } else {
                join.overlap.max
            };
            (self.passages.len() + i, -(frames as f64))
        });
        spans.chain(overlaps).collect()
    }
}

impl BinaryModel {
    fn add_zero_variable(&mut self) -> usize {
        let variable = self.objective.len();
        self.objective.push(0.);
        variable
    }
}

fn exactly(terms: Vec<(usize, f64)>, value: f64) -> Row {
    Row {
        terms,
        lower: Some(value),
        upper: Some(value),
    }
}

fn at_most(terms: Vec<(usize, f64)>, upper: f64) -> Row {
    Row {
        terms,
        lower: None,
        upper: Some(upper),
    }
}
