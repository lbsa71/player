//! Fair, finite measurement discovery, independent of changing draft rankings.
//!
//! Keep the pool order and per-track limits stable while using or resuming a
//! schedule. The cursor is the next global slot, including slots skipped because
//! their probe was already measured or their source has no remaining audio.

use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Probe {
    pub from: usize,
    pub to: usize,
    pub after: u64,
    pub tier: usize,
}

pub(super) struct Schedule {
    pub pool_cursor: usize,
    prefer_global: bool,
    seen: BTreeSet<Probe>,
}

// Cover duration-bearing exits before broadening to event and generic tactics.
const PHASES: [(u64, usize); 6] = [(60, 1), (35, 1), (80, 1), (0, 1), (0, 0), (0, 24)];

impl Schedule {
    pub fn new(pool_cursor: usize) -> Self {
        Self {
            pool_cursor,
            prefer_global: true,
            seen: BTreeSet::new(),
        }
    }

    /// Give global coverage at least every other turn, even when new draft
    /// boundaries continuously replace the front of `targeted`.
    pub fn next(&mut self, targeted: &[Probe], frames: &[u64], minimum: &[u64]) -> Option<Probe> {
        let prefer_global = self.prefer_global;
        self.prefer_global = !prefer_global;
        if prefer_global {
            self.next_global(frames, minimum)
                .or_else(|| self.next_targeted(targeted, frames, minimum))
        } else {
            self.next_targeted(targeted, frames, minimum)
                .or_else(|| self.next_global(frames, minimum))
        }
    }

    fn next_global(&mut self, frames: &[u64], minimum: &[u64]) -> Option<Probe> {
        let n = frames.len();
        let pairs = pair_count(n)?;
        let total = pairs.checked_mul(PHASES.len())?;
        while self.pool_cursor < total {
            let cursor = self.pool_cursor;
            self.pool_cursor += 1;
            let position = cursor % pairs;
            let from = position % n;
            let to = (from + 1 + position / n) % n;
            let probe = global_probe(from, to, cursor / pairs, frames, minimum);
            if valid(probe, frames)
                && !was_scanned(probe, cursor, frames, minimum)
                && self.seen.insert(probe)
            {
                return Some(probe);
            }
        }
        None
    }

    fn next_targeted(
        &mut self,
        targeted: &[Probe],
        frames: &[u64],
        minimum: &[u64],
    ) -> Option<Probe> {
        targeted.iter().copied().find(|&probe| {
            valid(probe, frames)
                && !was_scanned(probe, self.pool_cursor, frames, minimum)
                && self.seen.insert(probe)
        })
    }
}

fn pair_count(n: usize) -> Option<usize> {
    (n >= 2).then(|| n.checked_mul(n - 1)).flatten()
}

fn valid(probe: Probe, frames: &[u64]) -> bool {
    probe.from != probe.to
        && probe.to < frames.len()
        && frames.get(probe.from).is_some_and(|&end| probe.after < end)
}

fn global_probe(from: usize, to: usize, phase: usize, frames: &[u64], minimum: &[u64]) -> Probe {
    let (percent, tier) = PHASES[phase];
    let fraction = (u128::from(frames[from]) * u128::from(percent) / 100) as u64;
    Probe {
        from,
        to,
        after: fraction.max(minimum.get(from).copied().unwrap_or(0)),
        tier,
    }
}

