//! The message log: append-only, fsynced, one record per line, dense ids.

use crate::error::{Error, Result, msg};
use crate::store::{DayStore, Idx, Rec};
use crate::util::{day_of, now_iso};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const PAGE_CHARS: usize = 30_000;
pub const CLIP_CHARS: usize = 30_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    User,
    Agent,
    Tool,
    Echo,
    Work,
    Note,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::User => "user",
            Kind::Agent => "agent",
            Kind::Tool => "tool",
            Kind::Echo => "echo",
            Kind::Work => "work",
            Kind::Note => "note",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub i: u64,
    pub kind: Kind,
    pub text: String,
    pub size: usize,
    pub date: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub more: bool,
}

impl Message {
    /// `kind: text`, the form a message takes as a compaction source.
    pub fn source(&self) -> String {
        format!("{}: {}", self.kind.as_str(), self.text)
    }
}

/// Head and tail of a tool output, 30,000 characters in all, with a notice of what was cut.
pub fn clip(text: &str) -> String {
    let total = text.chars().count();
    if total <= CLIP_CHARS {
        return text.to_owned();
    }
    let half = CLIP_CHARS / 2;
    let head: String = text.chars().take(half).collect();
    let tail: String = text.chars().skip(total - half).collect();
    format!("{head}\n[... {} characters cut ...]\n{tail}", total - 2 * half)
}

/// Pages of at most `PAGE_CHARS` characters, split at the last newline inside the limit when there
/// is one. Joined, the pages are the original text.
pub fn paginate(text: &str) -> Vec<&str> {
    let mut pages = Vec::new();
    let mut rest = text;
    while rest.chars().count() > PAGE_CHARS {
        let limit = rest.char_indices().nth(PAGE_CHARS).map_or(rest.len(), |(b, _)| b);
        let cut = rest[..limit].rfind('\n').map_or(limit, |p| p + 1);
        let (page, tail) = rest.split_at(cut);
        pages.push(page);
        rest = tail;
    }
    pages.push(rest);
    pages
}

