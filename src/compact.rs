//! One compaction: ask a cheap model for a line, check its size, and retry in the same conversation.

use crate::aop::{Aop, AopResult, RunArgs};
use crate::error::{Error, Result};
use crate::memory::Prompt;
use crate::profile::Compactor as Settings;
use crate::tree::{NODE_BYTES, Usage};
use crate::util::cut_bytes;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::oneshot;

pub const MAX_ATTEMPTS: usize = 5;

/// What a raw history's cut asks for after the chat being cut: an analysis, then a summary.
const CUT: &str = include_str!("../prompts/cut.md");

/// The `<summary>` of a reply to the cut prompt, without its `<analysis>`.
pub fn cut_summary(reply: &str) -> String {
    let body = match (reply.find("<summary>"), reply.rfind("</summary>")) {
        (Some(a), Some(b)) if a < b => &reply[a + "<summary>".len()..b],
        _ => reply.rsplit_once("</analysis>").map_or(reply, |(_, s)| s),
    };
    body.trim().to_owned()
}

/// Strip an `id+n|` head the model may have copied from the view, and surrounding whitespace.
pub fn clean_reply(raw: &str) -> String {
    let t = raw.trim();
    let digits = |s: &str| s.bytes().take_while(u8::is_ascii_digit).count();
    let a = digits(t);
    if a > 0 && t[a..].starts_with('+') {
        let b = digits(&t[a + 1..]);
        if b > 0 && t[a + 1 + b..].starts_with('|') {
            return t[a + b + 2..].trim().to_owned();
        }
    }
    t.to_owned()
}

/// The largest size a reply may have and still be accepted without a retry.
fn limit(source_bytes: usize, accept_bytes: usize) -> usize {
    accept_bytes.min(source_bytes.saturating_sub(1))
}

/// A reply is accepted when it fits `accept_bytes` and is strictly smaller than its source.
pub fn accepts(reply: &str, source_bytes: usize, accept_bytes: usize) -> bool {
    !reply.is_empty() && reply.len() <= limit(source_bytes, accept_bytes)
}

/// The message sent in the same conversation when a reply is too long.
pub fn too_long_message(reply: &str, source_bytes: usize) -> String {
    let max = NODE_BYTES.min(source_bytes.saturating_sub(1));
    format!(
        "Too long: your line is {} bytes, over the {max}-byte limit. Write\nthe whole line again for the same <input>, cutting just enough of the\nleast valuable items to fit before this cut:\n{}| ← LIMIT",
        reply.len(),
        cut_bytes(reply, max)
    )
}

/// One AOP call made for a compaction, kept for the cost ledger.
#[derive(Clone, Debug)]
pub struct Call {
    pub run_id: String,
    pub model: Option<String>,
    pub usage: Option<Usage>,
    pub ok: bool,
}

impl Call {
    fn of(r: &AopResult) -> Self {
        Self { run_id: r.run_id.clone(), model: r.model.clone(), usage: r.usage, ok: r.succeeded }
    }
}

pub struct Built {
    pub text: String,
    /// Run that produced the accepted reply.
    pub run: String,
    pub model: Option<String>,
    /// Usage of every attempt together.
    pub usage: Usage,
}

pub struct Compaction {
    pub result: std::result::Result<Built, String>,
    pub calls: Vec<Call>,
}

pub struct Compactor {
    pub aop: Aop,
    pub settings: Settings,
    pub system_file: PathBuf,
    pub scratch: PathBuf,
    counter: AtomicU64,
}

impl Compactor {
    pub fn new(aop: Aop, settings: Settings, system_file: PathBuf, scratch: PathBuf) -> Self {
        Self { aop, settings, system_file, scratch, counter: AtomicU64::new(0) }
    }

    fn prompt_file(&self, text: &str) -> Result<PathBuf> {
        let path = self.scratch.join(format!("prompt-{}-{}.txt", std::process::id(), self.counter.fetch_add(1, Ordering::Relaxed)));
        std::fs::write(&path, text)?;
        Ok(path)
    }

    async fn call(&self, run: Option<&str>, prompt: &Prompt) -> Result<AopResult> {
        let parts = self.settings.prompt_parts && prompt.has_mark() && run.is_none();
        let file = self.prompt_file(&if parts { prompt.parts_json() } else { prompt.text() })?;
        let r = match run {
            None => {
                let args = RunArgs {
                    agent: self.settings.agent.clone(),
                    model: self.settings.model.clone(),
                    effort: self.settings.effort.clone(),
                    profile: "sealed".into(),
                    no_web: true,
                    tools: Some(String::new()),
                    system_prompt_file: Some(self.system_file.clone()),
                    prompt_file: file.clone(),
                    parts,
                    timeout: self.settings.timeout,
                    task: None,
                    ..Default::default()
                };
                self.aop.run(&args).await
            }
            Some(id) => self.aop.resume(id, &file, self.settings.timeout).await,
        };
        let _ = std::fs::remove_file(&file);
        r
    }

