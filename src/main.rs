//! endchat: a chat that never ends, with a summary tree for memory and AOP for every model call.

mod aop;
mod compact;
mod engine;
mod error;
mod events;
#[cfg(test)]
mod live;
mod log;
mod memory;
mod profile;
mod prompts;
mod raw;
mod sched;
mod search;
mod server;
mod sim;
mod store;
#[cfg(test)]
mod testkit;
mod tree;
mod turn;
mod units;
mod util;
mod view;

use clap::{Parser, Subcommand};
use engine::Engine;
use error::{Error, Result};
use profile::Profile;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(version, about = "A chat that never ends: every message logged, memory kept as a tree of summaries, every model call made through AOP")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Own a profile: hold its lock, run the compaction workers, and serve the control socket
    Serve {
        /// Profile name under ~/.endchat, or a directory path
        profile: String,
    },
    /// Load a history file into a profile and wait until it is fully summarized
    Import {
        profile: String,
        /// JSON array of {"kind": "user"|"agent"|"note", "text": ..., "date": "2026-10-08T12:00:00Z"}
        file: PathBuf,
        /// Give up waiting for summaries after this many seconds
        #[arg(long, default_value_t = 21_600)]
        timeout_secs: u64,
        /// Append this many messages at a time and wait for their summaries before the next batch,
        /// as a live chat would; 0 appends everything at once
        #[arg(long, default_value_t = 25)]
        pace: usize,
        /// With turn.raw, into an empty profile: a summary of the chat before this history, which
        /// raw prompts then start with until their first cut
        #[arg(long)]
        summary: Option<PathBuf>,
    },
    /// Send a message and print the reply (runs the turn itself when no server is running)
    Say {
        profile: String,
        text: Vec<String>,
        /// The date the model is told it is, instead of now
        #[arg(long)]
        date: Option<String>,
        /// Also print tool calls and their results to stderr
        #[arg(short, long)]
        verbose: bool,
    },
    /// Chat interactively: each line is a message, and replies print as they arrive
    /// (talks to the running server, or owns the profile itself when none runs)
    Chat {
        profile: String,
        /// Also print tool calls and their results
        #[arg(short, long)]
        verbose: bool,
    },
    /// Message count, view size, unsummarized count, running calls, cache ratios, last error
    Status { profile: String },
    /// Print what the model would see for zoom(ID, N)
    Zoom {
        profile: String,
        id: u64,
        n: u64,
        /// Character offset into a long message
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    /// Verify the log, tree, and views against each other; exit nonzero on a mismatch
    Check { profile: String },
    /// Print the messages of a profile's log that match a regular expression
    Search {
        profile: String,
        pattern: String,
        /// Only messages of this kind (user, agent, tool, echo, work, note); repeat for more
        #[arg(short, long)]
        kind: Vec<String>,
        /// Only the newest N matches
        #[arg(short, long)]
        newest: Option<usize>,
        /// Also print N messages before and after each match
        #[arg(short = 'C', long, default_value_t = 0)]
        context: u64,
        /// Ignore case
        #[arg(short, long)]
        ignore_case: bool,
        /// Characters shown of each message, around its first match
        #[arg(long, default_value_t = 2000)]
        chars: usize,
    },
    /// Replay a synthetic chat against a model of the prompt cache
    Sim {
        #[arg(long, default_value_t = 3000)]
        messages: u64,
        /// Seconds between messages
        #[arg(long, default_value_t = 30)]
        interval_secs: u64,
        /// Seconds a stub compaction takes
        #[arg(long, default_value_t = 4)]
        latency_secs: u64,
        /// Merge the chat view to this many lines after every message instead of batching
        #[arg(long)]
        every_message_lines: Option<usize>,
        /// Send every call of a burst at once instead of the first alone
        #[arg(long)]
        no_prime: bool,
        /// Compare merge timing on the view alone, as the gist does, over --messages messages
        #[arg(long)]
        compare_gist: bool,
        /// Replay an `import` history file instead of a synthetic chat (uses --pace)
        #[arg(long)]
        history: Option<PathBuf>,
        /// With --history: replay at the messages' own timestamps with a 1-hour cache lifetime
        #[arg(long)]
        real_time: bool,
        /// With --history: messages appended at a time before waiting for summaries; 0 is all at once
        #[arg(long, default_value_t = 0)]
        pace: usize,
    },
}

