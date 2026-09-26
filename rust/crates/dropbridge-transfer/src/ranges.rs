//! Byte-range sets for resume tracking (spec §39–40).
//!
//! Invariant: ranges are sorted, non-overlapping, non-adjacent
//! (adjacent ranges are merged).

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RangeSet {
    /// Sorted merged (start, end-exclusive) ranges.
    ranges: Vec<(u64, u64)>,
}

impl RangeSet {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_sorted(ranges: Vec<(u64, u64)>) -> Self {
        let mut s = Self::default();
        for (a, b) in ranges {
            s.add(a, b);
        }
        s
    }

    /// Insert [start, end). No-op for empty ranges.
    pub fn add(&mut self, mut start: u64, mut end: u64) {
        if start >= end {
            return;
        }
        // Merge with overlapping/adjacent existing ranges.
        let mut out = Vec::with_capacity(self.ranges.len() + 1);
        let mut inserted = false;
        for &(a, b) in &self.ranges {
            if b < start {
                out.push((a, b));
            } else if a > end {
                if !inserted {
                    out.push((start, end));
                    inserted = true;
                }
                out.push((a, b));
            } else {
                start = start.min(a);
                end = end.max(b);
            }
        }
        if !inserted {
            out.push((start, end));
        }
        self.ranges = out;
    }

    /// Total covered bytes.
    #[must_use]
    pub fn covered(&self) -> u64 {
        self.ranges.iter().map(|(a, b)| b - a).sum()
    }

    /// True if [0, size) is fully covered.
    #[must_use]
    pub fn complete(&self, size: u64) -> bool {
        size == 0 || self.ranges.iter().any(|&(a, b)| a == 0 && b >= size)
    }

    /// Iterate uncovered sub-ranges within [0, size).
    pub fn missing<'a>(&'a self, size: u64) -> impl Iterator<Item = (u64, u64)> + 'a {
        MissingIter {
            ranges: &self.ranges,
            idx: 0,
            cursor: 0,
            size,
        }
    }

    #[must_use]
    pub fn as_slice(&self) -> &[(u64, u64)] {
        &self.ranges
    }

    #[must_use]
    pub fn contains_offset(&self, off: u64) -> bool {
        self.ranges.iter().any(|&(a, b)| off >= a && off < b)
    }
}

struct MissingIter<'a> {
    ranges: &'a [(u64, u64)],
    idx: usize,
    cursor: u64,
    size: u64,
}

impl Iterator for MissingIter<'_> {
    type Item = (u64, u64);
    fn next(&mut self) -> Option<(u64, u64)> {
        if self.cursor >= self.size {
            return None;
        }
        // Skip ranges below cursor.
        while self.idx < self.ranges.len() && self.ranges[self.idx].1 <= self.cursor {
            self.idx += 1;
        }
        let hole_start = self.cursor;
        let hole_end = if self.idx < self.ranges.len() {
            let (a, b) = self.ranges[self.idx];
            if a > self.cursor {
                a.min(self.size)
            } else {
                self.cursor = b.min(self.size).max(self.cursor);
                return self.next();
            }
        } else {
            self.size
        };
        self.cursor = hole_end;
        if hole_end > hole_start {
            Some((hole_start, hole_end))
        } else {
            self.next()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn basics() {
        let mut r = RangeSet::new();
        assert!(r.complete(0));
        assert!(!r.complete(10));
        r.add(0, 5);
        r.add(5, 10);
        assert_eq!(r.as_slice(), &[(0, 10)]);
        assert!(r.complete(10));
        assert_eq!(r.covered(), 10);

        let mut r2 = RangeSet::new();
        r2.add(10, 20);
        r2.add(0, 5);
        let missing: Vec<_> = r2.missing(30).collect();
        assert_eq!(missing, vec![(5, 10), (20, 30)]);
        assert_eq!(r2.covered(), 15);
        assert!(r2.contains_offset(15));
        assert!(!r2.contains_offset(7));
    }

    proptest! {
        #[test]
        fn covered_matches_naive(
            ops in proptest::collection::vec((0u64..200, 0u64..200), 0..50)
        ) {
            let mut set = RangeSet::new();
            let mut naive = [false; 200];
            for (a, b) in &ops {
                let (a, b) = (*a.min(b), *a.max(b));
                set.add(a, b);
                for i in a..b { naive[i as usize] = true; }
            }
            let expected: u64 = naive.iter().filter(|x| **x).count() as u64;
            prop_assert_eq!(set.covered(), expected);
            let full = (0..200).all(|i| naive[i]);
            prop_assert_eq!(set.complete(200), full);
            // missing() exactly covers uncovered bytes
            let mut re = [false; 200];
            for (a, b) in set.missing(200) {
                for i in a..b { re[i as usize] = true; }
            }
            for i in 0..200 {
                prop_assert_eq!(re[i], !naive[i], "byte {}", i);
            }
        }
    }
}