    /// One call that summarizes `chat`, the part of a raw history being cut, as `prompts/cut.md`
    /// asks, under the harness's own system prompt with no tools.
    pub async fn summarize(&self, chat: &str, timeout: u64) -> (std::result::Result<String, String>, Vec<Call>) {
        let file = match self.prompt_file(&format!("<chat>\n{chat}</chat>\n\n{CUT}")) {
            Ok(f) => f,
            Err(e) => return (Err(e.to_string()), Vec::new()),
        };
        let args = RunArgs {
            agent: self.settings.agent.clone(),
            model: self.settings.model.clone(),
            effort: self.settings.effort.clone(),
            profile: "sealed".into(),
            no_web: true,
            tools: Some(String::new()),
            prompt_file: file.clone(),
            timeout,
            ..Default::default()
        };
        let r = self.aop.run(&args).await;
        let _ = std::fs::remove_file(&file);
        let r = match r {
            Ok(r) => r,
            Err(e) => return (Err(e.to_string()), Vec::new()),
        };
        self.aop.cleanup(&r.run_id).await;
        let calls = vec![Call::of(&r)];
        let summary = cut_summary(r.reply());
        let out = if !r.succeeded {
            Err(r.failure())
        } else if summary.is_empty() {
            Err(format!("run {} returned no summary", r.run_id))
        } else {
            Ok(summary)
        };
        (out, calls)
    }

    /// Build one node's text. `prompt` is the whole first prompt; `source` is what it summarizes,
    /// which the reply must be strictly smaller than.
    pub async fn build(&self, prompt: &Prompt, source: &str) -> Compaction {
        self.build_signalling(prompt, source, None).await
    }

    /// Like `build`, and sends on `first_reply` once the first call has answered, so callers can
    /// hold other calls on the same cold prefix until it is cached.
    pub async fn build_signalling(&self, prompt: &Prompt, source: &str, mut first_reply: Option<oneshot::Sender<()>>) -> Compaction {
        let mut calls = Vec::new();
        let mut runs = Vec::new();
        let result = self.attempts(prompt, source, &mut calls, &mut runs, &mut first_reply).await;
        if let Some(tx) = first_reply.take() {
            let _ = tx.send(());
        }
        for id in &runs {
            self.aop.cleanup(id).await;
        }
        Compaction { result, calls }
    }

