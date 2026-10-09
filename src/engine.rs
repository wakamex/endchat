//! The running profile: memory behind a lock, the compaction workers, the cost ledger, and the
//! queue of messages for the turn in progress.

use crate::aop::Aop;
use crate::compact::{Call, Compactor};
use crate::error::Result;
use crate::log::{Kind, Message};
use crate::memory::{Limits, Memory};
use crate::profile::Config;
use crate::prompts;
use crate::sched::Job;
use crate::tree::{Node, Usage};
use crate::util::now_iso;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::{oneshot, watch};

/// Cache entries live 5 minutes from their last read; past this a prefix counts as cold.
const COLD_AFTER: Duration = Duration::from_secs(240);
/// How long calls queued behind a priming call wait for it before going anyway.
const PRIME_WAIT: Duration = Duration::from_secs(150);
/// Calls in the rolling window of the cache-read ratio.
const RATIO_WINDOW: usize = 200;

#[derive(Debug, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub date: String,
    /// `turn` or `compaction`.
    pub kind: String,
    pub run: String,
    pub model: Option<String>,
    pub usage: Option<Usage>,
    pub ok: bool,
}

#[derive(Debug, Serialize)]
pub struct Status {
    pub messages: u64,
    pub view_lines: usize,
    pub view_bytes: usize,
    pub view_forced: bool,
    pub compaction_view_bytes: usize,
    pub unsummarized: usize,
    pub ready_merges: usize,
    pub running_calls: usize,
    pub turn_cache_read_ratio: Option<f64>,
    pub compaction_cache_read_ratio: Option<f64>,
    pub last_error: Option<String>,
}

impl Status {
    pub fn render(&self) -> String {
        let pct = |r: Option<f64>| r.map_or("none yet".to_owned(), |r| format!("{:.1}%", r * 100.0));
        format!(
            "messages: {}\nview: {} lines, {} bytes{}\ncompaction view: {} bytes\nunsummarized messages: {}\nmerges ready: {}\nrunning calls: {}\ncache read, turns: {}\ncache read, compactions: {}\nlast error: {}\n",
            self.messages,
            self.view_lines,
            self.view_bytes,
            if self.view_forced { " (forced mode)" } else { "" },
            self.compaction_view_bytes,
            self.unsummarized,
            self.ready_merges,
            self.running_calls,
            pct(self.turn_cache_read_ratio),
            pct(self.compaction_cache_read_ratio),
            self.last_error.as_deref().unwrap_or("none")
        )
    }
}

pub struct Engine {
    pub dir: PathBuf,
    pub cfg: Config,
    pub system_file: PathBuf,
    pub scratch: PathBuf,
    mem: Mutex<Memory>,
    compactor: Compactor,
    changed: watch::Sender<u64>,
    started: Instant,
    last_call: Mutex<Option<Instant>>,
    last_error: Mutex<Option<String>>,
    stop: AtomicBool,
    /// Messages sent while a turn runs, in order; the turn takes them between tool calls.
    pub inbox: Mutex<VecDeque<String>>,
    pub turn_running: AtomicBool,
}

impl Engine {
    pub fn open(dir: &Path, cfg: Config) -> Result<Arc<Self>> {
        let scratch = dir.join(".tmp");
        fs::create_dir_all(&scratch)?;
        for e in fs::read_dir(&scratch)?.flatten() {
            let _ = fs::remove_file(e.path());
        }
        let instructions = fs::read_to_string(dir.join("instructions.md")).ok();
        let system_file = dir.join("system-prompt.txt");
        fs::write(&system_file, prompts::system(instructions.as_deref()))?;
        let mem = Memory::open(dir, &Limits::from(&cfg.limits), cfg.compactor.workers)?;
        let aop = Aop { bin: cfg.aop_bin.clone(), cwd: dir.to_owned() };
        let compactor = Compactor::new(aop, cfg.compactor.clone(), system_file.clone(), scratch.clone());
        let (changed, _) = watch::channel(0);
        Ok(Arc::new(Self {
            dir: dir.to_owned(),
            cfg,
            system_file,
            scratch,
            mem: Mutex::new(mem),
            compactor,
            changed,
            started: Instant::now(),
            last_call: Mutex::new(None),
            last_error: Mutex::new(None),
            stop: AtomicBool::new(false),
            inbox: Mutex::new(VecDeque::new()),
            turn_running: AtomicBool::new(false),
        }))
    }

