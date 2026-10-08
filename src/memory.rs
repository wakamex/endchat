//! Memory: the log, the tree, the two views, and the scheduler, kept consistent.
//!
//! Everything here is synchronous and model-free, so the simulator and the tests drive it
//! directly. The async layers (`compact`, `turn`) take jobs from it and hand finished nodes back.

use crate::error::{Result, msg};
use crate::log::{Kind, Log, Message};
use crate::sched::{Job, Sched};
use crate::tree::{NODE_BYTES, Node, Tree};
use crate::view::{Line, PLACEHOLDER, Saved, View};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

/// The 512-dash ruler that shows a compaction how long its line may be.
pub fn ruler() -> String {
    "-".repeat(NODE_BYTES)
}

/// Characters of a message `zoom(id, 1)` returns at once.
pub const ZOOM_PAGE: usize = 25_000;

#[derive(Serialize, Deserialize)]
struct SavedViews {
    len: u64,
    chat: Saved,
    compaction: Saved,
}

pub struct Memory {
    pub dir: PathBuf,
    pub log: Log,
    pub tree: Tree,
    pub chat: View,
    pub comp: View,
    pub sched: Sched,
}

/// One compaction ready to send.
pub struct Task {
    pub job: Job,
    /// The whole prompt: the compaction view, then the task text.
    pub prompt: String,
    /// Bytes of the source the reply must be strictly smaller than.
    pub source_bytes: usize,
}

pub struct Limits {
    pub view_high: usize,
    pub view_low: usize,
    pub compaction_high: usize,
    pub compaction_low: usize,
}

impl From<&crate::profile::Limits> for Limits {
    fn from(l: &crate::profile::Limits) -> Self {
        Self { view_high: l.view_high, view_low: l.view_low, compaction_high: l.compaction_high, compaction_low: l.compaction_low }
    }
}

/// A node's text as one view line body: newlines become spaces.
fn flat(text: &str) -> String {
    text.replace('\n', " ")
}

pub fn line_name(l: u32, i: u64) -> String {
    format!("{}+{}", i << l, 1u64 << l)
}

fn free_join(a: &str, b: &str) -> Option<String> {
    (a.len() + 1 + b.len() <= NODE_BYTES).then(|| format!("{a}\n{b}"))
}

impl Memory {
    pub fn open(dir: &Path, limits: &Limits, workers: usize) -> Result<Self> {
        let mut log = Log::open(dir)?;
        let mut tree = Tree::open(dir)?;
        let mut sched = Sched::new(workers);
        Self::settle_tree(&mut log, &mut tree, &mut sched)?;
        let mut chat = View::new(limits.view_high, limits.view_low);
        let mut comp = View::new(limits.compaction_high, limits.compaction_low);
        let len = log.len();
        let saved = fs::read_to_string(dir.join("view.json")).ok().and_then(|s| serde_json::from_str::<SavedViews>(&s).ok());
        let restored = saved.and_then(|s| {
            let c = View::restore(&s.chat, s.len, limits.view_high, limits.view_low, &tree)?;
            let k = View::restore(&s.compaction, s.len, limits.compaction_high, limits.compaction_low, &tree)?;
            (s.len <= len).then_some((s.len, c, k))
        });
        let start = match restored {
            Some((n, c, k)) => {
                (chat, comp) = (c, k);
                n
            }
            None => {
                if len > 0 {
                    eprintln!("endchat: view.json is missing or does not match the log; replaying it from the log (every cache entry misses once)");
                }
                0
            }
        };
        let mut mem = Self { dir: dir.to_owned(), log, tree, chat, comp, sched };
        for i in start..len {
            mem.push_view(i)?;
        }
        if start < len {
            mem.save_views()?;
        }
        Ok(mem)
    }