    async fn attempts(
        &self,
        prompt: &Prompt,
        source: &str,
        calls: &mut Vec<Call>,
        runs: &mut Vec<String>,
        first_reply: &mut Option<oneshot::Sender<()>>,
    ) -> std::result::Result<Built, String> {
        let source_bytes = source.len();
        let mut usage = Usage::default();
        let mut best: Option<(String, String, Option<String>)> = None;
        let mut text = prompt.clone();
        let mut last_run: Option<String> = None;
        for attempt in 1..=MAX_ATTEMPTS {
            let r = self.call(last_run.as_deref(), &text).await;
            if let Some(tx) = first_reply.take() {
                let _ = tx.send(());
            }
            let r = r.map_err(|e: Error| e.to_string())?;
            runs.push(r.run_id.clone());
            calls.push(Call::of(&r));
            if let Some(u) = &r.usage {
                usage.add(u);
            }
            if !r.succeeded && r.refusal.is_some() {
                // A refusal is the input's doing and would recur on every retry, so the node is
                // its source clipped, as UniiChat does.
                let text = cut_bytes(source, NODE_BYTES.min(source_bytes.saturating_sub(1))).to_owned();
                return Ok(Built { text, run: r.run_id, model: r.model, usage });
            }
            if !r.succeeded {
                return Err(r.failure());
            }
            let reply = clean_reply(r.reply());
            if reply.is_empty() {
                return Err(format!("run {} returned an empty reply", r.run_id));
            }
            let done = accepts(&reply, source_bytes, self.settings.accept_bytes);
            if best.as_ref().is_none_or(|(b, ..)| reply.len() < b.len()) {
                best = Some((reply.clone(), r.run_id.clone(), r.model.clone()));
            }
            if done || attempt == MAX_ATTEMPTS {
                break;
            }
            text = Prompt(vec![(too_long_message(&reply, source_bytes), false)]);
            last_run = Some(r.run_id);
        }
        let (text, run, model) = best.expect("at least one attempt");
        // After the last retry the shortest reply stands if it is smaller than its source and
        // within the hard cap. Anything longer is a failure, never a stored degraded line.
        if text.len() < source_bytes && text.len() <= self.settings.hard_bytes.max(self.settings.accept_bytes) {
            Ok(Built { text, run, model, usage })
        } else {
            Err(format!("no reply under {} bytes in {MAX_ATTEMPTS} attempts (shortest {} bytes, source {source_bytes} bytes)", self.settings.hard_bytes, text.len()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_a_copied_head() {
        assert_eq!(clean_reply("  40+8|user: hi \n"), "user: hi");
        assert_eq!(clean_reply("0+1|x|y"), "x|y");
        assert_eq!(clean_reply("user: 1+2|3"), "user: 1+2|3");
        assert_eq!(clean_reply("12+|x"), "12+|x");
        assert_eq!(clean_reply("+5|x"), "+5|x");
    }

    #[test]
    fn acceptance_needs_size_and_a_smaller_line_than_the_source() {
        assert!(accepts(&"a".repeat(640), 2_000, 640));
        assert!(!accepts(&"a".repeat(641), 2_000, 640));
        assert!(!accepts(&"a".repeat(513), 2_000, 512));
        assert!(!accepts("", 2_000, 640));
        // A merge of two short lines must come back smaller than both together.
        assert!(!accepts(&"a".repeat(600), 600, 640));
        assert!(accepts(&"a".repeat(599), 600, 640));
    }

    #[test]
    fn too_long_message_quotes_the_first_512_bytes() {
        let reply = format!("{}é{}", "a".repeat(511), "b".repeat(300));
        let m = too_long_message(&reply, 5_000);
        assert!(m.starts_with("Too long: your line is 813 bytes, over the 512-byte limit. Write\nthe whole line again for the same <input>, cutting just enough of the\nleast valuable items to fit before this cut:\n"));
        assert!(m.ends_with(&format!("{}| ← LIMIT", "a".repeat(511))));
        assert!(too_long_message("x".repeat(450).as_str(), 400).contains("over the 399-byte limit"));
    }

    use crate::testkit::{FakeAop, real_aop_installed};
    use serde_json::json;

    fn compactor(fake: &FakeAop) -> Compactor {
        let system = fake.path().join("system.md");
        std::fs::write(&system, "system").unwrap();
        let scratch = fake.path().join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        Compactor::new(fake.aop(), Settings::default(), system, scratch)
    }

    fn src(n: usize) -> String {
        "s".repeat(n)
    }

    fn prompt() -> Prompt {
        Prompt(vec![("<chat>\n</chat>\nCompaction: compress message 3 into one line\n<input>\nuser: hello there\n</input>".into(), false)])
    }

    #[tokio::test]
    async fn an_acceptable_reply_is_one_call_then_cleanup() {
        let fake = FakeAop::new(json!({"compact": "ok"}));
        let out = compactor(&fake).build(&prompt(), &src(2_000)).await;
        let built = out.result.unwrap();
        assert!(built.text.starts_with("user: hello there") && built.text.len() == 300);
        assert_eq!((out.calls.len(), built.run.as_str(), built.usage.output_tokens), (1, "fake-1", 40));
        let kinds: Vec<String> = fake.calls().iter().map(|c| c["cmd"].as_str().unwrap().to_owned()).collect();
        assert_eq!(kinds, ["run", "cleanup"]);
        let args = fake.calls()[0]["args"].to_string();
        for want in ["--profile", "sealed", "--no-web", "--tools", "--system-prompt-file", "--json"] {
            assert!(args.contains(want), "{args}");
        }
    }

    #[tokio::test]
    async fn too_long_replies_retry_in_the_same_conversation_and_sum_usage() {
        let fake = FakeAop::new(json!({"compact": "long:2"}));
        let out = compactor(&fake).build(&prompt(), &src(2_000)).await;
        let built = out.result.unwrap();
        assert_eq!(built.text.len(), 300);
        assert_eq!(built.run, "fake-3");
        assert_eq!(built.usage.output_tokens, 120);
        let calls = fake.calls();
        assert_eq!((calls[1]["cmd"].as_str(), calls[1]["from"].as_str()), (Some("resume"), Some("fake-1")));
        assert_eq!(calls[2]["from"].as_str(), Some("fake-2"));
        assert!(calls[1]["prompt"].as_str().unwrap().starts_with("Too long: your line is 700 bytes, over the 512-byte limit."));
        let cleaned: Vec<&str> = calls.iter().filter(|c| c["cmd"] == "cleanup").map(|c| c["run_id"].as_str().unwrap()).collect();
        assert_eq!(cleaned, ["fake-1", "fake-2", "fake-3"]);
    }

    #[tokio::test]
    async fn after_five_attempts_the_shortest_reply_within_the_hard_cap_stands() {
        let fake = FakeAop::new(json!({"compact": "always_long"}));
        let out = compactor(&fake).build(&prompt(), &src(2_000)).await;
        assert_eq!(out.calls.len(), MAX_ATTEMPTS);
        assert_eq!(out.result.unwrap().text.len(), 900);
    }

    #[tokio::test]
    async fn a_reply_over_the_hard_cap_writes_no_node() {
        let fake = FakeAop::new(json!({"compact": "never_small"}));
        let out = compactor(&fake).build(&prompt(), &src(5_000)).await;
        assert_eq!(out.calls.len(), MAX_ATTEMPTS);
        assert!(out.result.err().unwrap().contains("no reply under 1024 bytes"));
    }

    #[test]
    fn a_cut_summary_drops_the_analysis() {
        assert_eq!(cut_summary("<analysis>\nthinking\n</analysis>\n\n<summary>\n1. Intent: x\n</summary>"), "1. Intent: x");
        assert_eq!(cut_summary("<analysis>a</analysis>\nplain notes"), "plain notes");
        assert_eq!(cut_summary("just notes"), "just notes");
    }

    #[tokio::test]
    async fn a_refused_input_becomes_its_source_clipped_without_retrying() {
        let fake = FakeAop::new(json!({"compact": "refuse"}));
        let source = format!("user: {}", "ab".repeat(400));
        let out = compactor(&fake).build(&prompt(), &source).await;
        assert_eq!(out.result.unwrap().text, source[..NODE_BYTES]);
        assert_eq!(out.calls.len(), 1);
    }

    #[tokio::test]
    async fn an_empty_reply_and_a_failed_call_are_errors() {
        let fake = FakeAop::new(json!({"compact": "empty"}));
        assert!(compactor(&fake).build(&prompt(), &src(2_000)).await.result.err().unwrap().contains("empty reply"));
        fake.set_scenario(json!({"compact": "fail"}));
        let out = compactor(&fake).build(&prompt(), &src(2_000)).await;
        assert!(out.result.err().unwrap().contains("failed"));
        assert_eq!(out.calls.len(), 1);
        assert!(!out.calls[0].ok);
    }

    #[tokio::test]
    async fn a_copied_head_is_stripped_before_the_size_check() {
        let fake = FakeAop::new(json!({"compact": "prefixed"}));
        let built = compactor(&fake).build(&prompt(), &src(2_000)).await.result.unwrap();
        assert!(built.text.starts_with("user: hello"), "{}", built.text);
    }

    #[tokio::test]
    async fn invalid_flags_are_rejected_as_the_real_aop_rejects_them() {
        if !real_aop_installed() {
            return;
        }
        let fake = FakeAop::new(json!({"compact": "ok"}));
        let mut c = compactor(&fake);
        c.settings.effort = "bogus".into();
        let err = c.build(&prompt(), &src(2_000)).await.result.err().unwrap();
        assert!(err.contains("invalid choice"), "{err}");
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn a_marked_prompt_goes_out_as_prompt_parts_and_a_retry_as_text() {
        let fake = FakeAop::new(json!({"compact": "long:1"}));
        let rows: Vec<String> = (0..9).map(|i| format!("{i}+1|line {i}\n")).collect();
        let prompt = crate::memory::view_prompt(&rows, "Compaction: compress message 9\n<input>\nuser: hi\n</input>");
        let built = compactor(&fake).build(&prompt, &src(2_000)).await.result.unwrap();
        assert_eq!(built.text.len(), 300);
        let calls = fake.calls();
        assert!(calls[0]["args"].to_string().contains("--prompt-parts"), "{}", calls[0]["args"]);
        assert_eq!(calls[0]["marks"], 1);
        assert_eq!(calls[0]["prompt"].as_str().unwrap(), prompt.text());
        assert!(calls[1]["args"].to_string().contains("--prompt-file") && calls[1]["prompt"].as_str().unwrap().starts_with("Too long"));
    }

    #[tokio::test]
    async fn prompt_parts_can_be_turned_off() {
        let fake = FakeAop::new(json!({"compact": "ok"}));
        let rows: Vec<String> = (0..9).map(|i| format!("{i}+1|line {i}\n")).collect();
        let prompt = crate::memory::view_prompt(&rows, "Compaction: x\n<input>\ny\n</input>");
        let mut c = compactor(&fake);
        c.settings.prompt_parts = false;
        c.build(&prompt, &src(2_000)).await.result.unwrap();
        assert!(fake.calls()[0]["args"].to_string().contains("--prompt-file"));
    }
}
