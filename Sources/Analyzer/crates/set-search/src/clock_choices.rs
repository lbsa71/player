//! Incremental enumeration of mutually exclusive frame-count choices.

#[derive(Debug, PartialEq, Eq)]
pub enum Advance {
    Candidate(Vec<usize>),
    Pending,
    Exhausted,
}

/// Portable DFS position. Frame totals and pruning bounds are rebuilt.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ClockCursor {
    selected: Vec<usize>,
    next: Vec<usize>,
    exhausted: bool,
}

pub struct ClockChoices {
    groups: Vec<Vec<u64>>,
    suffix_max: Vec<u64>,
    minimum: u64,
    maximum: u64,
    selected: Vec<usize>,
    totals: Vec<u64>,
    next: Vec<usize>,
    exhausted: bool,
}

impl ClockChoices {
    pub fn new(groups: Vec<Vec<u64>>, minimum: u64, maximum: u64) -> Self {
        let mut suffix_max = vec![0u64; groups.len() + 1];
        for (i, group) in groups.iter().enumerate().rev() {
            suffix_max[i] =
                suffix_max[i + 1].saturating_add(group.iter().copied().max().unwrap_or(0));
        }
        let exhausted =
            minimum > maximum || suffix_max[0] < minimum || groups.iter().any(Vec::is_empty);
        Self {
            next: vec![0; groups.len() + 1],
            groups,
            suffix_max,
            minimum,
            maximum,
            selected: Vec::new(),
            totals: vec![0],
            exhausted,
        }
    }

    pub fn cursor(&self) -> ClockCursor {
        ClockCursor {
            selected: self.selected.clone(),
            next: self.next[..=self.selected.len()].to_vec(),
            exhausted: self.exhausted,
        }
    }

    /// Restore a cursor for the same ordered option groups and frame interval.
    /// A malformed cursor leaves this search at its original position.
    pub fn restore(&mut self, cursor: ClockCursor) -> bool {
        let depth = cursor.selected.len();
        if depth > self.groups.len()
            || cursor.next.len() != depth + 1
            || cursor.exhausted && depth != 0
            || self.exhausted && !cursor.exhausted
        {
            return false;
        }
        let mut totals = vec![0u64];
        for (i, &choice) in cursor.selected.iter().enumerate() {
            let Some(&frames) = self.groups[i].get(choice) else {
                return false;
            };
            let Some(total) = totals[i].checked_add(frames) else {
                return false;
            };
            if cursor.next[i] != choice + 1
                || total > self.maximum
                || total.saturating_add(self.suffix_max[i + 1]) < self.minimum
            {
                return false;
            }
            totals.push(total);
        }
        if cursor.next[depth] > self.groups.get(depth).map_or(0, Vec::len) {
            return false;
        }
        self.next[..=depth].copy_from_slice(&cursor.next);
        self.selected = cursor.selected;
        self.totals = totals;
        self.exhausted = cursor.exhausted;
        true
    }

    fn backtrack(&mut self) {
        self.totals.pop();
        if self.selected.pop().is_none() {
            self.exhausted = true;
        }
    }

    pub fn advance(&mut self, steps: usize) -> Advance {
        for _ in 0..steps {
            if self.exhausted {
                return Advance::Exhausted;
            }
            let depth = self.selected.len();
            if depth == self.groups.len() {
                let result = self.selected.clone();
                self.backtrack();
                return Advance::Candidate(result);
            }
            let choice = self.next[depth];
            let Some(&added) = self.groups[depth].get(choice) else {
                self.backtrack();
                continue;
            };
            self.next[depth] += 1;
            let Some(total) = self.totals[depth].checked_add(added) else {
                continue;
            };
            if total > self.maximum
                || total.saturating_add(self.suffix_max[depth + 1]) < self.minimum
            {
                continue;
            }
            self.selected.push(choice);
            self.totals.push(total);
            self.next[depth + 1] = 0;
        }
        if self.exhausted {
            Advance::Exhausted
        } else {
            Advance::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incremental_search_preserves_exact_frame_alternatives() {
        let mut search =
            ClockChoices::new(vec![vec![0, 1001, 1004], vec![0, 1002, 1005]], 2006, 2009);
        let mut actual = Vec::new();
        loop {
            match search.advance(1) {
                Advance::Candidate(c) => actual.push(c),
                Advance::Pending => {}
                Advance::Exhausted => break,
            }
        }
        assert_eq!(actual, vec![vec![1, 2], vec![2, 1], vec![2, 2]]);
    }
    #[test]
    fn checkpoint_continues_after_a_yield_without_repeating_it() {
        let make = || ClockChoices::new(vec![vec![0, 1001, 1004], vec![0, 1002, 1005]], 2006, 2009);
        let mut uninterrupted = make();
        assert_eq!(uninterrupted.advance(32), Advance::Candidate(vec![1, 2]));
        let mut resumed = make();
        assert!(resumed.restore(uninterrupted.cursor()));
        assert_eq!(resumed.advance(32), Advance::Candidate(vec![2, 1]));
        assert_eq!(resumed.advance(32), Advance::Candidate(vec![2, 2]));
        assert_eq!(resumed.advance(32), Advance::Exhausted);
        let mut corrupt = uninterrupted.cursor();
        corrupt.next[0] = usize::MAX;
        assert!(!make().restore(corrupt));
    }
}