pub struct Log {
    store: DayStore,
    idx: Idx,
    len: u64,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct CheckReport {
    pub messages: u64,
    pub torn_lines: u64,
    pub index_mismatches: u64,
    pub index_slots: u64,
    pub unterminated_page_run: bool,
}

impl CheckReport {
    pub fn ok(&self) -> bool {
        self.torn_lines == 0 && self.index_mismatches == 0 && self.index_slots == self.messages
    }
}

impl Log {
    pub fn open(profile: &Path) -> Result<Self> {
        let store = DayStore::open(&profile.join("main"))?;
        let idx = Idx::open(&profile.join("main.idx"))?;
        let mut log = Self { store, idx, len: 0 };
        log.recover()?;
        Ok(log)
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn parse(line: &str) -> Option<Message> {
        serde_json::from_str(line).ok()
    }

    /// Bring the index in line with the log: extend it past a crash between append and index
    /// write, or rebuild it when its tail disagrees with the log. The log is never edited.
    fn recover(&mut self) -> Result<()> {
        let slots = self.idx.slots()?;
        if slots > 0 && !self.idx.is_ragged()? {
            if let Some(rec) = self.idx.get(slots - 1)? {
                if self.read_at(rec).is_ok_and(|m| m.i == slots - 1) {
                    self.len = slots;
                    return self.extend_from(rec);
                }
            }
        }
        self.rebuild()
    }

    fn extend_from(&mut self, last: Rec) -> Result<()> {
        let days = self.store.days()?;
        for d in days.into_iter().filter(|&d| d >= last.day) {
            let from = if d == last.day { last.end() } else { 0 };
            if !self.index_scan(d, from)? {
                return self.rebuild();
            }
        }
        Ok(())
    }

    /// Index the valid lines of a day file from `from`. False when an id is not the next one.
    fn index_scan(&mut self, day: u32, from: u64) -> Result<bool> {
        for (rec, line) in self.store.scan(day, from)? {
            let Some(m) = Self::parse(&line) else { continue };
            if m.i != self.len {
                return Ok(false);
            }
            self.idx.set(self.len, rec)?;
            self.len += 1;
        }
        Ok(true)
    }

    pub fn rebuild(&mut self) -> Result<()> {
        self.idx.clear()?;
        self.len = 0;
        for d in self.store.days()? {
            if !self.index_scan(d, 0)? {
                return msg(format!("log ids are not dense at message {}", self.len));
            }
        }
        self.idx.sync()
    }

    fn read_at(&self, rec: Rec) -> Result<Message> {
        let line = self.store.read(rec)?;
        serde_json::from_str(&line).map_err(Error::from)
    }

    pub fn get(&mut self, i: u64) -> Result<Message> {
        if i >= self.len {
            return msg(format!("no message {i}: the log has {}", self.len));
        }
        let hit = self.idx.get(i)?.and_then(|r| self.read_at(r).ok()).filter(|m| m.i == i);
        match hit {
            Some(m) => Ok(m),
            None => {
                self.rebuild()?;
                let rec = self.idx.get(i)?.ok_or(Error::IndexMismatch(i))?;
                self.read_at(rec)
            }
        }
    }

    /// Log text, clipped when it is tool output and paged when it is long. Returns the records.
    pub fn append(&mut self, kind: Kind, text: &str, run: Option<&str>) -> Result<Vec<Message>> {
        self.append_dated(kind, text, run, &now_iso())
    }

    pub fn append_dated(&mut self, kind: Kind, text: &str, run: Option<&str>, date: &str) -> Result<Vec<Message>> {
        let clipped;
        let text = if kind == Kind::Echo {
            clipped = clip(text);
            &clipped
        } else {
            text
        };
        let pages = if kind == Kind::Echo { vec![text] } else { paginate(text) };
        let last = pages.len() - 1;
        let mut out = Vec::with_capacity(pages.len());
        for (n, page) in pages.into_iter().enumerate() {
            let m = Message {
                i: self.len,
                kind,
                text: page.to_owned(),
                size: kind.as_str().len() + 2 + page.len(),
                date: date.to_owned(),
                run: run.map(str::to_owned),
                more: n < last,
            };
            let rec = self.store.append(day_of(date), &serde_json::to_string(&m)?)?;
            self.idx.set(m.i, rec)?;
            self.len += 1;
            out.push(m);
        }
        Ok(out)
    }

    /// Whether message `i` is complete: not a page followed by more pages still to come.
    pub fn is_complete(&mut self, i: u64) -> Result<bool> {
        Ok(!self.get(i)?.more || i + 1 < self.len)
    }

    pub fn check(&mut self) -> Result<CheckReport> {
        let mut r = CheckReport { index_slots: self.idx.slots()?, ..Default::default() };
        let mut next = 0;
        for d in self.store.days()? {
            for (rec, line) in self.store.scan(d, 0)? {
                match Self::parse(&line) {
                    None => r.torn_lines += 1,
                    Some(m) => {
                        if m.i != next || self.idx.get(m.i)? != Some(rec) {
                            r.index_mismatches += 1;
                        }
                        r.unterminated_page_run = m.more;
                        next = m.i + 1;
                    }
                }
            }
        }
        r.messages = next;
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, OpenOptions};
    use std::io::Write;

    fn open(dir: &Path) -> Log {
        Log::open(dir).unwrap()
    }

    #[test]
    fn append_and_read() {
        let d = tempfile::tempdir().unwrap();
        let mut log = open(d.path());
        log.append(Kind::User, "hello", None).unwrap();
        log.append(Kind::Agent, "héllo", Some("run1")).unwrap();
        assert_eq!(log.len(), 2);
        let m = log.get(1).unwrap();
        assert_eq!((m.i, m.kind, m.size, m.run.as_deref()), (1, Kind::Agent, "agent: héllo".len(), Some("run1")));
        drop(log);
        assert_eq!(open(d.path()).len(), 2);
    }

    #[test]
    fn days_split_files() {
        let d = tempfile::tempdir().unwrap();
        let mut log = open(d.path());
        log.append_dated(Kind::User, "a", None, "2026-10-07T23:59:59Z").unwrap();
        log.append_dated(Kind::User, "b", None, "2026-10-08T00:00:01Z").unwrap();
        assert!(d.path().join("main/2026-10-07.jsonl").exists());
        assert!(d.path().join("main/2026-10-08.jsonl").exists());
        drop(log);
        let mut log = open(d.path());
        assert_eq!(log.get(1).unwrap().text, "b");
    }

    #[test]
    fn long_message_pages_rejoin_exactly() {
        let d = tempfile::tempdir().unwrap();
        let mut log = open(d.path());
        let text: String = (0..20_000).map(|n| format!("line {n} é\n")).collect::<String>().chars().take(100_000).collect();
        let pages = log.append(Kind::User, &text, None).unwrap();
        assert!(pages.len() >= 4);
        assert!(pages.iter().all(|p| p.text.chars().count() <= PAGE_CHARS));
        assert!(pages[..pages.len() - 1].iter().all(|p| p.more && p.text.ends_with('\n')));
        assert!(!pages.last().unwrap().more);
        let joined: String = pages.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(joined, text);
        assert!(log.is_complete(pages.len() as u64 - 1).unwrap());
    }

    #[test]
    fn pages_without_newlines_split_at_characters() {
        let text = "é".repeat(70_000);
        let pages = paginate(&text);
        assert_eq!(pages.iter().map(|p| p.chars().count()).collect::<Vec<_>>(), [30_000, 30_000, 10_000]);
        assert_eq!(pages.concat(), text);
    }

    #[test]
    fn echo_is_clipped_not_paged() {
        let d = tempfile::tempdir().unwrap();
        let mut log = open(d.path());
        let out = "x".repeat(100_000);
        let recs = log.append(Kind::Echo, &out, None).unwrap();
        assert_eq!(recs.len(), 1);
        assert!(recs[0].text.contains("[... 70000 characters cut ...]"));
        assert!(recs[0].text.chars().count() < 30_100);
    }

    #[test]
    fn torn_last_line_is_ended_and_skipped() {
        let d = tempfile::tempdir().unwrap();
        let mut log = open(d.path());
        log.append_dated(Kind::User, "a", None, "2026-10-08T00:00:00Z").unwrap();
        drop(log);
        let path = d.path().join("main/2026-10-08.jsonl");
        OpenOptions::new().append(true).open(&path).unwrap().write_all(b"{\"i\":1,\"ki").unwrap();
        let mut log = open(d.path());
        assert_eq!(log.len(), 1);
        log.append_dated(Kind::User, "b", None, "2026-10-08T00:00:02Z").unwrap();
        assert_eq!(log.get(1).unwrap().text, "b");
        let report = log.check().unwrap();
        assert_eq!((report.messages, report.torn_lines, report.index_mismatches), (2, 1, 0));
        assert!(!report.ok());
    }

    #[test]
    fn second_writer_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let mut log = open(d.path());
        log.append_dated(Kind::User, "a", None, "2026-10-08T00:00:00Z").unwrap();
        let path = d.path().join("main/2026-10-08.jsonl");
        OpenOptions::new().append(true).open(&path).unwrap().write_all(b"{}\n").unwrap();
        let err = log.append_dated(Kind::User, "b", None, "2026-10-08T00:00:01Z").unwrap_err();
        assert!(matches!(err, Error::ForeignWrite { .. }), "{err}");
    }

