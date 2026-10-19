//! The raw-history prompt: the chat so far word for word instead of the view of summaries.
//!
//! Tool results longer than `stub_chars` are replaced by a one-line stub once they are older than
//! the previous turn, and when the raw part passes `high` bytes its oldest messages are cut down to
//! the chat view's summary lines until it is under `low`. The cut and the summary lines as rendered
//! then are kept in `raw.json`, so every turn until the next cut sends the same prefix, however the
//! view changes meanwhile.
//!
//! Each turn is one cache block, and the mark sits on the last block before the previous turn: that
//! part never changes again, while the previous turn's results are stubbed on the next turn. The
//! next prompt is one block longer, so its lookback finds this mark.

use crate::error::Result;
use crate::log::{Kind, Message};
use crate::memory::{Memory, Prompt};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs;

pub struct Limits {
    pub stub_chars: usize,
    pub high: usize,
    pub low: usize,
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    /// The first message sent word for word.
    cut: u64,
    /// The view lines covering the messages before `cut`, rendered when the cut was made.
    #[serde(default)]
    summary: String,
}

/// Written so the model takes the `agent` entries as its own replies and keeps to their rules and
/// length: on replayed turns of a real chat this answered as often and as long as sending the same
/// history as real conversation turns, and shorter and less often than a neutral description.
const INTRO: &str = "<chat>\nThis is the conversation you have been taking part in, oldest first, one message per entry as `id|kind: text`. `agent` entries are your own earlier replies, exactly as you sent them; `tool` and `echo` entries are your tool calls and their results; `user` entries are the messages you received; `work` and `note` entries are reports and notes. A large tool result older than the previous turn is replaced by a stub: run the call again if you need it. The message after </chat> is the next one you received. Your final response is your next `agent` entry: follow the same instructions and rules your earlier replies followed, and match their length and style.\n";

const SUMMARY_NOTE: &str = "The oldest messages are summarized as `id+n|text`, one line for the n messages from id on.";

const ZOOM_NOTE: &str = " To read the messages under a line, write `zoom(id, n)` alone on a line and the result comes back to you; `zoom(id, 1)` returns message id whole.";

/// The message as one transcript entry, with a stub in place of a long old result.
fn entry(m: &Message, call: Option<&Message>, stub: bool, lim: &Limits) -> String {
    let chars = m.text.chars().count();
    if stub && m.kind == Kind::Echo && chars > lim.stub_chars {
        return match call {
            Some(c) => format!("{}|echo: [stub: {chars} characters returned by {} in message {}]\n", m.i, c.text.split_whitespace().next().unwrap_or("the call"), c.i),
            None => format!("{}|echo: [stub: {chars} characters returned by an earlier call]\n", m.i),
        };
    }
    format!("{}|{}: {}\n", m.i, m.kind.as_str(), m.text)
}

/// The call each result answers. Results come back in the order of their calls, so with parallel
/// calls the first result after them belongs to the first call.
fn calls(msgs: &[Message]) -> Vec<Option<usize>> {
    let mut pending = VecDeque::new();
    msgs.iter()
        .enumerate()
        .map(|(k, m)| match m.kind {
            Kind::Tool => {
                pending.push_back(k);
                None
            }
            Kind::Echo => pending.pop_front(),
            Kind::User => {
                pending.clear();
                None
            }
            _ => None,
        })
        .collect()
}

/// Messages from `from` on, with each page run joined into its first message.
fn messages(mem: &mut Memory, from: u64) -> Result<Vec<Message>> {
    let mut out: Vec<Message> = Vec::new();
    let mut joining = false;
    for i in from..mem.log.len() {
        let m = mem.log.get(i)?;
        match out.last_mut() {
            Some(prev) if joining => prev.text.push_str(&m.text),
            _ => out.push(m.clone()),
        }
        joining = m.more;
    }
    Ok(out)
}