/// Reconstruct prior global identities from the cursor so resuming does not
/// repeat phases whose fractions collapse to the same minimum exit time.
fn was_scanned(probe: Probe, cursor: usize, frames: &[u64], minimum: &[u64]) -> bool {
    let n = frames.len();
    let Some(pairs) = pair_count(n) else {
        return false;
    };
    let offset = if probe.to > probe.from {
        probe.to - probe.from - 1
    } else {
        n - probe.from + probe.to - 1
    };
    let position = offset * n + probe.from;
    PHASES.iter().enumerate().any(|(phase, _)| {
        phase
            .checked_mul(pairs)
            .and_then(|start| start.checked_add(position))
            .is_some_and(|slot| {
                slot < cursor && global_probe(probe.from, probe.to, phase, frames, minimum) == probe
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(schedule: &mut Schedule, frames: &[u64], minimum: &[u64]) -> Vec<Probe> {
        std::iter::from_fn(|| schedule.next(&[], frames, minimum)).collect()
    }

    #[test]
    fn first_global_round_visits_every_source_despite_changing_targets() {
        let n = 7;
        let mut schedule = Schedule::new(0);
        let mut sources = Vec::new();
        for turn in 0..(2 * n - 1) {
            let targeted = [Probe {
                from: 0,
                to: 1,
                after: turn as u64,
                tier: 99,
            }];
            let probe = schedule
                .next(&targeted, &vec![1_000; n], &vec![0; n])
                .unwrap();
            if turn % 2 == 0 {
                assert_eq!(probe.tier, 1);
                assert_eq!(probe.after, 600);
                sources.push(probe.from);
            }
        }
        assert_eq!(sources, (0..n).collect::<Vec<_>>());
    }

    #[test]
    fn changing_targets_cannot_starve_any_directed_pair() {
        let n = 7;
        let pairs = n * (n - 1);
        let mut schedule = Schedule::new(0);
        let mut covered = BTreeSet::new();
        for turn in 0..(2 * pairs) {
            let targeted = [Probe {
                from: turn % n,
                to: (turn + 1) % n,
                after: turn as u64,
                tier: 99,
            }];
            let probe = schedule
                .next(&targeted, &vec![1_000; n], &vec![0; n])
                .unwrap();
            if probe.tier == 1 {
                assert_eq!(probe.after, 600);
                covered.insert((probe.from, probe.to));
            }
        }
        assert_eq!(covered.len(), pairs);
        for from in 0..n {
            for to in 0..n {
                assert_eq!(covered.contains(&(from, to)), from != to);
            }
        }
    }

    #[test]
    fn visits_late_fractions_then_event_and_generic_with_deduplication() {
        let mut schedule = Schedule::new(0);
        let probes = drain(&mut schedule, &[1_000, 2_000], &[900, 0]);
        let compact: Vec<_> = probes.iter().map(|p| (p.from, p.after, p.tier)).collect();
        assert_eq!(
            compact,
            vec![
                (0, 900, 1),
                (1, 1_200, 1),
                (1, 700, 1),
                (1, 1_600, 1),
                (1, 0, 1),
                (0, 900, 0),
                (1, 0, 0),
                (0, 900, 24),
                (1, 0, 24),
            ]
        );
        assert_eq!(probes.iter().collect::<BTreeSet<_>>().len(), probes.len());
        assert_eq!(schedule.pool_cursor, 12);
    }

    #[test]
    fn targeted_and_global_probes_share_identity_and_exhaustion_is_finite() {
        let mut schedule = Schedule::new(0);
        let targeted = [
            Probe {
                from: 0,
                to: 1,
                after: 600,
                tier: 1,
            },
            Probe {
                from: 0,
                to: 1,
                after: 750,
                tier: 24,
            },
        ];
        let mut probes = Vec::new();
        while let Some(probe) = schedule.next(&targeted, &[1_000; 2], &[0; 2]) {
            probes.push(probe);
        }
        assert_eq!(probes[0], targeted[0]);
        assert_eq!(probes[1], targeted[1]);
        assert_eq!(probes.iter().collect::<BTreeSet<_>>().len(), probes.len());
        assert_eq!(probes.len(), 13);
        assert_eq!(schedule.pool_cursor, 12);
        assert!(schedule.next(&targeted, &[1_000; 2], &[0; 2]).is_none());
    }

    #[test]
    fn cursor_resume_preserves_position_and_collapsed_phase_deduplication() {
        let frames = [1_000; 3];
        let minimum = [900; 3];
        let mut original = Schedule::new(0);
        for _ in 0..6 {
            original.next(&[], &frames, &minimum).unwrap();
        }
        assert_eq!(original.pool_cursor, 6);
        let mut resumed = Schedule::new(original.pool_cursor);
        assert_eq!(
            drain(&mut resumed, &frames, &minimum),
            drain(&mut original, &frames, &minimum)
        );
        assert_eq!(resumed.pool_cursor, 36);

        assert_eq!(
            Schedule::new(1).next(&[], &frames, &[0; 3]),
            Some(Probe {
                from: 1,
                to: 2,
                after: 600,
                tier: 1
            })
        );
    }

    #[test]
    fn exhausted_global_sweeps_still_accept_new_targets_once() {
        let mut schedule = Schedule::new(12);
        let already_global = Probe {
            from: 0,
            to: 1,
            after: 600,
            tier: 1,
        };
        let targeted = Probe {
            from: 0,
            to: 1,
            after: 777,
            tier: 24,
        };
        assert_eq!(
            schedule.next(&[already_global, targeted], &[1_000; 2], &[0; 2]),
            Some(targeted)
        );
        assert!(schedule.next(&[targeted], &[1_000; 2], &[0; 2]).is_none());
        assert_eq!(schedule.pool_cursor, 12);
    }

    #[test]
    fn skips_invalid_or_finished_sources_without_panicking_on_small_pools() {
        let mut schedule = Schedule::new(0);
        let invalid = [
            Probe {
                from: 0,
                to: 1,
                after: 100,
                tier: 1,
            },
            Probe {
                from: 1,
                to: 0,
                after: 0,
                tier: 1,
            },
            Probe {
                from: 2,
                to: 2,
                after: 0,
                tier: 1,
            },
            Probe {
                from: 3,
                to: 0,
                after: 0,
                tier: 1,
            },
        ];
        let mut probes = Vec::new();
        while let Some(probe) = schedule.next(&invalid, &[100, 0, 100], &[100, 0, 0]) {
            assert_eq!(probe.from, 2);
            probes.push(probe);
        }
        assert!(!probes.is_empty());
        assert!(Schedule::new(0).next(&[], &[], &[]).is_none());
        assert!(Schedule::new(0).next(&[], &[100], &[0]).is_none());
    }

    #[test]
    fn fraction_calculation_handles_long_frame_counts() {
        let probe = Schedule::new(0).next(&[], &[u64::MAX; 2], &[0; 2]).unwrap();
        assert_eq!(probe.after, (u128::from(u64::MAX) * 60 / 100) as u64);
    }
}
