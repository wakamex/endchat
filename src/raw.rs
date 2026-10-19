//! The raw-history prompt: the chat so far word for word instead of the view of summaries.
//!
//! Tool results longer than `stub_chars` are replaced by a one-line stub once they are older than
//! the previous turn, and tool calls that long, such as a command that writes a whole script, are
//! cut to their start. When the raw part passes `high` bytes, its oldest turns are cut off until
//! under `low` bytes remain, and one call summarizes what was cut, together with the summary of
//! any earlier cut. The cut and that summary are kept in `raw.json`, so every turn until the next
//! cut sends the same prefix.
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
use std::path::Path;

pub struct Limits {
    pub stub_chars: usize,
    pub high: usize,
    pub low: usize,
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    /// The first message sent word for word.
    cut: u64,
    /// The summary of the messages before `cut`.
    #[serde(default)]
    summary: String,
}

/// Written so the model takes the `agent` entries as its own replies and keeps to their rules and
/// length: on replayed turns of a real chat this answered as often and as long as sending the same
/// history as real conversation turns, and shorter and less often than a neutral description.
const INTRO: &str = "<chat>\nThis is the conversation you have been taking part in, oldest first, one message per entry as `id|kind: text`. `agent` entries are your own earlier replies, exactly as you sent them; `tool` and `echo` entries are your tool calls and their results; `user` entries are the messages you received; `work` and `note` entries are reports and notes. A large tool result older than the previous turn is replaced by a stub: run the call again if you need it. The message after </chat> is the next one you received. Your final response is your next `agent` entry: follow the same instructions and rules your earlier replies followed, and match their length and style.\n";

/// Characters an old long tool call keeps: enough to name the tool and show what it ran.
const CALL_HEAD: usize = 200;

