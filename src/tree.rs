//! The summary tree: node `(l, i)` summarizes messages `i*2^l` through `(i+1)*2^l - 1`.

use crate::error::{Error, Result, msg};
use crate::store::{DayStore, Idx, Rec};
use crate::util::{day_of, now_iso};
use crate::view::Sizes;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

pub const NODE_BYTES: usize = 512;

/// AOP's normalized token usage for one call (`aop-token-usage-v3`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub reasoning_output_tokens: u64,
}

impl Usage {
    pub fn add(&mut self, o: &Usage) {
        self.input_tokens += o.input_tokens;
        self.cached_input_tokens += o.cached_input_tokens;
        self.output_tokens += o.output_tokens;
        self.reasoning_output_tokens += o.reasoning_output_tokens;
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub l: u32,
    pub i: u64,
    pub text: String,
    pub size: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub usage: Option<Usage>,
}

impl Node {
    pub fn new(l: u32, i: u64, text: String) -> Self {
        Self { l, i, size: text.len(), text, run: None, model: None, usage: None }
    }

    /// First message covered, as in the node's name `id+n`.
    pub fn id(&self) -> u64 {
        self.i << self.l
    }
}

/// Unique slot of a node; slots number nodes in the order of their last message.
pub fn slot(l: u32, i: u64) -> u64 {
    ((2 * i + 1) << l) - 1
}

pub struct Tree {
    store: DayStore,
    idx: Idx,
    built: Vec<bool>,
    sizes: RefCell<HashMap<u64, usize>>,
}

impl Tree {
    pub fn open(profile: &Path) -> Result<Self> {
        let store = DayStore::open(&profile.join("tree"))?;
        let idx = Idx::open(&profile.join("tree.idx"))?;
        let mut t = Self { store, idx, built: Vec::new(), sizes: RefCell::new(HashMap::new()) };
        t.load()?;
        Ok(t)
    }

    fn mark(&mut self, slot: u64) {
        let s = slot as usize;
        if self.built.len() <= s {
            self.built.resize(s + 1, false);
        }
        self.built[s] = true;
    }

    /// Read the index, then index any log lines it does not know about (a crash between the
    /// log append and the index write, or a lost index).
    fn load(&mut self) -> Result<()> {
        let recs = if self.idx.is_ragged()? { Vec::new() } else { self.idx.read_all()? };
        let mut ends: HashMap<u32, u64> = HashMap::new();
        for (s, r) in recs.iter().enumerate() {
            if let Some(r) = r {
                self.mark(s as u64);
                let e = ends.entry(r.day).or_insert(0);
                *e = (*e).max(r.end());
            }
        }
        for d in self.store.days()? {
            let from = ends.get(&d).copied().unwrap_or(0);
            if self.store.file_len(d) > from {
                self.index_scan(d, from)?;
            }
        }
        Ok(())
    }

    fn index_scan(&mut self, day: u32, from: u64) -> Result<()> {
        for (rec, line) in self.store.scan(day, from)? {
            if let Ok(n) = serde_json::from_str::<Node>(&line) {
                let s = slot(n.l, n.i);
                if self.idx.get(s)?.is_none() {
                    self.idx.set(s, rec)?;
                    self.mark(s);
                }
            }
        }
        Ok(())
    }

    pub fn rebuild(&mut self) -> Result<()> {
        self.idx.clear()?;
        self.built.clear();
        for d in self.store.days()? {
            self.index_scan(d, 0)?;
        }
        self.idx.sync()
    }

    pub fn is_built(&self, l: u32, i: u64) -> bool {
        self.built.get(slot(l, i) as usize).copied().unwrap_or(false)
    }

    fn read(&self, rec: Rec, l: u32, i: u64) -> Option<Node> {
        let n: Node = serde_json::from_str(&self.store.read(rec).ok()?).ok()?;
        (n.l == l && n.i == i).then_some(n)
    }

    pub fn get(&mut self, l: u32, i: u64) -> Result<Option<Node>> {
        if !self.is_built(l, i) {
            return Ok(None);
        }
        let s = slot(l, i);
        if let Some(n) = self.idx.get(s)?.and_then(|r| self.read(r, l, i)) {
            return Ok(Some(n));
        }
        self.rebuild()?;
        match self.idx.get(s)? {
            Some(r) => Ok(self.read(r, l, i)),
            None => Err(Error::IndexMismatch(s)),
        }
    }

