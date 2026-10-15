//! Which compactions to start next. Ready nodes sit in ordered sets; the tree is never scanned.

use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Job {
    Leaf(u64),
    Merge(u32, u64),
}

impl Job {
    /// Sort key for merges: the last message covered, so the oldest merge goes first.
    fn merge_key(l: u32, i: u64) -> (u64, u32, u64) {
        ((i + 1) << l, l, i)
    }
}

/// A leaf starts only when fewer than this many unbuilt leaves precede it.
pub const LEAF_WINDOW: usize = 8;
const BACKOFF_SECS: [u64; 4] = [10, 30, 60, 60];

pub struct Sched {
    workers: usize,
    /// Leaves not built yet, whether ready, running, or backing off.
    unbuilt_leaves: BTreeSet<u64>,
    ready_leaves: BTreeSet<u64>,
    ready_merges: BTreeSet<(u64, u32, u64)>,
    running: HashSet<Job>,
    /// Failure count and earliest retry time (ms) per job; `u64::MAX` means wait for `kick`.
    fails: HashMap<Job, (u32, u64)>,
    consecutive_fails: u32,
    backoff_until: u64,
    paused: bool,
}

impl Sched {
    pub fn new(workers: usize) -> Self {
        Self {
            workers,
            unbuilt_leaves: BTreeSet::new(),
            ready_leaves: BTreeSet::new(),
            ready_merges: BTreeSet::new(),
            running: HashSet::new(),
            fails: HashMap::new(),
            consecutive_fails: 0,
            backoff_until: 0,
            paused: false,
        }
    }

    /// A message's leaf exists. It is ready once its message is complete.
    pub fn add_leaf(&mut self, i: u64, ready: bool) {
        self.unbuilt_leaves.insert(i);
        if ready {
            self.ready_leaves.insert(i);
        }
    }

    pub fn leaf_ready(&mut self, i: u64) {
        if self.unbuilt_leaves.contains(&i) && !self.running.contains(&Job::Leaf(i)) {
            self.ready_leaves.insert(i);
        }
    }

    pub fn add_merge(&mut self, l: u32, i: u64) {
        self.ready_merges.insert(Job::merge_key(l, i));
    }

    pub fn built(&mut self, job: Job) {
        self.running.remove(&job);
        self.fails.remove(&job);
        self.consecutive_fails = 0;
        self.backoff_until = 0;
        match job {
            Job::Leaf(i) => {
                self.unbuilt_leaves.remove(&i);
                self.ready_leaves.remove(&i);
            }
            Job::Merge(l, i) => {
                self.ready_merges.remove(&Job::merge_key(l, i));
            }
        }
    }

    fn backing_off(&self, job: Job, now: u64) -> bool {
        self.fails.get(&job).is_some_and(|&(_, t)| t > now)
    }

    fn leaf_rank(&self, i: u64) -> usize {
        self.unbuilt_leaves.range(..i).take(LEAF_WINDOW).count()
    }

    fn next_leaf(&self, now: u64) -> Option<u64> {
        for &i in &self.ready_leaves {
            if self.leaf_rank(i) >= LEAF_WINDOW {
                return None;
            }
            if !self.backing_off(Job::Leaf(i), now) {
                return Some(i);
            }
        }
        None
    }

    fn next_merge(&self, now: u64) -> Option<(u32, u64)> {
        self.ready_merges.iter().find(|&&(_, l, i)| !self.backing_off(Job::Merge(l, i), now)).map(|&(_, l, i)| (l, i))
    }

    fn running_merges(&self) -> usize {
        self.running.iter().filter(|j| matches!(j, Job::Merge(..))).count()
    }