fn open_engine(profile: &str) -> Result<(PathBuf, std::sync::Arc<Engine>)> {
    let dir = Profile::locate(profile)?;
    let p = Profile::open(&dir)?;
    let engine = Engine::open(&dir, p.config.clone())?;
    // The lock lives as long as the process: leak the guard rather than thread it through.
    std::mem::forget(p);
    Ok((dir, engine))
}

/// Ask the running server, or open the profile directly when none is running.
async fn query(profile: &str, req: Value) -> Result<Vec<Value>> {
    let dir = Profile::locate(profile)?;
    let mut out = Vec::new();
    if server::ask(&dir, &req, |v| out.push(v)).await? {
        return Ok(out);
    }
    let (_, engine) = open_engine(profile)?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let request: server::Request = serde_json::from_value(req).map_err(Error::from)?;
    server::handle(&engine, request, tx).await;
    while let Ok(v) = rx.try_recv() {
        out.push(v);
    }
    Ok(out)
}

async fn run(cli: Cli) -> Result<ExitCode> {
    match cli.command {
        Command::Serve { profile } => {
            let (_, engine) = open_engine(&profile)?;
            server::serve(engine).await?;
        }
        Command::Import { profile, file, timeout_secs, pace, summary } => return import(&profile, &file, timeout_secs, pace, summary.as_deref()).await,
        Command::Say { profile, text, date, verbose } => {
            let text = text.join(" ");
            if text.trim().is_empty() {
                return Err(Error::Msg("nothing to say".into()));
            }
            let dir = Profile::locate(&profile)?;
            let mut failed = false;
            let mut on_line = |v: Value| {
                let t = v["text"].as_str().unwrap_or("");
                match v["event"].as_str() {
                    Some("agent") => println!("{t}"),
                    Some("tool") if verbose => eprintln!("> {t}"),
                    Some("echo") if verbose => eprintln!("< {t}"),
                    Some("queued") => eprintln!("queued: a turn is running; the message reaches it between tool calls"),
                    Some("error") => {
                        eprintln!("error: {t}");
                        failed = true;
                    }
                    _ => {}
                }
            };
            let req = json!({"cmd": "say", "text": text, "date": date});
            if !server::ask(&dir, &req, &mut on_line).await? {
                // No server: run the turn here, with compaction workers for what the turn logs.
                let (_, engine) = open_engine(&profile)?;
                let pump = tokio::spawn(engine.clone().pump());
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                let request: server::Request = serde_json::from_value(req).map_err(Error::from)?;
                server::handle(&engine, request, tx).await;
                while let Ok(v) = rx.try_recv() {
                    on_line(v);
                }
                engine.stop();
                let _ = pump.await;
                engine.mem().save_views()?;
            }
            return Ok(if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS });
        }
        Command::Chat { profile, verbose } => return chat(&profile, verbose).await,
        Command::Status { profile } => return Ok(print_reply(query(&profile, json!({"cmd": "status"})).await?)),
        Command::Zoom { profile, id, n, offset } => return Ok(print_reply(query(&profile, json!({"cmd": "zoom", "id": id, "n": n, "offset": offset})).await?)),
        Command::Check { profile } => return Ok(print_reply(query(&profile, json!({"cmd": "check"})).await?)),
        Command::Search { profile, pattern, kind, newest, context, ignore_case, chars } => {
            let pattern = regex::RegexBuilder::new(&pattern).case_insensitive(ignore_case).build().map_err(|e| Error::Msg(e.to_string()))?;
            print!("{}", search::run(&Profile::locate(&profile)?, &search::Query { pattern, kinds: kind, newest, context, chars })?);
        }
        Command::Sim { messages, interval_secs, latency_secs, every_message_lines, no_prime, compare_gist, history, real_time, pace } => {
            if let Some(file) = history {
                let items: Vec<Imported> = serde_json::from_str(&std::fs::read_to_string(file)?).map_err(Error::from)?;
                let dir = tempfile::tempdir_in(std::env::temp_dir())?;
                let mut cfg = sim::SimConfig { latency_ms: latency_secs * 1000, prime_first: !no_prime, real_tails: true, ..Default::default() };
                if real_time {
                    cfg.ttl_ms = 3_600_000;
                    let history: Vec<_> = items.into_iter().map(|m| (m.kind, m.text, util::unix_from_iso(&m.date).unwrap_or(0))).collect();
                    println!("{:#?}", sim::run_real_time(dir.path(), &history, &cfg)?);
                } else {
                    let history: Vec<_> = items.into_iter().map(|m| (m.kind, m.text)).collect();
                    println!("{:#?}", sim::run_import(dir.path(), &history, pace, &cfg)?);
                }
            } else if compare_gist {
                let (every, batched, lines) = sim::compare_gist(messages, every_message_lines.unwrap_or(192));
                println!("view lines rewritten per message, over the second half of {messages} messages");
                println!("merged to {} lines after every message: {every:.1}", every_message_lines.unwrap_or(192));
                println!("batched between 64 KB and 128 KB ({lines:.0} lines on average): {batched:.2}");
            } else {
                let dir = tempfile::tempdir_in(std::env::temp_dir())?;
                let cfg = sim::SimConfig {
                    messages,
                    interval_ms: interval_secs * 1000,
                    latency_ms: latency_secs * 1000,
                    every_message_lines,
                    prime_first: !no_prime,
                    ..Default::default()
                };
                println!("{:#?}", sim::run(dir.path(), &cfg)?);
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

#[derive(serde::Deserialize)]
struct Imported {
    kind: log::Kind,
    text: String,
    date: String,
}

/// Append a history file, let the workers summarize it, and report what that cost.
async fn import(profile: &str, file: &std::path::Path, timeout_secs: u64, pace: usize, summary: Option<&std::path::Path>) -> Result<ExitCode> {
    let items: Vec<Imported> = serde_json::from_str(&std::fs::read_to_string(file)?).map_err(Error::from)?;
    let (dir, engine) = open_engine(profile)?;
    if let Some(path) = summary {
        if !engine.cfg.turn.raw || engine.mem().log.len() > 0 || dir.join("raw.json").exists() {
            return Err(Error::Msg("--summary needs turn.raw and an empty profile".into()));
        }
        raw::save_cut(&dir, 0, &std::fs::read_to_string(path)?)?;
    }
    let started = std::time::Instant::now();
    let before = engine.ledger().len();
    let existing = engine.mem().log.len();
    let mut seen = 0u64;
    let remaining: Vec<&Imported> = items
        .iter()
        .filter(|m| {
            let pages = if m.kind == log::Kind::Echo { 1 } else { log::paginate(&m.text).len() as u64 };
            let keep = seen >= existing;
            seen += pages;
            keep
        })
        .collect();
    let pump = tokio::spawn(engine.clone().pump());
    let limit = std::time::Duration::from_secs(timeout_secs);
    let mut done = true;
    for batch in remaining.chunks(if pace == 0 { remaining.len().max(1) } else { pace }) {
        for m in batch {
            engine.mem().append_dated(m.kind, &m.text, None, &m.date)?;
        }
        engine.changed();
        if pace > 0 {
            if !engine.wait_idle(limit).await {
                // A batch that does not finish (for example expired credentials) would make every
                // later batch wait out the limit as well.
                done = false;
                break;
            }
            engine.mem().settle_views()?;
        }
    }
    if done {
        done = engine.wait_idle(limit).await;
    }
    engine.stop();
    let _ = pump.await;
    engine.mem().settle_views()?;
    engine.mem().save_views()?;
    let calls: Vec<_> = engine.ledger().into_iter().skip(before).collect();
    let sum = |f: fn(&tree::Usage) -> u64| calls.iter().filter_map(|c| c.usage.as_ref()).map(f).sum::<u64>();
    let report = json!({
        "complete": done,
        "messages": engine.mem().log.len(),
        "calls": calls.len(),
        "failed_calls": calls.iter().filter(|c| !c.ok).count(),
        "input_tokens": sum(|u| u.input_tokens),
        "cached_input_tokens": sum(|u| u.cached_input_tokens),
        "output_tokens": sum(|u| u.output_tokens),
        "seconds": started.elapsed().as_secs(),
        "last_error": engine.status().last_error,
    });
    println!("{report}");
    Ok(if done { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

/// Print one event of a `say`; returns false for an error.
fn show(v: &Value, verbose: bool) -> bool {
    let t = v["text"].as_str().unwrap_or("");
    match v["event"].as_str() {
        Some("agent") => println!("{t}\n"),
        Some("tool") if verbose => eprintln!("> {t}"),
        Some("echo") if verbose => eprintln!("< {}", t.lines().take(20).collect::<Vec<_>>().join("\n< ")),
        Some("queued") => eprintln!("(queued: a turn is running; the message reaches it between tool calls)"),
        Some("error") => {
            eprintln!("error: {t}");
            return false;
        }
        _ => {}
    }
    true
}

/// A line-based chat with one profile. Through its server when one runs; otherwise this process
/// owns the profile, with its compaction workers, until the chat ends.
async fn chat(profile: &str, verbose: bool) -> Result<ExitCode> {
    let dir = Profile::locate(profile)?;
    let local = match server::ask(&dir, &json!({"cmd": "status"}), |_| {}).await? {
        true => None,
        false => {
            let (_, engine) = open_engine(profile)?;
            let pump = tokio::spawn(engine.clone().pump());
            Some((engine, pump))
        }
    };
    eprintln!("endchat chat: {} ({}). Each line is a message; Ctrl-D ends.", dir.display(), if local.is_some() { "no server running, so this process owns the profile" } else { "through its server" });
    let history = dir.join("chat-history.txt");
    let mut editor = rustyline::DefaultEditor::new().map_err(|e| Error::Msg(e.to_string()))?;
    let _ = editor.load_history(&history);
    loop {
        let line = match editor.readline("> ") {
            Ok(l) => l,
            Err(rustyline::error::ReadlineError::Interrupted) => continue,
            Err(_) => break,
        };
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        let _ = editor.add_history_entry(text);
        let req = json!({"cmd": "say", "text": text});
        match &local {
            None => {
                if !server::ask(&dir, &req, |v| {
                    show(&v, verbose);
                })
                .await?
                {
                    eprintln!("error: the server stopped; run endchat serve {profile} or chat again");
                    break;
                }
            }
            Some((engine, _)) => {
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                let request: server::Request = serde_json::from_value(req).map_err(Error::from)?;
                let turn = tokio::spawn({
                    let engine = engine.clone();
                    async move { server::handle(&engine, request, tx).await }
                });
                while let Some(v) = rx.recv().await {
                    show(&v, verbose);
                }
                let _ = turn.await;
            }
        }
    }
    let _ = editor.save_history(&history);
    if let Some((engine, pump)) = local {
        engine.stop();
        let _ = pump.await;
        engine.mem().save_views()?;
    }
    Ok(ExitCode::SUCCESS)
}

fn print_reply(lines: Vec<Value>) -> ExitCode {
    let mut ok = true;
    for v in &lines {
        print!("{}", v["text"].as_str().unwrap_or(""));
        ok &= v["ok"].as_bool().unwrap_or(true);
    }
    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("endchat: {e}");
            ExitCode::FAILURE
        }
    }
}