    /// Write a node, once. A second node for the same slot is an error.
    pub fn put(&mut self, node: &Node) -> Result<()> {
        if node.text.is_empty() {
            return msg(format!("refusing to write an empty node {}+{}", node.id(), 1u64 << node.l));
        }
        if self.is_built(node.l, node.i) {
            return msg(format!("node {}+{} is already built", node.id(), 1u64 << node.l));
        }
        let rec = self.store.append(day_of(&now_iso()), &serde_json::to_string(node)?)?;
        let s = slot(node.l, node.i);
        self.idx.set(s, rec)?;
        self.mark(s);
        self.sizes.borrow_mut().insert(s, node.size);
        Ok(())
    }

    /// Every built node, for checks and cost totals.
    pub fn all(&self) -> Result<Vec<Node>> {
        let mut out = Vec::new();
        for d in self.store.days()? {
            for (_, line) in self.store.scan(d, 0)? {
                if let Ok(n) = serde_json::from_str(&line) {
                    out.push(n);
                }
            }
        }
        Ok(out)
    }

    /// Size of a built node's text, read without repairing the index.
    fn peek_size(&self, l: u32, i: u64) -> Option<usize> {
        if !self.is_built(l, i) {
            return None;
        }
        let s = slot(l, i);
        if let Some(&n) = self.sizes.borrow().get(&s) {
            return Some(n);
        }
        let n = self.read(self.idx.get(s).ok()??, l, i)?.size;
        self.sizes.borrow_mut().insert(s, n);
        Some(n)
    }

    pub fn built_count(&self) -> usize {
        self.built.iter().filter(|b| **b).count()
    }
}

impl Sizes for Tree {
    fn text_size(&self, l: u32, i: u64) -> Option<usize> {
        self.peek_size(l, i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_unique_and_ordered_by_last_message() {
        // Message i's leaf is slot 2i; each parent follows its second child.
        assert_eq!((slot(0, 0), slot(0, 1), slot(1, 0), slot(0, 2), slot(0, 3), slot(1, 1), slot(2, 0)), (0, 2, 1, 4, 6, 5, 3));
        let mut seen = std::collections::HashSet::new();
        for l in 0..6 {
            for i in 0..(64u64 >> l) {
                assert!(seen.insert(slot(l, i)));
            }
        }
    }

    #[test]
    fn nodes_are_written_once_and_survive_reopen() {
        let d = tempfile::tempdir().unwrap();
        let mut t = Tree::open(d.path()).unwrap();
        let mut n = Node::new(1, 3, "merged".into());
        n.usage = Some(Usage { input_tokens: 9, cached_input_tokens: 4, output_tokens: 2, reasoning_output_tokens: 0 });
        t.put(&n).unwrap();
        assert!(t.put(&n).is_err());
        assert!(t.put(&Node::new(0, 0, String::new())).is_err());
        drop(t);
        let mut t = Tree::open(d.path()).unwrap();
        assert!(t.is_built(1, 3) && !t.is_built(1, 2));
        assert_eq!(t.get(1, 3).unwrap().unwrap(), n);
        assert_eq!(t.get(1, 2).unwrap(), None);
    }

    #[test]
    fn lost_or_stale_index_is_recovered_from_the_tree_log() {
        let d = tempfile::tempdir().unwrap();
        let mut t = Tree::open(d.path()).unwrap();
        for i in 0..4 {
            t.put(&Node::new(0, i, format!("n{i}"))).unwrap();
        }
        drop(t);
        std::fs::remove_file(d.path().join("tree.idx")).unwrap();
        let mut t = Tree::open(d.path()).unwrap();
        assert_eq!(t.built_count(), 4);
        assert_eq!(t.get(0, 2).unwrap().unwrap().text, "n2");
        // Point slot 0 at node 1's line: reading it notices and rebuilds.
        let r1 = t.idx.get(slot(0, 1)).unwrap().unwrap();
        t.idx.set(slot(0, 0), r1).unwrap();
        assert_eq!(t.get(0, 0).unwrap().unwrap().text, "n0");
    }
}
