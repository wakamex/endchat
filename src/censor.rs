//! Censored messages: a message the model's safeguards refuse would be resent, and refused, on
//! every turn until a cut removed it. Censoring replaces its text in the log with a one-line notice,
//! so every reader of the log (turns, cut summaries, search) sees the notice, and keeps the original
//! in `censored.json`, written before the log is touched, so it can be restored.

use crate::error::Result;
use crate::log::Message;
use crate::memory::Memory;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const FILE: &str = "censored.json";

/// The text a censored message is left with.
pub fn notice(i: u64) -> String {
    format!("[message {i} censored: the model's safeguards refused a turn on it, so its text is kept outside the chat]")
}

/// The original of every censored message, by id.
pub fn censored(dir: &Path) -> BTreeMap<u64, Message> {
    fs::read_to_string(dir.join(FILE)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save(dir: &Path, saved: &BTreeMap<u64, Message>) -> Result<()> {
    let tmp = dir.join(format!("{FILE}.tmp"));
    let f = fs::File::create(&tmp)?;
    serde_json::to_writer_pretty(&f, saved)?;
    f.sync_all()?;
    fs::rename(&tmp, dir.join(FILE))?;
    Ok(())
}

/// `ids` and the later pages of any that start a page run.
fn with_pages(ids: &[u64], mut more: impl FnMut(u64) -> Result<bool>) -> Result<Vec<u64>> {
    let mut out = Vec::new();
    for &i in ids {
        let mut k = i;
        out.push(k);
        while more(k)? {
            k += 1;
            out.push(k);
        }
    }
    Ok(out)
}

/// Censor `ids`; ones already censored are left as they are.
pub fn censor(mem: &mut Memory, ids: &[u64]) -> Result<()> {
    let mut saved = censored(&mem.dir);
    let ids = with_pages(ids, |k| match saved.get(&k) {
        Some(m) => Ok(m.more),
        None => Ok(mem.log.get(k)?.more),
    })?;
    let mut texts = BTreeMap::new();
    for &i in &ids {
        if saved.contains_key(&i) {
            continue;
        }
        let m = mem.log.get(i)?;
        // A page run keeps one notice, on its first page.
        let first = i == 0
            || !match saved.get(&(i - 1)) {
                Some(p) => p.more,
                None => mem.log.get(i - 1)?.more,
            };
        texts.insert(i, if first { notice(i) } else { String::new() });
        saved.insert(i, m);
    }
    if texts.is_empty() {
        return Ok(());
    }
    save(&mem.dir, &saved)?;
    mem.log.rewrite(&texts)
}

/// Put the originals of `ids` back in the log.
pub fn release(mem: &mut Memory, ids: &[u64]) -> Result<()> {
    let mut saved = censored(&mem.dir);
    let ids = with_pages(ids, |k| Ok(saved.get(&k).is_some_and(|m| m.more)))?;
    let texts: BTreeMap<u64, String> = ids.iter().filter_map(|i| saved.get(i).map(|m| (*i, m.text.clone()))).collect();
    if texts.is_empty() {
        return Ok(());
    }
    mem.log.rewrite(&texts)?;
    saved.retain(|i, _| !texts.contains_key(i));
    save(&mem.dir, &saved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::Kind;

    #[test]
    fn censoring_rewrites_the_log_keeps_the_original_and_releasing_restores_it() {
        let d = tempfile::tempdir().unwrap();
        let mut m = Memory::open(d.path(), &crate::memory::tests::limits(), 2).unwrap();
        m.append(Kind::User, "hello", None).unwrap();
        m.append(Kind::User, "the refused words", None).unwrap();
        m.append(Kind::Agent, &"x".repeat(crate::log::PAGE_CHARS + 5), None).unwrap();
        m.append(Kind::User, "after", None).unwrap();
        censor(&mut m, &[1, 2]).unwrap();
        assert_eq!(m.log.get(1).unwrap().text, notice(1));
        assert_eq!((m.log.get(2).unwrap().text, m.log.get(3).unwrap().text), (notice(2), String::new()), "a page run keeps one notice");
        assert_eq!(m.log.get(4).unwrap().text, "after");
        assert_eq!(censored(d.path())[&1].text, "the refused words");
        let day = std::fs::read_dir(d.path().join("main")).unwrap().map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap()).collect::<String>();
        assert!(!day.contains("the refused words"));
        m.append(Kind::Agent, "appends still work", None).unwrap();
        assert!(m.log.check().unwrap().ok());
        release(&mut m, &[1, 2]).unwrap();
        assert_eq!(m.log.get(1).unwrap().text, "the refused words");
        assert_eq!(m.log.get(3).unwrap().text.len(), 5);
        assert!(censored(d.path()).is_empty());
        let mut reopened = crate::log::Log::open(d.path()).unwrap();
        assert_eq!((reopened.len(), reopened.get(5).unwrap().text), (6, "appends still work".to_owned()));
    }
}