    /// Build every free node that is missing and queue every node that needs a model.
    fn settle_tree(log: &mut Log, tree: &mut Tree, sched: &mut Sched) -> Result<()> {
        let len = log.len();
        for i in 0..len {
            if tree.is_built(0, i) {
                continue;
            }
            let m = log.get(i)?;
            let src = m.source();
            if src.len() <= NODE_BYTES {
                tree.put(&Node::new(0, i, src))?;
            } else {
                sched.add_leaf(i, !m.more || i + 1 < len);
            }
        }
        let mut l = 1;
        while (len >> l) > 0 {
            for i in 0..(len >> l) {
                if tree.is_built(l, i) || !tree.is_built(l - 1, 2 * i) || !tree.is_built(l - 1, 2 * i + 1) {
                    continue;
                }
                let a = tree.get(l - 1, 2 * i)?.expect("built").text;
                let b = tree.get(l - 1, 2 * i + 1)?.expect("built").text;
                match free_join(&a, &b) {
                    Some(t) => tree.put(&Node::new(l, i, t))?,
                    None => sched.add_merge(l, i),
                }
            }
            l += 1;
        }
        Ok(())
    }

    fn save_views(&self) -> Result<()> {
        let saved = SavedViews { len: self.log.len(), chat: self.chat.saved(), compaction: self.comp.saved() };
        let tmp = self.dir.join("view.json.tmp");
        {
            let f = File::create(&tmp)?;
            serde_json::to_writer(&f, &saved)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, self.dir.join("view.json"))?;
        File::open(&self.dir)?.sync_all()?;
        Ok(())
    }

    /// Append a line to both views, merging them as their rules say.
    fn push_view(&mut self, i: u64) -> Result<()> {
        let chat_merged = !self.chat.push_message(i, &self.tree).is_empty();
        let comp_merged = !self.comp.push_message(i, &self.tree).is_empty();
        let mut shrunk = false;
        if chat_merged && !comp_merged {
            shrunk = !self.comp.shrink(i + 1, &self.tree).is_empty();
        }
        if chat_merged || comp_merged || shrunk {
            self.save_views()?;
        }
        Ok(())
    }

    /// Log a message, build its free leaf or queue its compaction, and extend the views.
    pub fn append(&mut self, kind: Kind, text: &str, run: Option<&str>) -> Result<Vec<Message>> {
        let msgs = self.log.append(kind, text, run)?;
        for m in &msgs {
            let src = m.source();
            if src.len() <= NODE_BYTES {
                self.finish(Node::new(0, m.i, src), Job::Leaf(m.i))?;
            } else {
                self.sched.add_leaf(m.i, true);
            }
        }
        for m in &msgs {
            self.push_view(m.i)?;
        }
        self.sched.kick();
        Ok(msgs)
    }

    /// Store a built node, update the views and the scheduler, and settle its parent.
    pub fn finish(&mut self, node: Node, job: Job) -> Result<()> {
        self.tree.put(&node)?;
        self.chat.node_built(node.l, node.i, node.size);
        self.comp.node_built(node.l, node.i, node.size);
        self.sched.built(job);
        self.settle_parent(node.l, node.i)
    }

    fn settle_parent(&mut self, l: u32, i: u64) -> Result<()> {
        let (mut l, mut i) = (l, i);
        loop {
            if !self.tree.is_built(l, i ^ 1) {
                return Ok(());
            }
            let (pl, pi) = (l + 1, i / 2);
            if self.tree.is_built(pl, pi) {
                return Ok(());
            }
            let a = self.tree.get(l, pi * 2)?.expect("built").text;
            let b = self.tree.get(l, pi * 2 + 1)?.expect("built").text;
            match free_join(&a, &b) {
                Some(t) => {
                    self.tree.put(&Node::new(pl, pi, t))?;
                    (l, i) = (pl, pi);
                }
                None => {
                    self.sched.add_merge(pl, pi);
                    return Ok(());
                }
            }
        }
    }

    /// Lines of the view as `id+n|text` rows; an unbuilt leaf shows the placeholder.
    pub fn render(&mut self, lines: &[Line]) -> Result<String> {
        let mut out = String::new();
        for line in lines {
            let text = match self.tree.get(line.l, line.i)? {
                Some(n) => flat(&n.text),
                None => PLACEHOLDER.to_owned(),
            };
            out += &format!("{}|{}\n", line_name(line.l, line.i), text);
        }
        Ok(out)
    }

    /// The chat view as a turn sees it.
    pub fn chat_block(&mut self) -> Result<String> {
        let lines = self.chat.lines().to_vec();
        Ok(format!("<chat>\n{}</chat>\n", self.render(&lines)?))
    }