    /// The next job to start, marked running. Leaves go first, oldest first, but the last free
    /// worker goes to a ready merge when none is running, so the views can still shrink while
    /// leaves keep every worker busy.
    pub fn next(&mut self, now: u64) -> Option<Job> {
        if self.running.len() >= self.workers || self.paused || now < self.backoff_until {
            return None;
        }
        let merge = self.next_merge(now);
        let leaf = self.next_leaf(now);
        let last_worker = self.running.len() + 1 == self.workers;
        let job = match (leaf, merge) {
            (Some(_), Some((l, i))) if last_worker && self.workers > 1 && self.running_merges() == 0 => Job::Merge(l, i),
            (Some(i), _) => Job::Leaf(i),
            (None, Some((l, i))) => Job::Merge(l, i),
            (None, None) => return None,
        };
        match job {
            Job::Leaf(i) => self.ready_leaves.remove(&i),
            Job::Merge(l, i) => self.ready_merges.remove(&Job::merge_key(l, i)),
        };
        self.running.insert(job);
        Some(job)
    }

    /// A call failed for good: requeue the job after 10 seconds, then 30, then 60, then 60, and
    /// stop retrying after that until a new message arrives (`kick`).
    pub fn failed(&mut self, job: Job, now: u64) {
        self.running.remove(&job);
        self.consecutive_fails += 1;
        let e = self.fails.entry(job).or_insert((0, 0));
        e.0 += 1;
        e.1 = match BACKOFF_SECS.get(e.0 as usize - 1) {
            Some(&secs) => now.saturating_add(secs * 1000),
            None => u64::MAX,
        };
        if self.consecutive_fails > BACKOFF_SECS.len() as u32 {
            self.paused = true;
        } else if e.1 < u64::MAX {
            self.backoff_until = self.backoff_until.max(e.1);
        }
        match job {
            Job::Leaf(i) => self.ready_leaves.insert(i),
            Job::Merge(l, i) => self.ready_merges.insert(Job::merge_key(l, i)),
        };
    }

    /// Pause all scheduling until `kick` (used on quota exhaustion).
    pub fn pause(&mut self) {
        self.paused = true;
    }

    /// A new message arrived: retry everything that is waiting out a failure.
    pub fn kick(&mut self) {
        self.paused = false;
        self.consecutive_fails = 0;
        self.backoff_until = 0;
        for v in self.fails.values_mut() {
            v.1 = 0;
        }
    }

    pub fn failures(&self, job: Job) -> u32 {
        self.fails.get(&job).map_or(0, |f| f.0)
    }

    /// Earliest time (ms) a backed-off job becomes startable.
    pub fn next_retry(&self, now: u64) -> Option<u64> {
        if self.paused {
            return None;
        }
        self.fails.values().map(|f| f.1.max(self.backoff_until)).filter(|&t| t > now && t < u64::MAX).min()
    }

    pub fn has_startable(&self, now: u64) -> bool {
        !self.paused && now >= self.backoff_until && (self.next_leaf(now).is_some() || self.next_merge(now).is_some())
    }

    /// Whether every unbuilt leaf before message `m` has failed at least `times` times.
    pub fn leaves_before_all_failed(&self, m: u64, times: u32) -> bool {
        self.unbuilt_leaves.range(..m).all(|&i| self.failures(Job::Leaf(i)) >= times)
    }

    pub fn unbuilt_before(&self, m: u64) -> usize {
        self.unbuilt_leaves.range(..m).count()
    }

    pub fn unbuilt_leaves(&self) -> usize {
        self.unbuilt_leaves.len()
    }

    pub fn ready_merges(&self) -> usize {
        self.ready_merges.len()
    }

    pub fn running(&self) -> usize {
        self.running.len()
    }

