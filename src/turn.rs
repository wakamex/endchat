//! A turn: one user message, one fresh AOP run over the view, and a short loop of tool rounds.
//!
//! AOP cannot yet relay a tool server into a run, so `zoom` and `date` are called as text: the
//! model writes the call alone on a line, endchat answers it and continues the same run with
//! `aop resume`. Nothing else carries over between turns.

use crate::aop::{AopResult, RunArgs};
use crate::compact::Call;
use crate::engine::Engine;
use crate::error::{Result, msg};
use crate::events::{Event, parse_claude};
use crate::log::Kind;
use crate::util::now_iso;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

/// How long a turn waits for earlier messages to be summarized before it goes on with placeholders.
const SUMMARY_WAIT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEvent {
    Queued,
    Agent(String),
    Tool(String),
    Echo(String),
    Error(String),
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCall {
    Zoom { id: u64, n: u64, offset: usize },
    Date { id: u64 },
}

impl ToolCall {
    pub fn log_text(&self) -> String {
        match self {
            ToolCall::Zoom { id, n, offset: 0 } => format!("zoom {{\"id\":{id},\"n\":{n}}}"),
            ToolCall::Zoom { id, n, offset } => format!("zoom {{\"id\":{id},\"n\":{n},\"offset\":{offset}}}"),
            ToolCall::Date { id } => format!("date {{\"id\":{id}}}"),
        }
    }

    pub fn label(&self) -> String {
        match self {
            ToolCall::Zoom { id, n, offset: 0 } => format!("zoom({id}, {n})"),
            ToolCall::Zoom { id, n, offset } => format!("zoom({id}, {n}, {offset})"),
            ToolCall::Date { id } => format!("date({id})"),
        }
    }
}

/// A line that is exactly one call, optionally in backticks.
fn parse_line(line: &str) -> Option<ToolCall> {
    let t = line.trim().trim_matches('`').trim();
    let (name, rest) = t.split_once('(')?;
    let args: Vec<u64> = rest.strip_suffix(')')?.split(',').map(|a| a.trim().parse().ok()).collect::<Option<_>>()?;
    match (name, args.as_slice()) {
        ("zoom", [id, n]) => Some(ToolCall::Zoom { id: *id, n: *n, offset: 0 }),
        ("zoom", [id, n, off]) => Some(ToolCall::Zoom { id: *id, n: *n, offset: *off as usize }),
        ("date", [id]) => Some(ToolCall::Date { id: *id }),
        _ => None,
    }
}

/// The reply with XML-style calls (`<invoke name="zoom"><parameter name="id">3</parameter>...`) rewritten
/// as call lines, which some replies use instead of the prompt's syntax.
fn normalize(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("<invoke name=\"") {
        out += &rest[..at];
        let from = &rest[at + "<invoke name=\"".len()..];
        let name = from.split('"').next().unwrap_or("");
        let end = from.find("</invoke>").map(|e| e + "</invoke>".len()).or_else(|| from.find("<invoke ")).unwrap_or(from.len());
        let (body, after) = from.split_at(end);
        let mut args: Vec<(&str, &str)> = Vec::new();
        let mut p = body;
        while let Some(i) = p.find("<parameter name=\"") {
            p = &p[i + "<parameter name=\"".len()..];
            let Some((key, tail)) = p.split_once("\">") else { break };
            let value = tail.split("</parameter>").next().unwrap_or("").trim();
            args.push((key, value));
        }
        let arg = |k: &str| args.iter().find(|(a, _)| *a == k).map(|(_, v)| *v);
        match (name, arg("id")) {
            ("zoom", Some(id)) => {
                out += &format!("\nzoom({id}, {}{})\n", arg("n").unwrap_or("1"), arg("offset").map_or(String::new(), |o| format!(", {o}")));
            }
            ("date", Some(id)) => out += &format!("\ndate({id})\n"),
            _ => out += "\n<invoke>\n",
        }
        rest = after;
    }
    out + rest
}

pub fn parse_calls(text: &str) -> Vec<ToolCall> {
    normalize(text).lines().filter_map(parse_line).collect()
}

/// A leftover tool-call fragment that parsed to nothing.
fn is_fragment(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("<invoke") || t.starts_with("</invoke") || t.starts_with("<parameter")
}

/// The reply has a tool call that could not be read.
pub fn has_broken_call(text: &str) -> bool {
    normalize(text).lines().any(is_fragment)
}

/// The reply without its call lines.
pub fn strip_calls(text: &str) -> String {
    normalize(text).lines().filter(|l| parse_line(l).is_none() && !is_fragment(l)).collect::<Vec<_>>().join("\n").trim().to_owned()
}

const BROKEN_CALL: &str = "Your last reply held a tool call that could not be read. Write each call alone on a line, such as zoom(40, 8) or date(7), then answer the user once you have what you need.\n";

fn emit(tx: &UnboundedSender<TurnEvent>, e: TurnEvent) {
    let _ = tx.send(e);
}

/// Accept a user message. If a turn is running it is queued for that turn; otherwise a turn
/// starts, followed by one for each message queued while it ran.
pub async fn say(engine: Arc<Engine>, text: String, date: Option<String>, tx: UnboundedSender<TurnEvent>) -> Result<()> {
    if engine.turn_running.swap(true, Ordering::SeqCst) {
        engine.inbox.lock().expect("inbox lock poisoned").push_back(text);
        emit(&tx, TurnEvent::Queued);
        emit(&tx, TurnEvent::Done);
        return Ok(());
    }
    let mut next = Some(text);
    let mut outcome = Ok(());
    while let Some(m) = next.take() {
        if let Err(e) = run_turn(&engine, &m, date.as_deref(), &tx).await {
            engine.set_error(format!("turn failed: {e}"));
            emit(&tx, TurnEvent::Error(e.to_string()));
            outcome = Err(e);
            break;
        }
        next = engine.inbox.lock().expect("inbox lock poisoned").pop_front();
    }
    engine.turn_running.store(false, Ordering::SeqCst);
    emit(&tx, TurnEvent::Done);
    outcome
}

async fn wait_for_summaries(engine: &Engine) {
    let mut rx = engine.subscribe();
    let deadline = tokio::time::Instant::now() + SUMMARY_WAIT;
    loop {
        {
            let m = engine.mem();
            if m.turn_may_proceed(m.log.len()) {
                return;
            }
        }
        if tokio::time::timeout_at(deadline, rx.changed()).await.is_err() {
            eprintln!("endchat: turn goes on with unsummarized messages after {SUMMARY_WAIT:?}");
            return;
        }
    }
}

fn write_prompt(engine: &Engine, text: &str) -> Result<PathBuf> {
    let path = engine.scratch.join(format!("turn-{}-{}.txt", std::process::id(), now_iso().replace([':', '-'], "")));
    std::fs::write(&path, text)?;
    Ok(path)
}

fn run_args(engine: &Engine, prompt_file: PathBuf, parts: bool, label: u64) -> RunArgs {
    let t = &engine.cfg.turn;
    RunArgs {
        agent: t.agent.clone(),
        model: t.model.clone(),
        effort: t.effort.clone(),
        profile: t.profile.clone(),
        no_web: t.no_web,
        tools: Some(t.tools.clone()),
        system_prompt_file: Some(engine.system_file.clone()),
        prompt_file,
        parts,
        timeout: t.timeout,
        task: (t.profile != "sealed").then(|| format!("endchat-turn-{label}")),
    }
}

fn turn_aop(engine: &Engine) -> crate::aop::Aop {
    let t = &engine.cfg.turn;
    let cwd = if t.profile == "sealed" { engine.dir.clone() } else { t.workdir.clone().unwrap_or_else(|| engine.dir.clone()) };
    crate::aop::Aop { bin: engine.cfg.aop_bin.clone(), cwd }
}

fn log(engine: &Engine, tx: &UnboundedSender<TurnEvent>, kind: Kind, text: &str, run: &str) -> Result<()> {
    engine.append(kind, text, Some(run))?;
    emit(
        tx,
        match kind {
            Kind::Agent => TurnEvent::Agent(text.to_owned()),
            Kind::Tool => TurnEvent::Tool(text.to_owned()),
            _ => TurnEvent::Echo(text.to_owned()),
        },
    );
    Ok(())
}

/// Log what a run did, in order. Returns the calls the model made as text, taken from its final message.
fn log_run(engine: &Engine, tx: &UnboundedSender<TurnEvent>, aop: &crate::aop::Aop, r: &AopResult) -> Result<Vec<ToolCall>> {
    let events = std::fs::read_to_string(aop.events_path(&r.run_id)).map(|s| parse_claude(&s)).unwrap_or_default();
    if events.is_empty() {
        eprintln!("endchat: no readable events for run {}; logging its final message only", r.run_id);
        let text = strip_calls(r.reply());
        if !text.is_empty() {
            log(engine, tx, Kind::Agent, &text, &r.run_id)?;
        }
    }
    for e in events {
        match e {
            Event::Text(t) => {
                let text = strip_calls(&t);
                if !text.is_empty() {
                    log(engine, tx, Kind::Agent, &text, &r.run_id)?;
                }
            }
            Event::ToolCall { name, input } => log(engine, tx, Kind::Tool, &format!("{name} {input}"), &r.run_id)?,
            Event::ToolResult(t) => log(engine, tx, Kind::Echo, &t, &r.run_id)?,
        }
    }
    Ok(parse_calls(r.reply()))
}

fn answer(engine: &Engine, call: &ToolCall) -> String {
    let mut m = engine.mem();
    let r = match call {
        ToolCall::Zoom { id, n, offset } => m.zoom(*id, *n, *offset),
        ToolCall::Date { id } => m.date(*id),
    };
    r.unwrap_or_else(|e| format!("error: {e}"))
}

async fn run_turn(engine: &Arc<Engine>, text: &str, date: Option<&str>, tx: &UnboundedSender<TurnEvent>) -> Result<()> {
    wait_for_summaries(engine).await;
    let tail = format!("<state>\ndate: {}\n</state>\n{text}", date.map_or_else(now_iso, str::to_owned));
    let (prompt, index) = {
        let mut m = engine.mem();
        (m.chat_prompt(&tail)?, m.log.len())
    };
    engine.append(Kind::User, text, None)?;
    let aop = turn_aop(engine);
    let cleanup = engine.cfg.turn.profile == "sealed";
    let mut runs: Vec<String> = Vec::new();
    let parts = engine.cfg.turn.prompt_parts && prompt.has_mark();
    let file = write_prompt(engine, &if parts { prompt.parts_json() } else { prompt.text() })?;
    let mut result = aop.run(&run_args(engine, file.clone(), parts, index)).await;
    let _ = std::fs::remove_file(&file);
    let mut outcome = Ok(());
    for round in 0..=engine.cfg.turn.max_rounds {
        let r = match result {
            Ok(r) => r,
            Err(e) => {
                outcome = Err(e);
                break;
            }
        };
        runs.push(r.run_id.clone());
        engine.record_calls("turn", &[Call { run_id: r.run_id.clone(), model: r.model.clone(), usage: r.usage, ok: r.succeeded }]);
        let calls = match log_run(engine, tx, &aop, &r) {
            Ok(c) => c,
            Err(e) => {
                outcome = Err(e);
                break;
            }
        };
        if !r.succeeded {
            outcome = msg(r.failure());
            break;
        }
        let broken = calls.is_empty() && has_broken_call(r.reply());
        if calls.is_empty() && !broken {
            break;
        }
        if round == engine.cfg.turn.max_rounds {
            outcome = msg(format!("the turn made tool calls for {round} rounds and was stopped"));
            break;
        }
        let mut next = String::new();
        if broken {
            next += BROKEN_CALL;
        }
        for c in &calls {
            log(engine, tx, Kind::Tool, &c.log_text(), &r.run_id)?;
            let out = answer(engine, c);
            log(engine, tx, Kind::Echo, &out, &r.run_id)?;
            next += &format!("<result call=\"{}\">\n{}\n</result>\n", c.label(), crate::log::clip(&out));
        }
        let queued: Vec<String> = engine.inbox.lock().expect("inbox lock poisoned").drain(..).collect();
        for q in queued {
            engine.append(Kind::User, &q, None)?;
            next += &format!("The user sent this while you worked:\n{q}\n");
        }
        let file = write_prompt(engine, &next)?;
        result = aop.resume(&r.run_id, &file, engine.cfg.turn.timeout).await;
        let _ = std::fs::remove_file(&file);
    }
    if cleanup {
        for id in &runs {
            aop.cleanup(id).await;
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Config;
    use crate::testkit::FakeAop;
    use serde_json::json;
    use tokio::sync::mpsc::unbounded_channel;

    #[test]
    fn calls_are_whole_lines() {
        let text = "Let me look.\nzoom(40, 8)\n`date(7)`\nzoom(7, 1, 25000)\nSee zoom(1, 1) for details.\nzoom(x, 2)\nzoom(1)";
        assert_eq!(
            parse_calls(text),
            [ToolCall::Zoom { id: 40, n: 8, offset: 0 }, ToolCall::Date { id: 7 }, ToolCall::Zoom { id: 7, n: 1, offset: 25_000 }]
        );
        assert_eq!(strip_calls(text), "Let me look.\nSee zoom(1, 1) for details.\nzoom(x, 2)\nzoom(1)");
        assert_eq!(strip_calls("zoom(1, 1)"), "");
        let xml = "Looking.\n<invoke name=\"zoom\">\n<parameter name=\"id\">5</parameter>\n<parameter name=\"n\">2</parameter>\n</invoke>\n<invoke name=\"date\"><parameter name=\"id\">9</parameter></invoke>";
        assert_eq!(parse_calls(xml), [ToolCall::Zoom { id: 5, n: 2, offset: 0 }, ToolCall::Date { id: 9 }]);
        assert_eq!(strip_calls(xml), "Looking.");
        assert!(!has_broken_call(xml));
        let junk = "<invoke name=\"date\">\n<parameter name=\"id\">1</parameter>\n</invoke>\n\n<parameter name=\"id\">0</parameter>\n</invoke>";
        assert_eq!(parse_calls(junk), [ToolCall::Date { id: 1 }]);
        assert!(has_broken_call(junk));
        assert_eq!(strip_calls(junk), "");
        assert_eq!(ToolCall::Zoom { id: 40, n: 8, offset: 0 }.log_text(), r#"zoom {"id":40,"n":8}"#);
    }

    fn engine(fake: &FakeAop, dir: &std::path::Path) -> Arc<Engine> {
        let mut cfg = Config::default();
        cfg.aop_bin = fake.aop().bin;
        Engine::open(dir, cfg).unwrap()
    }

    async fn collect(mut rx: tokio::sync::mpsc::UnboundedReceiver<TurnEvent>) -> Vec<TurnEvent> {
        let mut v = Vec::new();
        while let Some(e) = rx.recv().await {
            let done = e == TurnEvent::Done;
            v.push(e);
            if done {
                break;
            }
        }
        v
    }

    #[tokio::test]
    async fn a_turn_logs_the_exchange_zooms_and_continues_the_same_run() {
        let fake = FakeAop::new(json!({"turn_replies": ["Checking.\nzoom(0, 2)", "The first message said hello."]}));
        let d = tempfile::tempdir().unwrap();
        let e = engine(&fake, d.path());
        e.mem().append(Kind::User, "hello", None).unwrap();
        e.mem().append(Kind::Agent, "hi there", None).unwrap();
        let (tx, rx) = unbounded_channel();
        say(e.clone(), "what did I say first?".into(), None, tx).await.unwrap();
        let events = collect(rx).await;
        assert_eq!(
            events,
            [
                TurnEvent::Agent("Checking.".into()),
                TurnEvent::Tool(r#"zoom {"id":0,"n":2}"#.into()),
                TurnEvent::Echo("0+1|user: hello\n1+1|agent: hi there\n".into()),
                TurnEvent::Agent("The first message said hello.".into()),
                TurnEvent::Done,
            ]
        );
        let mut m = e.mem();
        let kinds: Vec<Kind> = (0..m.log.len()).map(|i| m.log.get(i).unwrap().kind).collect();
        assert_eq!(kinds, [Kind::User, Kind::Agent, Kind::User, Kind::Agent, Kind::Tool, Kind::Echo, Kind::Agent]);
        drop(m);
        let calls = fake.calls();
        assert_eq!(calls[0]["cmd"], "run");
        let prompt = calls[0]["prompt"].as_str().unwrap();
        assert!(prompt.starts_with("<chat>\n0+1|user: hello\n1+1|agent: hi there\n</chat>\n<state>\ndate: 20"), "{prompt}");
        assert!(prompt.ends_with("</state>\nwhat did I say first?"));
        assert!(!prompt.contains("what did I say first?\n</chat>"), "the new message comes after the view");
        assert!(calls[0]["system"].as_str().unwrap().starts_with("You are Endchat"));
        assert_eq!((calls[1]["cmd"].as_str(), calls[1]["from"].as_str()), (Some("resume"), Some("fake-1")));
        assert!(calls[1]["prompt"].as_str().unwrap().starts_with("<result call=\"zoom(0, 2)\">\n0+1|user: hello"));
        assert_eq!(e.ledger().iter().filter(|l| l.kind == "turn").count(), 2);
        assert!(calls.iter().filter(|c| c["cmd"] == "cleanup").count() >= 2);
    }

    #[tokio::test]
    async fn messages_sent_during_a_turn_are_logged_and_delivered_with_the_next_result() {
        let fake = FakeAop::new(json!({"turn_replies": ["date(0)", "Noted.", "Second turn."]}));
        let d = tempfile::tempdir().unwrap();
        let e = engine(&fake, d.path());
        e.mem().append(Kind::User, "start", None).unwrap();
        e.turn_running.store(true, Ordering::SeqCst); // a turn is in progress
        let (tx, rx) = unbounded_channel();
        say(e.clone(), "wait, also this".into(), None, tx).await.unwrap();
        assert_eq!(collect(rx).await, [TurnEvent::Queued, TurnEvent::Done]);
        e.turn_running.store(false, Ordering::SeqCst);
        let (tx, rx) = unbounded_channel();
        say(e.clone(), "go".into(), None, tx).await.unwrap();
        let events = collect(rx).await;
        // The queued message arrives with the date result, and is logged as a user message.
        let calls = fake.calls();
        assert!(calls[1]["prompt"].as_str().unwrap().contains("The user sent this while you worked:\nwait, also this"));
        let mut m = e.mem();
        let texts: Vec<String> = (0..m.log.len()).map(|i| m.log.get(i).unwrap().text).collect();
        assert!(texts.contains(&"wait, also this".to_owned()));
        assert!(events.contains(&TurnEvent::Agent("Noted.".into())));
    }

    #[tokio::test]
    async fn a_queued_message_starts_the_next_turn_when_no_more_tools_are_called() {
        let fake = FakeAop::new(json!({"turn_replies": ["First answer.", "Second answer."]}));
        let d = tempfile::tempdir().unwrap();
        let e = engine(&fake, d.path());
        e.inbox.lock().unwrap().push_back("and another thing".into());
        let (tx, rx) = unbounded_channel();
        say(e.clone(), "one".into(), None, tx).await.unwrap();
        let events = collect(rx).await;
        assert_eq!(events.iter().filter(|x| matches!(x, TurnEvent::Agent(_))).count(), 2);
        let runs: Vec<_> = fake.calls().into_iter().filter(|c| c["cmd"] == "run").collect();
        assert_eq!(runs.len(), 2, "each turn is a fresh run");
        assert!(runs[1]["prompt"].as_str().unwrap().contains("</state>\nand another thing"));
        assert!(runs[1]["prompt"].as_str().unwrap().contains("agent: First answer."), "the second turn's view holds the first turn");
    }

    #[tokio::test]
    async fn a_failed_run_is_reported_and_the_chat_stays_usable() {
        let fake = FakeAop::new(json!({"fail_first": 1, "turn_replies": ["fine"]}));
        let d = tempfile::tempdir().unwrap();
        let e = engine(&fake, d.path());
        let (tx, rx) = unbounded_channel();
        assert!(say(e.clone(), "hi".into(), None, tx).await.is_err());
        let events = collect(rx).await;
        assert!(matches!(events[0], TurnEvent::Error(_)));
        assert!(!e.turn_running.load(Ordering::SeqCst));
        let (tx, rx) = unbounded_channel();
        say(e.clone(), "again".into(), None, tx).await.unwrap();
        assert!(collect(rx).await.contains(&TurnEvent::Agent("fine".into())));
    }

    #[tokio::test]
    async fn a_turn_waits_for_earlier_messages_to_be_summarized() {
        let fake = FakeAop::new(json!({"compact": "ok", "turn_replies": ["done"]}));
        let d = tempfile::tempdir().unwrap();
        let e = engine(&fake, d.path());
        e.mem().append(Kind::User, &"long message ".repeat(100), None).unwrap();
        let pump = tokio::spawn(e.clone().pump());
        e.changed();
        let (tx, rx) = unbounded_channel();
        say(e.clone(), "now?".into(), None, tx).await.unwrap();
        assert!(collect(rx).await.contains(&TurnEvent::Agent("done".into())));
        e.stop();
        pump.await.unwrap();
        let turn_prompt = fake.calls().into_iter().find(|c| c["cmd"] == "run" && !c["prompt"].as_str().unwrap().contains("Compaction:")).unwrap();
        assert!(!turn_prompt["prompt"].as_str().unwrap().contains("not summarized yet"));
    }

    #[tokio::test]
    async fn messages_logged_during_a_turn_wake_the_workers_so_the_next_turn_can_go_on() {
        let fake = FakeAop::new(json!({"compact": "ok", "turn_replies": ["zoom(0, 1)", "Read it.", "Second answer."]}));
        let d = tempfile::tempdir().unwrap();
        let e = engine(&fake, d.path());
        e.mem().append(Kind::User, &"a long first message ".repeat(60), None).unwrap();
        let pump = tokio::spawn(e.clone().pump());
        let both = async {
            // The first turn's tool result is long, so it needs a summary before the second turn.
            for text in ["read the first message", "and now?"] {
                let (tx, rx) = unbounded_channel();
                say(e.clone(), text.into(), None, tx).await.unwrap();
                collect(rx).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(30), both).await.expect("the second turn never started");
        e.stop();
        pump.await.unwrap();
    }
}
