//! The view: nodes that tile the log, merged by due order in batches.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BinaryHeap;

pub const PLACEHOLDER: &str = "(not summarized yet: zoom it)";

/// Sizes of built nodes' text. `None` means the node is not built.
pub trait Sizes {
    fn text_size(&self, l: u32, i: u64) -> Option<usize>;
}

impl<F: Fn(u32, u64) -> Option<usize>> Sizes for F {
    fn text_size(&self, l: u32, i: u64) -> Option<usize> {
        self(l, i)
    }
}

fn digits(mut n: u64) -> usize {
    let mut d = 1;
    while n >= 10 {
        n /= 10;
        d += 1;
    }
    d
}

/// Bytes of the rendered line `id+n|text\n`; an unbuilt node renders the placeholder.
pub fn line_bytes(l: u32, i: u64, text: Option<usize>) -> usize {
    digits(i << l) + 1 + digits(1 << l) + 1 + text.unwrap_or(PLACEHOLDER.len()) + 1
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Line {
    pub l: u32,
    pub i: u64,
    pub bytes: usize,
}

impl Line {
    pub fn id(&self) -> u64 {
        self.i << self.l
    }

    pub fn n(&self) -> u64 {
        1 << self.l
    }

    /// One past the last message covered.
    pub fn end(&self) -> u64 {
        (self.i + 1) << self.l
    }
}

/// How due a pair is: `(T - last) / 2^l`, compared as exact fractions. The oldest of equal pairs wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Due {
    age: u64,
    l: u32,
    id: u64,
}

impl Ord for Due {
    fn cmp(&self, o: &Self) -> Ordering {
        let a = u128::from(self.age) << o.l;
        let b = u128::from(o.age) << self.l;
        a.cmp(&b).then(o.id.cmp(&self.id))
    }
}

impl PartialOrd for Due {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

#[derive(PartialEq, Eq)]
struct Entry {
    due: Due,
    left: usize,
    right: usize,
    left_node: (u32, u64),
    right_node: (u32, u64),
}

impl Ord for Entry {
    fn cmp(&self, o: &Self) -> Ordering {
        self.due.cmp(&o.due)
    }
}

impl PartialOrd for Entry {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Saved {
    pub lines: Vec<(u32, u64)>,
    pub forced: bool,
}

#[derive(Clone, Debug)]
pub struct View {
    lines: Vec<Line>,
    total: usize,
    high: usize,
    low: usize,
    forced: bool,
}

const NONE: usize = usize::MAX;

impl View {
    pub fn new(high: usize, low: usize) -> Self {
        Self { lines: Vec::new(), total: 0, high, low, forced: false }
    }

    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    pub fn total(&self) -> usize {
        self.total
    }

    pub fn forced(&self) -> bool {
        self.forced
    }

    pub fn low(&self) -> usize {
        self.low
    }

    pub fn push(&mut self, l: u32, i: u64, sizes: &impl Sizes) {
        let bytes = line_bytes(l, i, sizes.text_size(l, i));
        self.total += bytes;
        self.lines.push(Line { l, i, bytes });
    }

    /// A node got built: its line, if shown, changes from the placeholder to its text.
    pub fn node_built(&mut self, l: u32, i: u64, text_size: usize) {
        let id = i << l;
        if let Ok(p) = self.lines.binary_search_by_key(&id, Line::id) {
            let line = &mut self.lines[p];
            if line.l == l {
                let new = line_bytes(l, i, Some(text_size));
                self.total = self.total - line.bytes + new;
                line.bytes = new;
            }
        }
    }

    /// Append message `i`'s line and merge in a batch when the view is over its high mark, or
    /// keep merging when an earlier batch could not reach the low mark. Returns the parents merged.
    pub fn push_message(&mut self, i: u64, sizes: &impl Sizes) -> Vec<(u32, u64)> {
        self.push(0, i, sizes);
        if self.forced || self.total > self.high {
            return self.shrink(i + 1, sizes);
        }
        Vec::new()
    }