pub fn prompt(mem: &mut Memory, lim: &Limits, zoom: bool, tail: &str) -> Result<Prompt> {
    let path = mem.dir.join("raw.json");
    let mut saved: Saved = fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    loop {
        let msgs = messages(mem, saved.cut)?;
        // A turn starts at each message the user sent; one delivered during a run carries that run.
        let turns: Vec<usize> = (0..msgs.len()).filter(|&k| msgs[k].kind == Kind::User && msgs[k].run.is_none()).collect();
        let prev = turns.last().copied().unwrap_or(0);
        let calls = calls(&msgs);
        let entries: Vec<String> = (0..msgs.len()).map(|k| entry(&msgs[k], calls[k].map(|c| &msgs[c]), k < prev, lim)).collect();
        let bytes: usize = entries.iter().map(String::len).sum();
        if bytes > lim.high {
            // Cut at the first view line boundary that leaves at most `low` bytes, or failing that at
            // the last one before the previous turn, which is never cut into.
            let mut left = bytes;
            let mut cut = None;
            for (k, e) in entries.iter().enumerate().take(prev) {
                left -= e.len();
                let next = msgs[k + 1].i;
                if mem.chat.lines().iter().any(|l| l.end() == next) {
                    cut = Some(next);
                    if left <= lim.low {
                        break;
                    }
                }
            }
            if let Some(c) = cut {
                // The view lines tile the log, so those ending by `c` cover exactly the messages before it.
                let lines: Vec<_> = mem.chat.lines().iter().filter(|l| l.end() <= c).copied().collect();
                saved = Saved { cut: c, summary: mem.render(&lines)? };
                fs::write(&path, serde_json::to_string(&saved)?)?;
                continue;
            }
        }
        let mut head = INTRO.to_owned();
        if !saved.summary.is_empty() {
            head += SUMMARY_NOTE;
            if zoom {
                head += ZOOM_NOTE;
            }
            head += "\n<summary>\n";
            head += &saved.summary;
            head += "</summary>\n";
        }
        let mut parts = vec![(head, false)];
        let mut bounds = turns.clone();
        if bounds.first() != Some(&0) {
            bounds.insert(0, 0);
        }
        for w in bounds.windows(2) {
            parts.push((entries[w[0]..w[1]].concat(), false));
        }
        let last = parts.len() - 1;
        parts[last].1 = true;
        parts.push((entries[prev..].concat() + "</chat>\n" + tail, false));
        parts.retain(|(t, _)| !t.is_empty());
        return Ok(Prompt(parts));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::tests::{limits, long, stub_finish};

    fn mem(dir: &std::path::Path) -> Memory {
        Memory::open(dir, &limits(), 2).unwrap()
    }

    const LIM: Limits = Limits { stub_chars: 50, high: 100_000, low: 50_000 };

    fn turn(m: &mut Memory, user: &str, result: &str) {
        m.append(Kind::User, user, None).unwrap();
        m.append(Kind::Tool, "Bash {\"command\":\"ls\"}", Some("r")).unwrap();
        m.append(Kind::Echo, result, Some("r")).unwrap();
        m.append(Kind::Agent, "done", Some("r")).unwrap();
    }

    #[test]
    fn old_results_are_stubbed_and_the_mark_ends_before_the_previous_turn() {
        let d = tempfile::tempdir().unwrap();
        let mut m = mem(d.path());
        turn(&mut m, "first", &"a".repeat(60));
        turn(&mut m, "second", &"b".repeat(60));
        turn(&mut m, "third", &"c".repeat(60));
        let p = prompt(&mut m, &LIM, true, "next").unwrap();
        let text = p.text();
        assert!(text.contains("2|echo: [stub: 60 characters returned by Bash in message 1]"), "{text}");
        assert!(text.contains("6|echo: [stub: 60 characters"));
        assert!(text.ends_with(&format!("10|echo: {}\n11|agent: done\n</chat>\nnext", "c".repeat(60))), "the previous turn keeps its results");
        assert!(!text.contains("<summary>"));
        let marked: Vec<usize> = (0..p.0.len()).filter(|&k| p.0[k].1).collect();
        assert_eq!(marked.len(), 1);
        assert!(p.0[marked[0]].0.starts_with("4|user: second"), "{:?}", p.0);
        assert!(p.0[marked[0] + 1].0.starts_with("8|user: third"));
    }

    #[test]
    fn the_next_prompt_repeats_every_block_up_to_the_mark() {
        let d = tempfile::tempdir().unwrap();
        let mut m = mem(d.path());
        turn(&mut m, "first", &"a".repeat(60));
        turn(&mut m, "second", &"b".repeat(60));
        let before = prompt(&mut m, &LIM, true, "x").unwrap();
        turn(&mut m, "third", "short");
        let after = prompt(&mut m, &LIM, true, "y").unwrap();
        let mark = before.0.iter().position(|(_, c)| *c).unwrap();
        assert_eq!(before.0[..=mark].iter().map(|(t, _)| t).collect::<Vec<_>>(), after.0[..=mark].iter().map(|(t, _)| t).collect::<Vec<_>>());
        assert_eq!(after.0.iter().position(|(_, c)| *c), Some(mark + 1));
    }

    #[test]
    fn a_long_history_is_cut_to_summary_lines_and_the_cut_stays() {
        let d = tempfile::tempdir().unwrap();
        let mut m = mem(d.path());
        for k in 0..40 {
            m.append(Kind::User, &format!("question {k}"), None).unwrap();
            m.append(Kind::Agent, &long(3000), Some("r")).unwrap();
        }
        let lim = Limits { stub_chars: 1000, high: 60_000, low: 30_000 };
        let p = prompt(&mut m, &lim, true, "now").unwrap();
        let text = p.text();
        assert!(text.contains("<summary>\n0+"), "{}", &text[..300]);
        assert!(text.contains("zoom(id, n)"));
        let raw = &text[text.find("</summary>").unwrap()..];
        assert!(raw.len() <= 31_000 && raw.contains("79|agent"), "{}", raw.len());
        let cut: Saved = serde_json::from_str(&fs::read_to_string(d.path().join("raw.json")).unwrap()).unwrap();
        assert!(cut.cut > 0);
        // Summaries finishing and the view merging change the view, but not the summary sent.
        while let Some(job) = m.sched.next(0) {
            stub_finish(&mut m, job, 200);
        }
        m.append(Kind::User, "one more", None).unwrap();
        let again = prompt(&mut m, &lim, true, "now").unwrap();
        assert_ne!(m.render(&m.chat.lines().to_vec()).unwrap(), text[text.find("<summary>\n").unwrap() + 10..text.find("</summary>").unwrap()]);
        assert_eq!(again.0[0], p.0[0], "the summary block is the same until the next cut");
    }

    #[test]
    fn parallel_results_are_stubbed_with_their_own_calls() {
        let d = tempfile::tempdir().unwrap();
        let mut m = mem(d.path());
        m.append(Kind::User, "go", None).unwrap();
        m.append(Kind::Tool, "Read {}", Some("r")).unwrap();
        m.append(Kind::Tool, "Bash {}", Some("r")).unwrap();
        m.append(Kind::Echo, &"a".repeat(60), Some("r")).unwrap();
        m.append(Kind::Echo, &"b".repeat(70), Some("r")).unwrap();
        m.append(Kind::User, "while you work", Some("r")).unwrap();
        turn(&mut m, "next", "short");
        let text = prompt(&mut m, &LIM, true, "t").unwrap().text();
        assert!(text.contains("3|echo: [stub: 60 characters returned by Read in message 1]"), "{text}");
        assert!(text.contains("4|echo: [stub: 70 characters returned by Bash in message 2]"));
    }

    #[test]
    fn a_previous_turn_larger_than_low_still_cuts_before_it() {
        let d = tempfile::tempdir().unwrap();
        let mut m = mem(d.path());
        for k in 0..20 {
            m.append(Kind::User, &format!("question {k}"), None).unwrap();
            m.append(Kind::Agent, &long(1000), Some("r")).unwrap();
        }
        m.append(Kind::User, "big", None).unwrap();
        for _ in 0..10 {
            m.append(Kind::Agent, &long(1000), Some("r")).unwrap();
        }
        let lim = Limits { stub_chars: 1000, high: 60_000, low: 10_000 };
        let text = prompt(&mut m, &lim, true, "now").unwrap().text();
        assert!(text.contains("<summary>") && text.contains("40|user: big"), "{}", &text[..300]);
    }

    #[test]
    fn page_runs_are_one_message() {
        let d = tempfile::tempdir().unwrap();
        let mut m = mem(d.path());
        let text = "x".repeat(crate::log::PAGE_CHARS + 10);
        m.append(Kind::User, &text, None).unwrap();
        let p = prompt(&mut m, &LIM, true, "t").unwrap().text();
        assert!(p.contains(&format!("0|user: {text}\n</chat>")));
    }
}
