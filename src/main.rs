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
mod sched;
mod server;
mod sim;
mod store;
#[cfg(test)]
mod testkit;
mod tree;
mod turn;
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
    /// Send a message and print the reply
    Say {
        profile: String,
        text: Vec<String>,
        /// Also print tool calls and their results to stderr
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
        Command::Say { profile, text, verbose } => {
            let text = text.join(" ");
            if text.trim().is_empty() {
                return Err(Error::Msg("nothing to say".into()));
            }
            let dir = Profile::locate(&profile)?;
            let mut failed = false;
            let ran = server::ask(&dir, &json!({"cmd": "say", "text": text}), |v| {
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
            })
            .await?;
            if !ran {
                return Err(Error::Msg(format!("no server for {profile}: run `endchat serve {profile}`")));
            }
            return Ok(if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS });
        }
        Command::Status { profile } => return Ok(print_reply(query(&profile, json!({"cmd": "status"})).await?)),
        Command::Zoom { profile, id, n, offset } => return Ok(print_reply(query(&profile, json!({"cmd": "zoom", "id": id, "n": n, "offset": offset})).await?)),
        Command::Check { profile } => return Ok(print_reply(query(&profile, json!({"cmd": "check"})).await?)),
        Command::Sim { messages, interval_secs, latency_secs, every_message_lines, no_prime, compare_gist } => {
            if compare_gist {
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