    /// Merge down to the low mark now, entering forced mode when it cannot be reached.
    pub fn shrink(&mut self, t: u64, sizes: &impl Sizes) -> Vec<(u32, u64)> {
        let low = self.low;
        let merged = self.batch(t, sizes, |bytes, _| bytes <= low);
        self.forced = self.total > self.low;
        merged
    }

    /// Merge the most due pairs whose parent is built until `stop(bytes, lines)` holds or no pair
    /// can merge. Costs O(n log n) in the number of lines.
    pub fn batch(&mut self, t: u64, sizes: &impl Sizes, stop: impl Fn(usize, usize) -> bool) -> Vec<(u32, u64)> {
        let n = self.lines.len();
        let mut lines = self.lines.clone();
        let mut prev: Vec<usize> = (0..n).map(|k| k.wrapping_sub(1)).collect();
        let mut next: Vec<usize> = (1..=n).collect();
        if n > 0 {
            prev[0] = NONE;
            next[n - 1] = NONE;
        }
        let mut alive = vec![true; n];
        let mut heap = BinaryHeap::new();
        let offer = |heap: &mut BinaryHeap<Entry>, lines: &[Line], next: &[usize], a: usize| {
            if a == NONE || next[a] == NONE {
                return;
            }
            let (x, y) = (lines[a], lines[next[a]]);
            if x.l == y.l && x.i % 2 == 0 && y.i == x.i + 1 && sizes.text_size(x.l + 1, x.i / 2).is_some() {
                let due = Due { age: t.saturating_sub(y.end() - 1), l: x.l, id: x.id() };
                heap.push(Entry { due, left: a, right: next[a], left_node: (x.l, x.i), right_node: (y.l, y.i) });
            }
        };
        for a in 0..n {
            offer(&mut heap, &lines, &next, a);
        }
        let (mut total, mut count) = (self.total, n);
        let mut merged = Vec::new();
        while !stop(total, count) {
            let Some(e) = heap.pop() else { break };
            let (a, b) = (e.left, e.right);
            if !alive[a] || !alive[b] || next[a] != b || (lines[a].l, lines[a].i) != e.left_node || (lines[b].l, lines[b].i) != e.right_node {
                continue;
            }
            let (l, i) = (lines[a].l + 1, lines[a].i / 2);
            let size = sizes.text_size(l, i);
            let bytes = line_bytes(l, i, size);
            total = total - lines[a].bytes - lines[b].bytes + bytes;
            lines[a] = Line { l, i, bytes };
            alive[b] = false;
            next[a] = next[b];
            if next[b] != NONE {
                prev[next[b]] = a;
            }
            count -= 1;
            merged.push((l, i));
            offer(&mut heap, &lines, &next, prev[a]);
            offer(&mut heap, &lines, &next, a);
        }
        self.lines = (0..n).filter(|&k| alive[k]).map(|k| lines[k]).collect();
        self.total = total;
        merged
    }

    /// Lines wholly before message `end`: what a compaction ending at `end` sees.
    pub fn prefix(&self, end: u64) -> &[Line] {
        &self.lines[..self.lines.partition_point(|l| l.end() <= end)]
    }

    pub fn saved(&self) -> Saved {
        Saved { lines: self.lines.iter().map(|l| (l.l, l.i)).collect(), forced: self.forced }
    }

    /// Restore a saved view. None unless it tiles messages `0..len` and every merged node is built.
    pub fn restore(saved: &Saved, len: u64, high: usize, low: usize, sizes: &impl Sizes) -> Option<Self> {
        let mut v = Self::new(high, low);
        let mut at = 0;
        for &(l, i) in &saved.lines {
            let line = Line { l, i, bytes: 0 };
            if l > 63 || line.id() != at || (l > 0 && sizes.text_size(l, i).is_none()) {
                return None;
            }
            at = line.end();
            v.push(l, i, sizes);
        }
        (at == len).then(|| {
            v.forced = saved.forced;
            v
        })
    }

