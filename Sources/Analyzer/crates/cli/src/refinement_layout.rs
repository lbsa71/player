//! Pure layout rules for complete draft sets. Neither helper admits audio.
use crate::required_pairs::RequiredPairs;

/// Order intact measured blocks while preserving every required song adjacency.
/// The bound limits this heuristic attempt; `None` is not an infeasibility proof.
pub(super) fn order(
    blocks: &[Vec<usize>],
    required: &RequiredPairs,
    pool_len: usize,
) -> Option<Vec<usize>> {
    if blocks.is_empty() || !(1..=128).contains(&pool_len) {
        return None;
    }
    let mut all = 0u128;
    for block in blocks {
        if block.is_empty() {
            return None;
        }
        for &track in block {
            if track >= pool_len || all & (1u128 << track) != 0 {
                return None;
            }
            all |= 1u128 << track;
        }
    }
    if !required.complete_used(all) {
        return None;
    }
    let mut search = Ordering {
        blocks,
        required,
        tracks: all.count_ones() as usize,
        work: 0,
    };
    let mut result = Vec::with_capacity(blocks.len());
    search.visit(0, 0, None, &mut result).then_some(result)
}

struct Ordering<'a> {
    blocks: &'a [Vec<usize>],
    required: &'a RequiredPairs,
    tracks: usize,
    work: usize,
}
impl Ordering<'_> {
    fn visit(
        &mut self,
        used_blocks: u128,
        used_tracks: u128,
        last: Option<usize>,
        result: &mut Vec<usize>,
    ) -> bool {
        if result.len() == self.blocks.len() {
            return self.required.complete_used(used_tracks);
        }
        let mut choices: Vec<_> = (0..self.blocks.len())
            .filter(|i| used_blocks & (1u128 << i) == 0)
            .collect();
        choices.sort_by(|&a, &b| {
            let priority = |index: usize| {
                last.map_or(0., |from| {
                    self.required.priority(from, self.blocks[index][0])
                })
            };
            priority(b).total_cmp(&priority(a)).then(a.cmp(&b))
        });
        for index in choices {
            if self.work >= 10_000 {
                return false;
            }
            self.work += 1;
            let mut used = used_tracks;
            let mut previous = last;
            let mut allowed = true;
            for &track in &self.blocks[index] {
                let remaining = self.tracks - used.count_ones() as usize - 1;
                if previous.map_or_else(
                    || !self.required.start_allowed(track),
                    |from| !self.required.allows(used, from, track, remaining),
                ) {
                    allowed = false;
                    break;
                }
                used |= 1u128 << track;
                previous = Some(track);
            }
            if allowed {
                result.push(index);
                if self.visit(used_blocks | (1u128 << index), used, previous, result) {
                    return true;
                }
                result.pop();
            }
        }
        false
    }
}