/// The message as one transcript entry, with a stub in place of a long old result and only the
/// start of a long old call.
fn entry(m: &Message, call: Option<&Message>, stub: bool, lim: &Limits) -> String {
    let chars = m.text.chars().count();
    if stub && m.kind == Kind::Tool && chars > lim.stub_chars.max(CALL_HEAD) {
        let head: String = m.text.chars().take(CALL_HEAD).collect();
        return format!("{}|tool: {head} [... {chars} characters in all]\n", m.i);
    }
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

fn saved(dir: &Path) -> Saved {
    fs::read_to_string(dir.join("raw.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// The history since the last cut as entries, the index of each turn's first entry, and the index
/// of the previous turn's.
struct History {
    msgs: Vec<Message>,
    entries: Vec<String>,
    turns: Vec<usize>,
    prev: usize,
}

fn history(mem: &mut Memory, from: u64, lim: &Limits) -> Result<History> {
    let msgs = messages(mem, from)?;
    // A turn starts at each message the user sent; one delivered during a run carries that run.
    let turns: Vec<usize> = (0..msgs.len()).filter(|&k| msgs[k].kind == Kind::User && msgs[k].run.is_none()).collect();
    let prev = turns.last().copied().unwrap_or(0);
    let calls = calls(&msgs);
    let entries = (0..msgs.len()).map(|k| entry(&msgs[k], calls[k].map(|c| &msgs[c]), k < prev, lim)).collect();
    Ok(History { msgs, entries, turns, prev })
}

/// The cut due now, if the history has passed `high`: the first message to keep and the text to
/// summarize (the earlier summary, then the messages cut off). It cuts at the first turn that
/// leaves at most `low` bytes, or failing that at the previous turn, which is never cut into.
pub fn due_cut(mem: &mut Memory, lim: &Limits) -> Result<Option<(u64, String)>> {
    let s = saved(&mem.dir);
    let h = history(mem, s.cut, lim)?;
    let bytes: usize = h.entries.iter().map(String::len).sum();
    if bytes <= lim.high {
        return Ok(None);
    }
    let mut left = bytes;
    let mut at = None;
    for k in 0..h.prev {
        left -= h.entries[k].len();
        if h.turns.contains(&(k + 1)) {
            at = Some(k + 1);
            if left <= lim.low {
                break;
            }
        }
    }
    let Some(at) = at else { return Ok(None) };
    let mut input = String::new();
    if !s.summary.is_empty() {
        input += &format!("<summary of messages before {}>\n{}\n</summary>\n", s.cut, s.summary.trim_end());
    }
    input += &h.entries[..at].concat();
    Ok(Some((h.msgs[at].i, input)))
}

/// Keep `summary` as the summary of the messages before `cut`.
pub fn save_cut(dir: &Path, cut: u64, summary: &str) -> Result<()> {
    fs::write(dir.join("raw.json"), serde_json::to_string(&Saved { cut, summary: summary.trim().to_owned() })?)?;
    Ok(())
}

pub fn prompt(mem: &mut Memory, lim: &Limits, tail: &str) -> Result<Prompt> {
    let s = saved(&mem.dir);
    let h = history(mem, s.cut, lim)?;
    let mut head = INTRO.to_owned();
    if !s.summary.is_empty() {
        head += &format!("The chat before message {} is summarized here; the messages themselves are kept in full in the log.\n<summary>\n{}\n</summary>\n", s.cut, s.summary);
    }
    let mut parts = vec![(head, false)];
    let mut bounds = h.turns.clone();
    if bounds.first() != Some(&0) {
        bounds.insert(0, 0);
    }
    for w in bounds.windows(2) {
        parts.push((h.entries[w[0]..w[1]].concat(), false));
    }
    let last = parts.len() - 1;
    parts[last].1 = true;
    parts.push((h.entries[h.prev..].concat() + "</chat>\n" + tail, false));
    parts.retain(|(t, _)| !t.is_empty());
    Ok(Prompt(parts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::tests::{limits, long};

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
        let p = prompt(&mut m, &LIM, "next").unwrap();
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
        let before = prompt(&mut m, &LIM, "x").unwrap();
        turn(&mut m, "third", "short");
        let after = prompt(&mut m, &LIM, "y").unwrap();
        let mark = before.0.iter().position(|(_, c)| *c).unwrap();
        assert_eq!(before.0[..=mark].iter().map(|(t, _)| t).collect::<Vec<_>>(), after.0[..=mark].iter().map(|(t, _)| t).collect::<Vec<_>>());
        assert_eq!(after.0.iter().position(|(_, c)| *c), Some(mark + 1));
    }

    #[test]
    fn a_long_history_is_cut_at_a_turn_and_the_summary_heads_every_later_prompt() {
        let d = tempfile::tempdir().unwrap();
        let mut m = mem(d.path());
        for k in 0..40 {
            m.append(Kind::User, &format!("question {k}"), None).unwrap();
            m.append(Kind::Agent, &long(3000), Some("r")).unwrap();
        }
        let lim = Limits { stub_chars: 1000, high: 60_000, low: 30_000 };
        assert!(due_cut(&mut m, &Limits { stub_chars: 1000, high: 600_000, low: 300_000 }).unwrap().is_none());
        let (cut, input) = due_cut(&mut m, &lim).unwrap().unwrap();
        assert!(cut % 2 == 0 && cut > 0, "the cut lands on a user message: {cut}");
        assert!(input.starts_with("0|user: question 0\n") && input.ends_with(&format!("{}|agent: {}\n", cut - 1, long(3000))));
        save_cut(d.path(), cut, "We talked about forty questions.").unwrap();
        let p = prompt(&mut m, &lim, "now").unwrap();
        let text = p.text();
        assert!(text.contains(&format!("before message {cut} is summarized here")) && text.contains("<summary>\nWe talked about forty questions.\n</summary>"));
        let raw = &text[text.find("</summary>").unwrap()..];
        assert!(raw.len() <= 31_000 && raw.contains("79|agent") && !raw.contains(&format!("\n{}|", cut - 1)), "{}", raw.len());
        m.append(Kind::User, "one more", None).unwrap();
        assert_eq!(prompt(&mut m, &lim, "now").unwrap().0[0], p.0[0], "the summary block is the same until the next cut");
        // A later cut summarizes the earlier summary along with what it cuts.
        for k in 0..20 {
            m.append(Kind::User, &format!("more {k}"), None).unwrap();
            m.append(Kind::Agent, &long(3000), Some("r")).unwrap();
        }
        let (later, input) = due_cut(&mut m, &lim).unwrap().unwrap();
        assert!(later > cut && input.starts_with(&format!("<summary of messages before {cut}>\nWe talked about forty questions.\n</summary>\n{cut}|user:")));
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
        let text = prompt(&mut m, &LIM, "t").unwrap().text();
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
        assert_eq!(due_cut(&mut m, &lim).unwrap().unwrap().0, 40);
    }

    #[test]
    fn a_long_old_call_keeps_only_its_start() {
        let d = tempfile::tempdir().unwrap();
        let mut m = mem(d.path());
        let script = format!("Bash {{\"command\":\"cat > plot.py <<'E'\\n{}\"}}", "x = 1\\n".repeat(100));
        for user in ["first", "second", "third"] {
            m.append(Kind::User, user, None).unwrap();
            m.append(Kind::Tool, &script, Some("r")).unwrap();
            m.append(Kind::Echo, "ok", Some("r")).unwrap();
        }
        let lim = Limits { stub_chars: 300, ..LIM };
        let text = prompt(&mut m, &lim, "t").unwrap().text();
        let head: String = script.chars().take(200).collect();
        let stub = format!("|tool: {head} [... {} characters in all]\n", script.chars().count());
        assert!(text.contains(&format!("1{stub}")) && text.contains(&format!("4{stub}")), "{text}");
        assert!(text.contains(&format!("7|tool: {script}\n")), "the previous turn keeps its call whole");
    }

    #[test]
    fn page_runs_are_one_message() {
        let d = tempfile::tempdir().unwrap();
        let mut m = mem(d.path());
        let text = "x".repeat(crate::log::PAGE_CHARS + 10);
        m.append(Kind::User, &text, None).unwrap();
        let p = prompt(&mut m, &LIM, "t").unwrap().text();
        assert!(p.contains(&format!("0|user: {text}\n</chat>")));
    }
}
