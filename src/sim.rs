//! Deterministic simulator: the real memory, view, and scheduler code against a stub compactor
//! and a model of Anthropic's prompt cache (entries only at marks, a 20-block lookback, 5-minute
//! expiry). It reports how many view lines each message rewrites and how much of each call's
//! prefix is read from cache.

use crate::error::Result;
use crate::log::Kind;
use crate::memory::{Limits, Memory};
use crate::sched::Job;
use crate::view::Line;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::path::Path;

const BLOCK_LINES: usize = 4;
const LOOKBACK_BLOCKS: usize = 20;

#[derive(Clone, Debug)]
pub struct SimConfig {
    pub messages: u64,
    /// Milliseconds between messages.
    pub interval_ms: u64,
    /// Milliseconds a stub compaction takes.
    pub latency_ms: u64,
    /// Milliseconds until a request's cache entries can be read by others.
    pub ttft_ms: u64,
    pub workers: usize,
    /// Merge the chat view down to this many lines after every message, instead of batching.
    pub every_message_lines: Option<usize>,
    /// Send the first call on a cold prefix alone, as the spec requires.
    pub prime_first: bool,
    pub system_bytes: usize,
    pub ttl_ms: u64,
    pub limits: (usize, usize, usize, usize),
    pub seed: u64,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            messages: 3_000,
            interval_ms: 30_000,
            latency_ms: 4_000,
            ttft_ms: 1_500,
            workers: 8,
            every_message_lines: None,
            prime_first: true,
            system_bytes: 6_000,
            ttl_ms: 300_000,
            limits: (128_000, 64_000, 32_000, 16_000),
            seed: 1,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct SimReport {
    pub messages: u64,
    pub mean_view_lines: f64,
    pub mean_view_bytes: f64,
    /// Mean view lines each turn sends that differ from the previous turn's view, over the second
    /// half of the run. The line a message adds counts as one.
    pub mean_rewritten_lines: f64,
    pub turn_read_ratio: f64,
    pub compaction_read_ratio: f64,
    pub compactions: u64,
    pub max_unbuilt_in_view: usize,
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
}

fn mix(h: u64, k: u64) -> u64 {
    (h ^ k).wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(29)
}

struct Cache {
    ttl: u64,
    entries: HashMap<u64, u64>,
    pending: BinaryHeap<Reverse<(u64, u64)>>,
}

impl Cache {
    fn settle(&mut self, now: u64) {
        while let Some(&Reverse((t, h))) = self.pending.peek() {
            if t > now {
                break;
            }
            self.pending.pop();
            self.entries.insert(h, t + self.ttl);
        }
    }

    fn live(&self, h: u64, now: u64) -> bool {
        self.entries.get(&h).is_some_and(|&exp| exp > now)
    }