    pub fn mem(&self) -> MutexGuard<'_, Memory> {
        self.mem.lock().expect("memory lock poisoned")
    }

    pub fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// Wake the workers and anything waiting on summaries.
    pub fn changed(&self) {
        self.changed.send_modify(|v| *v += 1);
    }

    /// Log a message and wake the workers, which may have new summaries to build.
    pub fn append(&self, kind: Kind, text: &str, run: Option<&str>) -> Result<Vec<Message>> {
        let msgs = self.mem().append(kind, text, run)?;
        self.changed();
        Ok(msgs)
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.changed();
    }

    pub fn set_error(&self, e: impl Into<String>) {
        *self.last_error.lock().expect("error lock poisoned") = Some(e.into());
    }

    fn ledger_path(&self) -> PathBuf {
        self.dir.join("ledger.jsonl")
    }

    pub fn record_calls(&self, kind: &str, calls: &[Call]) {
        let Ok(mut f) = OpenOptions::new().create(true).append(true).open(self.ledger_path()) else { return };
        for c in calls {
            let e = LedgerEntry { date: now_iso(), kind: kind.into(), run: c.run_id.clone(), model: c.model.clone(), usage: c.usage, ok: c.ok };
            if let Ok(line) = serde_json::to_string(&e) {
                // One write per entry: concurrent workers append to the same file.
                let _ = f.write_all(format!("{line}\n").as_bytes());
            }
        }
        let _ = f.sync_data();
    }

    pub fn ledger(&self) -> Vec<LedgerEntry> {
        fs::read_to_string(self.ledger_path()).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    /// Cached share of input tokens over the last calls of a kind.
    pub fn cache_ratio(&self, kind: &str) -> Option<f64> {
        let entries: Vec<LedgerEntry> = self.ledger().into_iter().filter(|e| e.kind == kind).collect();
        let (mut read, mut total) = (0u64, 0u64);
        for u in entries.iter().rev().take(RATIO_WINDOW).filter_map(|e| e.usage) {
            read += u.cached_input_tokens;
            total += u.input_tokens;
        }
        (total > 0).then(|| read as f64 / total as f64)
    }

    pub fn status(&self) -> Status {
        let m = self.mem();
        Status {
            messages: m.log.len(),
            view_lines: m.chat.lines().len(),
            view_bytes: m.chat.total(),
            view_forced: m.chat.forced(),
            compaction_view_bytes: m.comp.total(),
            unsummarized: m.sched.unbuilt_leaves(),
            ready_merges: m.sched.ready_merges(),
            running_calls: m.sched.running(),
            turn_cache_read_ratio: None,
            compaction_cache_read_ratio: None,
            last_error: self.last_error.lock().expect("error lock poisoned").clone(),
        }
        .with_ratios(self.cache_ratio("turn"), self.cache_ratio("compaction"))
    }

    /// Start compactions as the scheduler releases them, until `stop`. The first call on a cold
    /// prefix goes alone; the others in its burst wait until it has answered.
    pub async fn pump(self: Arc<Self>) {
        let mut rx = self.subscribe();
        while !self.stop.load(Ordering::SeqCst) {
            let (jobs, retry) = {
                let mut m = self.mem();
                let now = self.now_ms();
                let mut jobs = Vec::new();
                while let Some(j) = m.sched.next(now) {
                    jobs.push(j);
                }
                (jobs, m.sched.next_retry(now))
            };
            self.launch(jobs);
            let wait = retry.map_or(Duration::from_secs(3600), |t| Duration::from_millis(t.saturating_sub(self.now_ms()) + 1));
            tokio::select! {
                _ = rx.changed() => {}
                _ = tokio::time::sleep(wait) => {}
            }
        }
    }

    fn launch(self: &Arc<Self>, jobs: Vec<Job>) {
        let cold = self.last_call.lock().expect("lock poisoned").is_none_or(|t| t.elapsed() > COLD_AFTER);
        let prime = cold && jobs.len() > 1;
        let (reply_tx, reply_rx) = oneshot::channel::<()>();
        let (open_tx, open_rx) = watch::channel(!prime);
        if prime {
            tokio::spawn(async move {
                let _ = tokio::time::timeout(PRIME_WAIT, reply_rx).await;
                let _ = open_tx.send(true);
            });
        }
        let mut signal = prime.then_some(reply_tx);
        for (n, job) in jobs.into_iter().enumerate() {
            let me = self.clone();
            let gate = (n > 0).then(|| open_rx.clone());
            let signal = if n == 0 { signal.take() } else { None };
            tokio::spawn(async move { me.run_job(job, gate, signal).await });
        }
    }

    async fn run_job(self: Arc<Self>, job: Job, wait_for: Option<watch::Receiver<bool>>, signal: Option<oneshot::Sender<()>>) {
        if let Some(mut rx) = wait_for {
            let _ = rx.wait_for(|open| *open).await;
        }
        let task = self.mem().task(job);
        let task = match task {
            Ok(t) => t,
            Err(e) => return self.job_failed(job, e.to_string()),
        };
        *self.last_call.lock().expect("lock poisoned") = Some(Instant::now());
        let out = self.compactor.build_signalling(&task.prompt, task.source_bytes, signal).await;
        self.record_calls("compaction", &out.calls);
        match out.result {
            Ok(b) => {
                let (l, i) = match job {
                    Job::Leaf(i) => (0, i),
                    Job::Merge(l, i) => (l, i),
                };
                let mut node = Node::new(l, i, b.text);
                (node.run, node.model, node.usage) = (Some(b.run), b.model, Some(b.usage));
                if let Err(e) = self.mem().finish(node, job) {
                    return self.job_failed(job, e.to_string());
                }
                self.changed();
            }
            Err(e) => self.job_failed(job, e),
        }
    }

    fn job_failed(&self, job: Job, why: String) {
        let now = self.now_ms();
        let mut m = self.mem();
        m.sched.failed(job, now);
        let name = match job {
            Job::Leaf(i) => format!("message {i}"),
            Job::Merge(l, i) => format!("line {}", crate::memory::line_name(l, i)),
        };
        let msg = format!("compaction of {name} failed ({} so far): {why}", m.sched.failures(job));
        eprintln!("endchat: {msg}");
        self.set_error(msg);
        drop(m);
        self.changed();
    }

    /// Wait until the scheduler has nothing left to do, for tests and the live checks.
    pub async fn wait_idle(&self, limit: Duration) -> bool {
        let mut rx = self.subscribe();
        let deadline = Instant::now() + limit;
        loop {
            {
                let m = self.mem();
                if m.sched.is_idle() && m.sched.unbuilt_leaves() == 0 {
                    return true;
                }
            }
            if tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), rx.changed()).await.is_err() {
                return false;
            }
        }
    }
}