    #[test]
    fn stale_index_is_rebuilt_from_the_log() {
        let d = tempfile::tempdir().unwrap();
        let mut log = open(d.path());
        for t in ["a", "b", "c"] {
            log.append(Kind::User, t, None).unwrap();
        }
        drop(log);
        // Point record 2 at record 0's line.
        let idx = Idx::open(&d.path().join("main.idx")).unwrap();
        let r0 = idx.get(0).unwrap().unwrap();
        idx.set(2, r0).unwrap();
        drop(idx);
        let mut log = open(d.path());
        assert_eq!(log.len(), 3);
        assert_eq!(log.get(2).unwrap().text, "c");
        assert!(log.check().unwrap().ok());
    }

    #[test]
    fn crash_between_log_append_and_index_write_loses_nothing() {
        let d = tempfile::tempdir().unwrap();
        let mut log = open(d.path());
        log.append(Kind::User, "a", None).unwrap();
        log.append(Kind::User, "b", None).unwrap();
        drop(log);
        // The log line for message 2 was written but its index record was not.
        let day = day_of(&now_iso());
        let mut store = DayStore::open(&d.path().join("main")).unwrap();
        let m = Message { i: 2, kind: Kind::User, text: "c".into(), size: 7, date: now_iso(), run: None, more: false };
        store.append(day, &serde_json::to_string(&m).unwrap()).unwrap();
        drop(store);
        let mut log = open(d.path());
        assert_eq!(log.len(), 3);
        assert_eq!(log.get(2).unwrap().text, "c");
        assert!(log.check().unwrap().ok());
    }

    #[test]
    fn missing_index_is_rebuilt() {
        let d = tempfile::tempdir().unwrap();
        let mut log = open(d.path());
        log.append(Kind::User, "a", None).unwrap();
        log.append(Kind::User, "b", None).unwrap();
        drop(log);
        fs::remove_file(d.path().join("main.idx")).unwrap();
        let mut log = open(d.path());
        assert_eq!((log.len(), log.get(1).unwrap().text.as_str()), (2, "b"));
    }
}