    pub fn is_idle(&self) -> bool {
        self.running.is_empty() && self.ready_leaves.is_empty() && self.ready_merges.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaves(n: u64) -> Sched {
        let mut s = Sched::new(8);
        for i in 0..n {
            s.add_leaf(i, true);
        }
        s
    }

    #[test]
    fn leaves_start_oldest_first_within_the_window() {
        let mut s = leaves(30);
        let started: Vec<Job> = std::iter::from_fn(|| s.next(0)).collect();
        assert_eq!(started, (0..8).map(Job::Leaf).collect::<Vec<_>>());
        s.built(Job::Leaf(0));
        assert_eq!(s.next(0), Some(Job::Leaf(8)));
    }

    #[test]
    fn a_leaf_waits_while_eight_earlier_leaves_are_unbuilt() {
        let mut s = Sched::new(16);
        for i in 0..12 {
            s.add_leaf(i, true);
        }
        let started: Vec<Job> = std::iter::from_fn(|| s.next(0)).collect();
        // Leaf 7 has 7 unbuilt leaves before it; leaf 8 has 8.
        assert_eq!(started, (0..8).map(Job::Leaf).collect::<Vec<_>>());
    }

    #[test]
    fn a_page_run_leaf_is_not_started_until_marked_ready() {
        let mut s = Sched::new(8);
        s.add_leaf(0, false);
        assert_eq!(s.next(0), None);
        s.leaf_ready(0);
        assert_eq!(s.next(0), Some(Job::Leaf(0)));
    }

    #[test]
    fn one_worker_is_reserved_for_merges_when_leaves_fill_the_rest() {
        let mut s = Sched::new(3);
        for i in 0..50 {
            s.add_leaf(i, true);
        }
        s.add_merge(1, 40);
        assert_eq!(s.next(0), Some(Job::Leaf(0)));
        assert_eq!(s.next(0), Some(Job::Leaf(1)));
        assert_eq!(s.next(0), Some(Job::Merge(1, 40)));
        assert_eq!(s.next(0), None);
        s.built(Job::Leaf(0));
        // A merge is running already: the freed worker takes a leaf again.
        assert_eq!(s.next(0), Some(Job::Leaf(2)));
    }

    #[test]
    fn merges_run_oldest_first_by_last_message() {
        let mut s = Sched::new(8);
        s.add_merge(2, 5); // ends at 24
        s.add_merge(1, 2); // ends at 6
        s.add_merge(3, 0); // ends at 8
        assert_eq!(std::iter::from_fn(|| s.next(0)).collect::<Vec<_>>(), [Job::Merge(1, 2), Job::Merge(3, 0), Job::Merge(2, 5)]);
    }

    #[test]
    fn failures_back_off_10_30_60_60_and_a_message_retries_at_once() {
        let mut s = Sched::new(8);
        s.add_leaf(0, true);
        let job = s.next(0).unwrap();
        let mut now = 0;
        for wait in [10_000, 30_000, 60_000, 60_000] {
            s.failed(job, now);
            assert_eq!(s.next(now + wait - 1), None);
            assert_eq!(s.next_retry(now), Some(now + wait));
            now += wait;
            assert_eq!(s.next(now), Some(job));
        }
        assert_eq!(s.failures(job), 4);
        s.failed(job, now);
        assert_eq!(s.next(now + 1_000_000), None);
        assert_eq!(s.next_retry(now), None);
        assert!(!s.has_startable(now + 1_000_000));
        s.kick();
        assert_eq!(s.next(now), Some(job));
        s.built(job);
        assert_eq!(s.failures(job), 0);
        assert!(s.is_idle());
    }

    #[test]
    fn waiting_turn_can_proceed_after_three_failures_each() {
        let mut s = Sched::new(8);
        s.add_leaf(0, true);
        s.add_leaf(1, true);
        assert!(!s.leaves_before_all_failed(2, 3));
        for i in 0..2 {
            let job = Job::Leaf(i);
            for _ in 0..3 {
                s.next(u64::MAX / 2).unwrap_or(job);
                s.failed(job, 0);
            }
        }
        assert!(s.leaves_before_all_failed(2, 3));
        assert!(s.leaves_before_all_failed(0, 3), "no unbuilt leaf before message 0");
    }
}