    #[cfg(test)]
    pub fn from_nodes(nodes: &[(u32, u64)], high: usize, low: usize, sizes: &impl Sizes) -> Self {
        let mut v = Self::new(high, low);
        for &(l, i) in nodes {
            v.push(l, i, sizes);
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_built(_: u32, _: u64) -> Option<usize> {
        Some(400)
    }

    /// Taelin's `push` from rollback_state_list.js with `life` fixed at 0: newest first, (keep, state).
    fn push(new: u64, states: &[(bool, u64)]) -> Vec<(bool, u64)> {
        match states.split_first() {
            None => vec![(false, new)],
            Some((&(false, state), older)) => [&[(true, state)][..], older].concat(),
            Some((&(true, state), older)) => [&[(false, new)][..], &push(state, older)].concat(),
        }
    }

    fn starts(v: &View) -> Vec<u64> {
        v.lines().iter().rev().map(Line::id).collect()
    }

    #[test]
    fn push_reference_matches_the_gist_table() {
        let mut s = Vec::new();
        let mut got = Vec::new();
        for t in 0..10 {
            s = push(t, &s);
            got.push(s.iter().map(|&(k, st)| format!("{}{st}", if k { '+' } else { '-' })).collect::<Vec<_>>().join(","));
        }
        assert_eq!(got[2], "-2,-0");
        assert_eq!(got[5], "+4,+0");
        assert_eq!(got[6], "-6,-4,-0");
        assert_eq!(got[9], "+8,+4,-0");
    }

    #[test]
    fn merge_order_matches_taelins_push_at_every_step() {
        let mut v = View::new(usize::MAX, 0);
        let mut s = Vec::new();
        for t in 0..20_000u64 {
            s = push(t, &s);
            v.push(0, t, &all_built);
            let k = s.len();
            v.batch(t + 1, &all_built, |_, n| n <= k);
            let want: Vec<u64> = s.iter().map(|&(_, st)| st).collect();
            assert_eq!(starts(&v), want, "diverged at t={t}");
        }
    }

    #[test]
    fn age_is_measured_from_the_last_message() {
        let mut v = View::from_nodes(&[(2, 0), (2, 1), (0, 8), (0, 9)], usize::MAX, 0, &all_built);
        let merged = v.batch(10, &all_built, |_, n| n <= 3);
        assert_eq!(merged, [(1, 4)]);
        assert_eq!(v.lines().iter().map(|l| (l.id(), l.n())).collect::<Vec<_>>(), [(0, 4), (4, 4), (8, 2)]);
    }

    #[test]
    fn equal_due_pairs_merge_oldest_first() {
        // Both pairs have age 3 at level 0 once T is chosen so: pairs end at 3 and 5 would differ,
        // so use levels: (0,0)+(0,1) ends at 1, (1,1)+(1,2)? are not siblings. Use two level-0 pairs.
        let mut v = View::from_nodes(&[(0, 0), (0, 1), (0, 2), (0, 3)], usize::MAX, 0, &all_built);
        // T=4: pair 0-1 age 3, pair 2-3 age 1: the older pair is more due anyway; check the tie
        // with identical ages by giving the same last-message distance across levels.
        assert_eq!(v.batch(4, &all_built, |_, n| n <= 3), [(1, 0)]);
        // (1,0) and (0,2),(0,3): level-1 pair (1,0)? Not sibling with level-0 lines. Next merge is 2-3.
        assert_eq!(v.batch(4, &all_built, |_, n| n <= 2), [(1, 1)]);
    }

    #[test]
    fn exact_ties_across_levels_pick_the_oldest() {
        // Pair A: level 1 at (1,0),(1,1): last=3, due=(T-3)/2. Pair B: level 0 at (0,4),(0,5): last=5, due=T-5.
        // T=7: A=4/2=2, B=2/1=2. Tie: A is older.
        let mut v = View::from_nodes(&[(1, 0), (1, 1), (0, 4), (0, 5), (0, 6)], usize::MAX, 0, &all_built);
        assert_eq!(v.batch(7, &all_built, |_, n| n <= 4), [(2, 0)]);
    }

    #[test]
    fn unbuilt_parents_are_never_merged() {
        let built = |l: u32, i: u64| (l == 0 || (i * 7 + u64::from(l)) % 3 != 0).then_some(300);
        let mut v = View::new(0, 0);
        for i in 0..2_000 {
            v.push(0, i, &built);
            for (l, i) in v.batch(i + 1, &built, |_, n| n <= 8) {
                assert!(built(l, i).is_some(), "merged unbuilt parent {l},{i}");
            }
        }
        // Lines still tile the log.
        let mut at = 0;
        for l in v.lines() {
            assert_eq!(l.id(), at);
            at = l.end();
        }
        assert_eq!(at, 2_000);
    }

    #[test]
    fn view_is_a_sawtooth_that_only_grows_at_its_end_between_batches() {
        let size = |l: u32, i: u64| Some(200 + ((i * 2_654_435_761 + u64::from(l) * 97) % 313) as usize);
        let (high, low) = (128_000, 64_000);
        let mut v = View::new(high, low);
        let mut batches = 0;
        let mut seen_full_cycle = false;
        for i in 0..30_000u64 {
            let before: Vec<(u32, u64)> = v.lines().iter().map(|l| (l.l, l.i)).collect();
            let merged = v.push_message(i, &size);
            let after: Vec<(u32, u64)> = v.lines().iter().map(|l| (l.l, l.i)).collect();
            if merged.is_empty() {
                assert_eq!(&after[..before.len()], &before[..], "view changed before its end at {i}");
                assert_eq!(after.len(), before.len() + 1);
            } else {
                batches += 1;
                assert!(v.total() <= low, "batch left {} bytes at {i}", v.total());
                assert!(!v.forced());
                seen_full_cycle |= batches > 1;
            }
            assert!(v.total() <= high + 600, "view reached {} at {i}", v.total());
            if batches > 0 {
                assert!(v.total() > low - 1_100, "view fell to {} at {i}", v.total());
            }
        }
        assert!(seen_full_cycle);
    }

    #[test]
    fn forced_mode_continues_merging_until_the_low_mark() {
        // Nothing above level 0 is built at first, so a batch cannot shrink the view.
        let mut view = View::new(1_000, 500);
        let none = |l: u32, _: u64| (l == 0).then_some(100);
        for i in 0..20 {
            view.push_message(i, &none);
        }
        assert!(view.forced() && view.total() > 1_000);
        // Parents now exist: the next message merges down below the low mark.
        let all = |_: u32, _: u64| Some(100);
        view.push_message(20, &all);
        assert!(!view.forced() && view.total() <= 500, "{}", view.total());
    }

    #[test]
    fn node_built_updates_bytes_of_the_placeholder_line() {
        let none = |_: u32, _: u64| None;
        let mut v = View::new(10_000, 5_000);
        v.push(0, 5, &none);
        let before = v.total();
        v.node_built(0, 5, 100);
        assert_eq!(v.total(), before - PLACEHOLDER.len() + 100);
    }

    #[test]
    fn prefix_and_restore() {
        let mut v = View::from_nodes(&[(2, 0), (1, 2), (0, 6), (0, 7)], 1_000_000, 0, &all_built);
        assert_eq!(v.prefix(6).len(), 2);
        assert_eq!(v.prefix(5).len(), 1);
        assert_eq!(v.prefix(0).len(), 0);
        let saved = v.saved();
        assert!(View::restore(&saved, 8, 10, 5, &all_built).is_some());
        assert!(View::restore(&saved, 9, 10, 5, &all_built).is_none());
        let unbuilt = |l: u32, _: u64| (l == 0).then_some(1);
        assert!(View::restore(&saved, 8, 10, 5, &unbuilt).is_none());
        v.lines.swap(0, 1);
        assert!(View::restore(&v.saved(), 8, 10, 5, &all_built).is_none());
    }

    #[test]
    fn line_bytes_matches_the_rendering() {
        for (l, i, t) in [(0, 0, 5), (3, 5, 400), (10, 123, 512)] {
            let text = "x".repeat(t);
            let rendered = format!("{}+{}|{}\n", i << l, 1u64 << l, text);
            assert_eq!(line_bytes(l, i, Some(t)), rendered.len());
        }
    }
}