    fn compaction_head(&mut self, end: u64) -> Result<String> {
        let lines = self.comp.prefix(end).to_vec();
        Ok(format!("<chat>\n{}</chat>\n", self.render(&lines)?))
    }

    /// Everything a compaction call needs: its view up to the node and its task text.
    pub fn task(&mut self, job: Job) -> Result<Task> {
        match job {
            Job::Leaf(i) => {
                let m = self.log.get(i)?;
                let source = m.source();
                let head = self.compaction_head(i)?;
                let ruler = ruler();
                let prompt = format!(
                    "{head}Compaction: compress message {i} into one line of at most {NODE_BYTES} bytes\n(about 70 words), the length of this ruler:\n{ruler}\n<input>\n{source}\n</input>"
                );
                Ok(Task { job, prompt, source_bytes: source.len() })
            }
            Job::Merge(l, i) => {
                let (a, b) = (self.tree.get(l - 1, 2 * i)?, self.tree.get(l - 1, 2 * i + 1)?);
                let (Some(a), Some(b)) = (a, b) else { return msg(format!("merge {} has an unbuilt half", line_name(l, i))) };
                let (first, end) = (i << l, (i + 1) << l);
                let head = self.compaction_head(end)?;
                let (na, nb) = (line_name(a.l, a.i), line_name(b.l, b.i));
                let ruler = ruler();
                let prompt = format!(
                    "{head}Compaction: merge lines {na} and {nb}, adjacent, into one line of at most\n{NODE_BYTES} bytes (about 70 words), the length of this ruler:\n{ruler}\n<chat> may hold their messages, {first} to {}, in more detail: take details\nof them from there too.\n<input>\n{na}|{}\n{nb}|{}\n</input>",
                    end - 1,
                    flat(&a.text),
                    flat(&b.text)
                );
                Ok(Task { job, prompt, source_bytes: a.size + 1 + b.size })
            }
        }
    }

    /// What `zoom(id, n)` returns: the two lines under a node, or message `id` whole.
    pub fn zoom(&mut self, id: u64, n: u64, offset: usize) -> Result<String> {
        let len = self.log.len();
        if n == 0 || !n.is_power_of_two() || id % n != 0 {
            return msg(format!("zoom({id}, {n}): n must be a power of 2 and id a multiple of n.{}", self.container_hint(id)));
        }
        if id + n > len {
            return msg(format!("zoom({id}, {n}): the chat has {len} messages, so there is no line {id}+{n}.{}", self.container_hint(id.min(len.saturating_sub(1)))));
        }
        if n == 1 {
            let m = self.log.get(id)?;
            let body = m.source();
            let total = body.chars().count();
            let page: String = body.chars().skip(offset).take(ZOOM_PAGE).collect();
            if offset + ZOOM_PAGE < total {
                return Ok(format!("{page}\n[continues: {} of {total} characters shown; zoom({id}, 1, {})]", offset + ZOOM_PAGE, offset + ZOOM_PAGE));
            }
            return Ok(page);
        }
        let l = n.trailing_zeros();
        let Some(_) = self.tree.get(l, id >> l)? else {
            return msg(format!("line {id}+{n} is not built yet. Zoom a smaller line that covers the messages you need."));
        };
        let lines = [Line { l: l - 1, i: 2 * (id >> l), bytes: 0 }, Line { l: l - 1, i: 2 * (id >> l) + 1, bytes: 0 }];
        self.render(&lines)
    }

    /// Names the view line that holds message `id`, so a bad zoom can be corrected.
    fn container_hint(&self, id: u64) -> String {
        let lines = self.chat.lines();
        let p = lines.partition_point(|l| l.end() <= id);
        match lines.get(p) {
            Some(l) => format!(" Message {id} is in the view line {}.", line_name(l.l, l.i)),
            None => String::new(),
        }
    }

    pub fn date(&mut self, id: u64) -> Result<String> {
        if id >= self.log.len() {
            return msg(format!("date({id}): the chat has {} messages", self.log.len()));
        }
        Ok(self.log.get(id)?.date)
    }