impl Status {
    fn with_ratios(mut self, turn: Option<f64>, comp: Option<f64>) -> Self {
        self.turn_cache_read_ratio = turn;
        self.compaction_cache_read_ratio = comp;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::Kind;
    use crate::testkit::FakeAop;
    use serde_json::json;

    fn engine(fake: &FakeAop, dir: &Path) -> Arc<Engine> {
        let mut cfg = Config::default();
        cfg.aop_bin = fake.aop().bin;
        Engine::open(dir, cfg).unwrap()
    }

    fn long(n: usize) -> String {
        format!("message {n}: {}", "lorem ipsum dolor sit amet ".repeat(40))
    }

    #[tokio::test]
    async fn a_chat_is_fully_summarized_through_aop() {
        let fake = FakeAop::new(json!({"compact": "long:1"}));
        let d = tempfile::tempdir().unwrap();
        let e = engine(&fake, d.path());
        let pump = tokio::spawn(e.clone().pump());
        for n in 0..12 {
            e.mem().append(if n % 2 == 0 { Kind::User } else { Kind::Agent }, &long(n), None).unwrap();
            e.changed();
        }
        assert!(e.wait_idle(Duration::from_secs(60)).await, "{}", e.status().render());
        e.stop();
        pump.await.unwrap();
        let mut m = e.mem();
        // 12 leaves and 10 merges, all built once with the usage of their calls.
        let nodes = m.tree.all().unwrap();
        assert_eq!(nodes.len(), 22);
        assert!(nodes.iter().all(|n| n.usage.is_some_and(|u| u.output_tokens == 80) && n.model.as_deref() == Some("claude-fake-1") && n.size <= 512));
        assert_eq!(m.sched.unbuilt_leaves(), 0);
        assert!(m.tree.is_built(3, 0) && m.tree.is_built(2, 2));
        assert!(!m.chat_block().unwrap().contains("not summarized"));
        drop(m);
        let ledger = e.ledger();
        assert_eq!(ledger.len(), 44);
        assert!(ledger.iter().all(|l| l.kind == "compaction"));
        assert!(e.status().compaction_cache_read_ratio.unwrap() > 0.0);
    }

    #[tokio::test]
    async fn failed_nodes_are_retried_and_written_once() {
        let fake = FakeAop::new(json!({"compact": "ok", "fail_first": 2}));
        let d = tempfile::tempdir().unwrap();
        let e = engine(&fake, d.path());
        let pump = tokio::spawn(e.clone().pump());
        e.mem().append(Kind::User, &long(0), None).unwrap();
        e.changed();
        for _ in 0..100 {
            if e.status().last_error.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(e.status().last_error.unwrap().contains("compaction of message 0 failed"));
        assert!(e.mem().tree.all().unwrap().is_empty(), "a failed call writes no node");
        // The next message retries everything that is waiting.
        for n in 1..=2 {
            e.mem().append(Kind::Agent, &long(n), None).unwrap();
            e.changed();
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        assert!(e.wait_idle(Duration::from_secs(60)).await, "{}", e.status().render());
        e.stop();
        pump.await.unwrap();
        let nodes = e.mem().tree.all().unwrap();
        let leaf0 = nodes.iter().filter(|n| n.l == 0 && n.i == 0).count();
        assert_eq!(leaf0, 1);
        assert!(e.ledger().iter().any(|l| !l.ok));
    }

    #[tokio::test]
    async fn the_waiting_turn_rule_lets_a_turn_go_on_after_three_failures() {
        let fake = FakeAop::new(json!({"compact": "fail"}));
        let d = tempfile::tempdir().unwrap();
        let e = engine(&fake, d.path());
        let pump = tokio::spawn(e.clone().pump());
        e.mem().append(Kind::User, &long(0), None).unwrap();
        e.changed();
        for round in 1..=3 {
            for _ in 0..150 {
                if e.mem().sched.failures(Job::Leaf(0)) >= round {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert!(e.mem().sched.failures(Job::Leaf(0)) >= round, "round {round}");
            // The next message retries everything that is waiting out a failure.
            e.mem().append(Kind::Agent, "ok", None).unwrap();
            e.changed();
        }
        assert!(e.mem().turn_may_proceed(4));
        assert!(e.mem().tree.all().unwrap().iter().all(|n| !(n.l == 0 && n.i == 0)), "no node for the failing message");
        e.stop();
        pump.await.unwrap();
    }
}