    /// One request: a fixed system part, view lines, and a fresh tail. Returns (bytes read, bytes sent).
    fn request(&mut self, now: u64, ttft: u64, system: usize, lines: &[(u64, usize)], tail: usize) -> (usize, usize) {
        self.settle(now);
        let mut hashes = vec![mix(0, system as u64)];
        let mut bytes = vec![system];
        for &(k, b) in lines {
            hashes.push(mix(*hashes.last().expect("non-empty"), k));
            bytes.push(bytes.last().expect("non-empty") + b);
        }
        let total = bytes[lines.len()] + tail;
        let last_block = lines.len() / BLOCK_LINES * BLOCK_LINES;
        let mut read = 0;
        let mut at = last_block;
        let floor = last_block.saturating_sub(LOOKBACK_BLOCKS * BLOCK_LINES);
        loop {
            if self.live(hashes[at], now) {
                read = bytes[at];
                self.entries.insert(hashes[at], now + self.ttl);
                break;
            }
            if at <= floor || at == 0 {
                break;
            }
            at -= BLOCK_LINES;
        }
        for mark in [0, last_block] {
            self.pending.push(Reverse((now + ttft, hashes[mark])));
        }
        (read, total)
    }
}

fn line_key(mem: &Memory, line: &Line) -> u64 {
    mix(mix(u64::from(line.l), line.i), u64::from(mem.tree.is_built(line.l, line.i)))
}

fn keyed(mem: &Memory, lines: &[Line]) -> Vec<(u64, usize)> {
    lines.iter().map(|l| (line_key(mem, l), l.bytes)).collect()
}

struct Flight {
    done: u64,
    job: Job,
    bytes: usize,
}

pub fn run(dir: &Path, cfg: &SimConfig) -> Result<SimReport> {
    let (high, low, chigh, clow) = cfg.limits;
    let chat_high = if cfg.every_message_lines.is_some() { usize::MAX } else { high };
    let limits = Limits { view_high: chat_high, view_low: low, compaction_high: chigh, compaction_low: clow };
    let mut mem = Memory::open(dir, &limits, cfg.workers)?;
    let mut rng = Rng(cfg.seed);
    let mut cache = Cache { ttl: cfg.ttl_ms, entries: HashMap::new(), pending: BinaryHeap::new() };
    let mut flights: Vec<Flight> = Vec::new();
    let mut report = SimReport::default();
    let (mut turn_read, mut turn_total) = (0usize, 0usize);
    let (mut rewritten, mut rewritten_n) = (0usize, 0usize);
    let (mut lines_sum, mut bytes_sum) = (0usize, 0usize);
    let mut now = 0;

    let mut comp_stats = (0usize, 0usize, 0u64);
    for m in 0..cfg.messages {
        let next_msg = now + cfg.interval_ms;
        // Let compactions finish before the next message.
        loop {
            flights.sort_by_key(|f| Reverse(f.done));
            match flights.last() {
                Some(f) if f.done <= next_msg => {
                    let f = flights.pop().expect("peeked");
                    now = f.done;
                    let (l, i) = match f.job {
                        Job::Leaf(i) => (0, i),
                        Job::Merge(l, i) => (l, i),
                    };
                    mem.finish(crate::tree::Node::new(l, i, "s".repeat(f.bytes)), f.job)?;
                    start_burst(&mut mem, &mut cache, &mut flights, &mut rng, now, cfg, &mut comp_stats);
                }
                _ => break,
            }
        }
        now = next_msg;
        // The turn sees the view before the new message.
        let lines = mem.chat.lines().to_vec();
        let keyed_lines = keyed(&mem, &lines);
        let (read, total) = cache.request(now, cfg.ttft_ms, cfg.system_bytes, &keyed_lines, 800);
        turn_read += read;
        turn_total += total;
        let keys: Vec<u64> = keyed_lines.iter().map(|k| k.0).collect();
        let size = match rng.next() % 10 {
            0..=2 => 40 + rng.next() as usize % 400,
            3..=8 => 600 + rng.next() as usize % 2_400,
            _ => 3_000 + rng.next() as usize % 9_000,
        };
        let kind = if m % 2 == 0 { Kind::User } else { Kind::Agent };
        mem.append(kind, &"x".repeat(size), None)?;
        if let Some(budget) = cfg.every_message_lines {
            // The idealized baseline has every summary available at once, as in the push model.
            while let Some(job) = mem.sched.next(now) {
                let (l, i) = match job {
                    Job::Leaf(i) => (0, i),
                    Job::Merge(l, i) => (l, i),
                };
                mem.finish(crate::tree::Node::new(l, i, "s".repeat(380 + (rng.next() % 133) as usize)), job)?;
            }
            mem.chat.batch(m + 1, &mem.tree, |_, n| n <= budget);
        }
        let after: Vec<u64> = keyed(&mem, mem.chat.lines()).iter().map(|k| k.0).collect();
        let common = keys.iter().zip(&after).take_while(|(a, b)| a == b).count();
        if m >= cfg.messages / 2 {
            rewritten += after.len() - common;
            rewritten_n += 1;
        }
        lines_sum += after.len();
        bytes_sum += mem.chat.total();
        let unbuilt = mem.chat.lines().iter().filter(|l| l.l == 0 && !mem.tree.is_built(0, l.i)).count();
        report.max_unbuilt_in_view = report.max_unbuilt_in_view.max(unbuilt);
        mem.sched.kick();
        start_burst(&mut mem, &mut cache, &mut flights, &mut rng, now, cfg, &mut comp_stats);
    }
    report.messages = cfg.messages;
    report.compactions = comp_stats.2;
    report.mean_view_lines = lines_sum as f64 / cfg.messages as f64;
    report.mean_view_bytes = bytes_sum as f64 / cfg.messages as f64;
    report.mean_rewritten_lines = rewritten as f64 / rewritten_n.max(1) as f64;
    report.turn_read_ratio = turn_read as f64 / turn_total.max(1) as f64;
    report.compaction_read_ratio = comp_stats.0 as f64 / comp_stats.1.max(1) as f64;
    Ok(report)
}

fn start_burst(mem: &mut Memory, cache: &mut Cache, flights: &mut Vec<Flight>, rng: &mut Rng, now: u64, cfg: &SimConfig, stats: &mut (usize, usize, u64)) {
    let mut jobs = Vec::new();
    while let Some(job) = mem.sched.next(now) {
        jobs.push(job);
    }
    let mut at = now;
    for (n, job) in jobs.into_iter().enumerate() {
        let end = match job {
            Job::Leaf(i) => i,
            Job::Merge(l, i) => (i + 1) << l,
        };
        let lines = keyed(mem, mem.comp.prefix(end));
        let tail = 1_000 + rng.next() as usize % 600;
        let (read, total) = cache.request(at, cfg.ttft_ms, cfg.system_bytes, &lines, tail);
        stats.0 += read;
        stats.1 += total;
        stats.2 += 1;
        if cfg.prime_first && n == 0 && read * 2 < total {
            at = now + cfg.ttft_ms;
        }
        let bytes = 380 + (rng.next() % 133) as usize;
        flights.push(Flight { done: at + cfg.latency_ms, job, bytes });
    }
}

/// The gist's comparison of merge timing, on the view alone with every summary built at once:
/// mean view lines rewritten per message when the view is merged to a fixed line count after every
/// message, and when it is batched between 64 KB and 128 KB. Returns (every message, batched,
/// mean lines of the batched view), measured over the second half of the run.
pub fn compare_gist(messages: u64, lines: usize) -> (f64, f64, f64) {
    use crate::view::View;
    let size = |_: u32, _: u64| Some(500 - 11);
    let measure = |mut v: View, step: &dyn Fn(&mut View, u64)| {
        let (mut rewritten, mut count, mut mean_lines) = (0usize, 0usize, 0usize);
        for t in 0..messages {
            let before = v.lines().to_vec();
            step(&mut v, t);
            if t >= messages / 2 {
                let common = before.iter().zip(v.lines()).take_while(|(a, b)| a == b).count();
                rewritten += v.lines().len() - common;
                mean_lines += v.lines().len();
                count += 1;
            }
        }
        (rewritten as f64 / count as f64, mean_lines as f64 / count as f64)
    };
    let (every, _) = measure(View::new(usize::MAX, 0), &|v, t| {
        v.push(0, t, &size);
        v.batch(t + 1, &size, |_, n| n <= lines);
    });
    let (b, mean) = measure(View::new(128_000, 64_000), &|v, t| {
        v.push_message(t, &size);
    });
    (every, b, mean)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(cfg: &SimConfig) -> SimReport {
        let d = tempfile::tempdir().unwrap();
        run(d.path(), cfg).unwrap()
    }

    #[test]
    fn batching_rewrites_far_fewer_lines_than_merging_every_message() {
        // The gist reports about 53 of 192 lines per message against about 2, over 30,000 messages.
        let (every, batched, lines) = compare_gist(30_000, 192);
        assert!((45.0..=60.0).contains(&every), "every-message rewrote {every:.1} lines");
        assert!(batched < 3.0, "batched rewrote {batched:.2} lines");
        assert!((150.0..=230.0).contains(&lines), "batched view held {lines:.0} lines");
    }

    #[test]
    fn steady_chat_reads_most_of_every_prefix_from_cache() {
        let r = report(&SimConfig { messages: 800, ..Default::default() });
        assert!(r.turn_read_ratio > 0.9, "{r:?}");
        assert!(r.compaction_read_ratio > 0.8, "{r:?}");
        assert!(r.mean_rewritten_lines < 3.0, "{r:?}");
        assert!(r.mean_view_bytes > 60_000.0 && r.mean_view_bytes < 130_000.0, "{r:?}");
    }

    #[test]
    fn the_same_seed_gives_the_same_report() {
        let cfg = SimConfig { messages: 150, ..Default::default() };
        assert_eq!(format!("{:?}", report(&cfg)), format!("{:?}", report(&cfg)));
    }

    #[test]
    fn sending_the_first_call_alone_helps_a_burst_on_a_cold_prefix() {
        let burst = |prime_first| report(&SimConfig { messages: 200, interval_ms: 100, prime_first, ..Default::default() });
        let (with, without) = (burst(true), burst(false));
        assert!(with.compaction_read_ratio > without.compaction_read_ratio, "{with:?} {without:?}");
    }
}
