//! `endchat search`: a regular expression over a profile's log, which holds each message once.
//!
//! The day files are read directly, so it works while a server owns the profile.

use crate::error::{Error, Result};
use crate::log::Message;
use regex::Regex;
use std::path::Path;

pub struct Query {
    pub pattern: Regex,
    /// Only these kinds; empty means all.
    pub kinds: Vec<String>,
    /// Only the newest this many matches.
    pub newest: Option<usize>,
    /// Messages shown before and after each match.
    pub context: u64,
    /// Characters shown of each message, around its first match.
    pub chars: usize,
}

/// Every message in the log, in order. Page runs stay separate pages.
fn messages(profile: &Path) -> Result<Vec<Message>> {
    let mut days: Vec<_> = std::fs::read_dir(profile.join("main"))?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "jsonl")).collect();
    days.sort();
    let mut out = Vec::new();
    for day in days {
        out.extend(std::fs::read_to_string(day)?.lines().filter_map(|l| serde_json::from_str::<Message>(l).ok()));
    }
    out.sort_by_key(|m| m.i);
    Ok(out)
}

/// `text` cut to about `chars` characters around byte `at`, marking what was left out.
fn window(text: &str, at: usize, chars: usize) -> String {
    let total = text.chars().count();
    if total <= chars {
        return text.to_owned();
    }
    let center = text[..at].chars().count();
    let start = center.saturating_sub(chars / 3).min(total - chars);
    let body: String = text.chars().skip(start).take(chars).collect();
    let (head, tail) = (start > 0, start + chars < total);
    format!("{}{body}{}", if head { "[...] " } else { "" }, if tail { format!(" [... {total} characters in all]") } else { String::new() })
}

pub fn run(profile: &Path, q: &Query) -> Result<String> {
    let msgs = messages(profile)?;
    if msgs.is_empty() && !profile.join("main").exists() {
        return Err(Error::Msg(format!("{} has no log", profile.display())));
    }
    let mut hits: Vec<usize> = (0..msgs.len()).filter(|&k| (q.kinds.is_empty() || q.kinds.iter().any(|x| x == msgs[k].kind.as_str())) && q.pattern.is_match(&msgs[k].text)).collect();
    if let Some(n) = q.newest {
        hits = hits.split_off(hits.len().saturating_sub(n));
    }
    let mut out = String::new();
    let mut shown_to = 0usize;
    for &k in &hits {
        let from = k.saturating_sub(q.context as usize).max(shown_to);
        let to = (k + q.context as usize + 1).min(msgs.len());
        if from > shown_to && !out.is_empty() {
            out += "--\n";
        }
        for m in &msgs[from..to] {
            let at = q.pattern.find(&m.text).map_or(0, |f| f.start());
            out += &format!("{}|{}|{}: {}\n", m.i, m.kind.as_str(), &m.date, window(&m.text, at, q.chars));
        }
        shown_to = to;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{Kind, Log};

    fn profile() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let mut log = Log::open(d.path()).unwrap();
        for (kind, text) in [(Kind::User, "what was the vault code?"), (Kind::Agent, "It was 4417."), (Kind::Tool, "Bash {\"command\":\"grep vault notes\"}"), (Kind::Echo, "vault: 4417"), (Kind::User, "thanks")] {
            log.append(kind, text, None).unwrap();
        }
        d
    }

    fn query(p: &str) -> Query {
        Query { pattern: Regex::new(p).unwrap(), kinds: vec![], newest: None, context: 0, chars: 1000 }
    }

    #[test]
    fn matches_kinds_newest_and_context() {
        let d = profile();
        let all = run(d.path(), &query("vault")).unwrap();
        assert_eq!(all.lines().filter(|l| *l != "--").count(), 3);
        assert!(all.contains("\n--\n"), "a gap between matches is marked");
        assert!(all.lines().next().unwrap().starts_with("0|user|20") && all.contains("|echo|"));
        let echo = run(d.path(), &Query { kinds: vec!["echo".into()], ..query("4417") }).unwrap();
        assert!(echo.starts_with("3|echo|") && echo.lines().count() == 1);
        let newest = run(d.path(), &Query { newest: Some(1), context: 1, ..query("4417") }).unwrap();
        assert_eq!(newest.lines().map(|l| l.split('|').next().unwrap()).collect::<Vec<_>>(), ["2", "3", "4"]);
    }

    #[test]
    fn a_long_message_shows_a_window_around_its_match() {
        let text = format!("{}needle{}", "a".repeat(5000), "b".repeat(5000));
        let w = window(&text, 5000, 300);
        assert!(w.starts_with("[...] ") && w.contains("needle") && w.ends_with("[... 10006 characters in all]"));
    }
}