    /// A waiting turn may go on when every earlier message is summarized or its leaf has failed 3 times.
    pub fn turn_may_proceed(&self, m: u64) -> bool {
        self.sched.unbuilt_before(m) == 0 || self.sched.leaves_before_all_failed(m, 3)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn limits() -> Limits {
        Limits { view_high: 4_000, view_low: 2_000, compaction_high: 1_500, compaction_low: 800 }
    }

    pub fn long(n: usize) -> String {
        format!("{} ", "word".repeat(n))
    }

    /// Finish a job with a stub summary of `bytes` bytes.
    pub fn stub_finish(mem: &mut Memory, job: Job, bytes: usize) {
        let (l, i) = match job {
            Job::Leaf(i) => (0, i),
            Job::Merge(l, i) => (l, i),
        };
        mem.finish(Node::new(l, i, "s".repeat(bytes)), job).unwrap();
    }

    #[test]
    fn short_messages_are_free_leaves_and_short_pairs_join() {
        let d = tempfile::tempdir().unwrap();
        let mut mem = Memory::open(d.path(), &limits(), 8).unwrap();
        mem.append(Kind::User, "hi", None).unwrap();
        mem.append(Kind::Agent, "hello", None).unwrap();
        assert_eq!(mem.tree.get(0, 0).unwrap().unwrap().text, "user: hi");
        assert_eq!(mem.tree.get(1, 0).unwrap().unwrap().text, "user: hi\nagent: hello");
        assert!(mem.sched.is_idle());
        assert_eq!(mem.chat_block().unwrap(), "<chat>\n0+1|user: hi\n1+1|agent: hello\n</chat>\n");
    }

    #[test]
    fn long_message_queues_a_leaf_and_shows_a_placeholder() {
        let d = tempfile::tempdir().unwrap();
        let mut mem = Memory::open(d.path(), &limits(), 8).unwrap();
        mem.append(Kind::User, &long(200), None).unwrap();
        assert_eq!(mem.sched.unbuilt_leaves(), 1);
        assert!(mem.chat_block().unwrap().contains("0+1|(not summarized yet: zoom it)"));
        assert!(!mem.turn_may_proceed(1));
        assert!(mem.turn_may_proceed(0));
        let job = mem.sched.next(0).unwrap();
        stub_finish(&mut mem, job, 100);
        assert!(mem.turn_may_proceed(1));
        assert!(!mem.chat_block().unwrap().contains("not summarized"));
    }

    #[test]
    fn merge_becomes_ready_when_both_halves_are_built() {
        let d = tempfile::tempdir().unwrap();
        let mut mem = Memory::open(d.path(), &limits(), 8).unwrap();
        mem.append(Kind::User, &long(200), None).unwrap();
        mem.append(Kind::Agent, &long(200), None).unwrap();
        let (a, b) = (mem.sched.next(0).unwrap(), mem.sched.next(0).unwrap());
        stub_finish(&mut mem, a, 400);
        assert_eq!(mem.sched.ready_merges(), 0);
        stub_finish(&mut mem, b, 400);
        assert_eq!(mem.sched.ready_merges(), 1);
        let job = mem.sched.next(0).unwrap();
        assert_eq!(job, Job::Merge(1, 0));
        let task = mem.task(job).unwrap();
        assert!(task.prompt.contains("Compaction: merge lines 0+1 and 1+1, adjacent"));
        assert!(task.prompt.contains("messages, 0 to 1,"));
        assert_eq!(task.source_bytes, 801);
    }

    #[test]
    fn leaf_task_shows_only_the_lines_before_the_message() {
        let d = tempfile::tempdir().unwrap();
        let mut mem = Memory::open(d.path(), &limits(), 8).unwrap();
        mem.append(Kind::User, "first", None).unwrap();
        mem.append(Kind::Agent, &long(200), None).unwrap();
        mem.append(Kind::User, "third", None).unwrap();
        let Task { prompt, .. } = mem.task(Job::Leaf(1)).unwrap();
        assert!(prompt.starts_with("<chat>\n0+1|user: first\n</chat>\nCompaction: compress message 1 into one line of at most 512 bytes\n(about 70 words), the length of this ruler:\n-"));
        assert!(prompt.contains(&format!("{}\n<input>\nagent: wordword", ruler())));
        assert!(prompt.ends_with("\n</input>"));
        assert_eq!(ruler().len(), 512);
    }

    #[test]
    fn placeholder_keeps_the_short_lines_after_an_unbuilt_one() {
        let d = tempfile::tempdir().unwrap();
        let mut mem = Memory::open(d.path(), &limits(), 8).unwrap();
        mem.append(Kind::Agent, &long(200), None).unwrap(); // 0: unbuilt
        mem.append(Kind::Tool, "ls", None).unwrap(); // 1: short, built
        mem.append(Kind::User, &long(200), None).unwrap(); // 2
        let head = mem.task(Job::Leaf(2)).unwrap().prompt;
        assert!(head.contains("0+1|(not summarized yet: zoom it)\n1+1|tool: ls\n"), "{head}");
    }

    #[test]
    fn zoom_opens_lines_and_names_the_container_on_error() {
        let d = tempfile::tempdir().unwrap();
        let mut mem = Memory::open(d.path(), &limits(), 8).unwrap();
        for t in ["a", "b", "c", "d"] {
            mem.append(Kind::User, t, None).unwrap();
        }
        assert_eq!(mem.zoom(0, 2, 0).unwrap(), "0+1|user: a\n1+1|user: b\n");
        assert_eq!(mem.zoom(3, 1, 0).unwrap(), "user: d");
        let e = mem.zoom(1, 2, 0).unwrap_err().to_string();
        assert!(e.contains("multiple of n") && e.contains("is in the view line 1+1"), "{e}");
        assert!(mem.zoom(0, 8, 0).unwrap_err().to_string().contains("no line 0+8"));
        assert!(mem.zoom(0, 3, 0).is_err());
        assert!(mem.date(9).is_err());
        assert!(mem.date(0).unwrap().ends_with('Z'));
    }

    #[test]
    fn zoom_pages_a_long_message() {
        let d = tempfile::tempdir().unwrap();
        let mut mem = Memory::open(d.path(), &limits(), 8).unwrap();
        mem.append(Kind::User, &"x".repeat(28_000), None).unwrap();
        let first = mem.zoom(0, 1, 0).unwrap();
        assert!(first.ends_with("zoom(0, 1, 25000)]"), "{}", &first[first.len() - 80..]);
        let rest = mem.zoom(0, 1, 25_000).unwrap();
        assert_eq!(rest.len(), 28_000 + "user: ".len() - 25_000);
    }

    #[test]
    fn views_survive_a_restart_without_rebuilding() {
        let d = tempfile::tempdir().unwrap();
        let before;
        {
            let mut mem = Memory::open(d.path(), &limits(), 8).unwrap();
            for i in 0..120 {
                mem.append(Kind::User, &format!("message number {i} with some padding to make it longer than usual"), None).unwrap();
            }
            before = (mem.chat_block().unwrap(), mem.comp.saved());
            assert!(mem.chat.lines().len() < 120, "the chat view merged");
        }
        let mut mem = Memory::open(d.path(), &limits(), 8).unwrap();
        assert_eq!((mem.chat_block().unwrap(), mem.comp.saved()), before);
    }

    #[test]
    fn broken_view_json_is_replayed_from_the_log() {
        let d = tempfile::tempdir().unwrap();
        {
            let mut mem = Memory::open(d.path(), &limits(), 8).unwrap();
            for i in 0..60 {
                mem.append(Kind::User, &format!("message number {i} with some padding to make it longer than usual"), None).unwrap();
            }
        }
        fs::write(d.path().join("view.json"), "{ not json").unwrap();
        let mem = Memory::open(d.path(), &limits(), 8).unwrap();
        let mut at = 0;
        for l in mem.chat.lines() {
            assert_eq!(l.id(), at);
            at = l.end();
        }
        assert_eq!(at, 60);
    }

    #[test]
    fn restart_requeues_missing_nodes() {
        let d = tempfile::tempdir().unwrap();
        {
            let mut mem = Memory::open(d.path(), &limits(), 8).unwrap();
            mem.append(Kind::User, &long(200), None).unwrap();
            mem.append(Kind::User, &long(200), None).unwrap();
            let job = mem.sched.next(0).unwrap();
            stub_finish(&mut mem, job, 300);
        }
        let mut mem = Memory::open(d.path(), &limits(), 8).unwrap();
        assert_eq!(mem.sched.unbuilt_leaves(), 1);
        assert_eq!(mem.sched.next(0), Some(Job::Leaf(1)));
    }
}