/// Distribute trimming across available block tails, preserving all minima and
/// an exact output-frame total. Malformed or overflowing ranges fail closed.
pub(super) fn fit(ranges: &[(u64, u64)], target: u64) -> Option<Vec<u64>> {
    let (minimum, maximum) = ranges.iter().try_fold((0u64, 0u64), |(lo, hi), &(a, b)| {
        if a > b {
            return None;
        }
        Some((lo.checked_add(a)?, hi.checked_add(b)?))
    })?;
    if target < minimum || target > maximum {
        return None;
    }
    let capacity = maximum - minimum;
    if capacity == 0 {
        return Some(ranges.iter().map(|&(_, end)| end).collect());
    }
    let trim = maximum - target;
    let mut allocated = 0u64;
    let mut lengths = Vec::with_capacity(ranges.len());
    for &(minimum, maximum) in ranges {
        let share =
            u64::try_from(u128::from(trim) * u128::from(maximum - minimum) / u128::from(capacity))
                .ok()?;
        allocated = allocated.checked_add(share)?;
        lengths.push(maximum.checked_sub(share)?);
    }
    // Proportional integer rounding leaves fewer than one frame per block.
    let mut remainder = trim.checked_sub(allocated)?;
    for (length, &(minimum, _)) in lengths.iter_mut().zip(ranges) {
        if remainder > 0 && *length > minimum {
            *length -= 1;
            remainder -= 1;
        }
    }
    (remainder == 0).then_some(lengths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::required_pairs::{Mode, Pair};

    // All mentioned songs are present in these ordering tests. Production
    // RequiredPairs::resolve also builds the inclusion mask checked by order.
    fn pairs(mode: Mode, edges: &[[usize; 2]]) -> RequiredPairs {
        let mut required = RequiredPairs::default();
        required.mode = mode;
        required.pairs = edges
            .iter()
            .map(|&indices| Pair {
                indices,
                spotify_ids: indices.map(|i| format!("spotify-{i}")),
                track_ids: indices.map(|i| format!("track-{i}")),
            })
            .collect();
        required
    }

    #[test]
    fn empty_rules_keep_order_and_never_repeat_or_invent_a_song() {
        let required = RequiredPairs::default();
        assert_eq!(
            order(&[vec![2, 3], vec![0], vec![1]], &required, 4),
            Some(vec![0, 1, 2])
        );
        assert!(order(&[vec![0, 1], vec![1, 2]], &required, 3).is_none());
        assert!(order(&[vec![0, 0]], &required, 3).is_none());
        assert!(order(&[vec![3]], &required, 3).is_none());
        assert!(order(&[vec![128]], &required, 128).is_none());
        assert!(order(&[vec![0]], &required, 129).is_none());
        assert!(order(&[vec![]], &required, 1).is_none());
        assert!(order(&[], &required, 1).is_none());
    }

    #[test]
    fn required_adjacency_is_checked_inside_blocks_as_well_as_between_them() {
        let required = pairs(Mode::Ordered, &[[0, 1], [1, 2]]);
        assert_eq!(
            order(&[vec![2], vec![1], vec![3], vec![0]], &required, 4),
            Some(vec![2, 3, 1, 0])
        );
        assert!(order(&[vec![0, 3, 1], vec![2]], &required, 4).is_none());
        assert!(order(&[vec![2, 1, 0]], &required, 3).is_none());
        let either = pairs(Mode::Either, &[[0, 1], [1, 2]]);
        assert_eq!(
            order(&[vec![1], vec![2], vec![0]], &either, 3),
            Some(vec![1, 0, 2])
        );
        assert_eq!(order(&[vec![2, 1, 0]], &either, 3), Some(vec![0]));
    }

    #[test]
    fn duration_fits_exactly_and_never_consumes_protected_audio() {
        let ranges = [(10, 20), (7, 7), (30, 45)];
        for target in 47..=72 {
            let lengths = fit(&ranges, target).unwrap();
            assert_eq!(lengths.iter().sum::<u64>(), target);
            assert!(lengths
                .iter()
                .zip(ranges)
                .all(|(&n, (lo, hi))| (lo..=hi).contains(&n)));
            assert_eq!(lengths, fit(&ranges, target).unwrap());
        }
        assert_eq!(fit(&[(0, 1), (0, 1), (0, 1)], 2), Some(vec![0, 1, 1]));
        assert_eq!(fit(&[(5, 5)], 5), Some(vec![5]));
        assert_eq!(fit(&[], 0), Some(vec![]));
        assert!(fit(&ranges, 46).is_none());
        assert!(fit(&ranges, 73).is_none());
        assert!(fit(&[(5, 4)], 5).is_none());
        assert!(fit(&[], 1).is_none());
    }

    #[test]
    fn overflowing_frame_totals_fail_instead_of_wrapping() {
        assert!(fit(&[(u64::MAX, u64::MAX), (1, 1)], 0).is_none());
        assert!(fit(&[(0, u64::MAX), (0, 1)], 1).is_none());
        let lengths = fit(&[(0, u64::MAX - 1), (0, 1)], u64::MAX / 2).unwrap();
        assert_eq!(lengths.iter().sum::<u64>(), u64::MAX / 2);
    }
}
