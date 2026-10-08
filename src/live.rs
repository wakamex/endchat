//! Live checks against the real `aop` and a real model, on synthetic data. Ignored by default:
//!
//!   ENDCHAT_LIVE_MESSAGES=300 cargo test live -- --ignored --nocapture
//!
//! The chat is generated, labeled synthetic, and holds a few facts at known places; the recall
//! questions then need `zoom` to reach them.

use crate::engine::Engine;
use crate::log::Kind;
use crate::profile::Config;
use crate::turn::{TurnEvent, say};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::unbounded_channel;

struct Rng(u64);

impl Rng {
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        items[(self.0 >> 33) as usize % items.len()]
    }
}

const SERVICES: &[&str] = &["billing", "search", "ingest", "gateway", "notifier", "scheduler", "exporter"];
const FILES: &[&str] = &["config/routes.toml", "src/worker.rs", "deploy/values.yaml", "scripts/migrate.sh", "src/cache.rs", "docs/runbook.md"];
const ERRORS: &[&str] = &["a timeout after 30 seconds", "a missing environment variable", "a stale lock file", "an unexpected 502 from the load balancer", "a schema mismatch in the staging table"];
const FIXES: &[&str] = &["raising the retry limit to 5", "regenerating the lock file", "pinning the base image to an older tag", "restarting the worker pool", "moving the cron job to 03:15"];

/// A synthetic message long enough to need a model summary.
fn filler(rng: &mut Rng, n: usize) -> (Kind, String) {
    let (svc, file, err, fix) = (rng.pick(SERVICES), rng.pick(FILES), rng.pick(ERRORS), rng.pick(FIXES));
    let pad = "The logs show the same sequence on every attempt: the request is accepted, queued, retried, and then dropped without a trace in the audit table. ".repeat(3);
    if n % 2 == 0 {
        (Kind::User, format!("Synthetic message {n}. The {svc} service keeps failing in {file} with {err}. {pad}Can you look into it and tell me what you would change first? Please keep a note of anything that affects the release on Friday."))
    } else {
        (Kind::Agent, format!("Synthetic message {n}. I read {file} for the {svc} service. The failure comes from {err}, and the smallest safe change is {fix}. {pad}I did not touch the release branch. Next I would rerun the {svc} job and compare the audit table before and after."))
    }
}

/// (message index, text, question, expected answer)
const FACTS: &[(usize, &str, &str, &str)] = &[
    (6, "By the way, my cat is named Biscuit and she only eats the salmon flavor.", "What is my cat's name?", "Biscuit"),
    (41, "Decision: the staging database password rotates every Thursday, and Priya owns the rotation.", "Who owns the staging database password rotation?", "Priya"),
    (96, "I booked the ferry called Osprey Dawn for the 14th, leaving from pier 9.", "What is the name of the ferry I booked?", "Osprey Dawn"),
    (151, "Correction to what I said before: the release freeze starts on the 22nd, not the 20th.", "On what date does the release freeze start?", "22"),
    (221, "Our internal ticket prefix is KRILL, so tickets look like KRILL-1042.", "What is our internal ticket prefix?", "KRILL"),
    (286, "The warehouse door code changed to 7731 yesterday.", "What is the warehouse door code now?", "7731"),
];

#[tokio::test]
#[ignore = "calls a real model through aop"]
async fn live_recall() {
    let total: usize = std::env::var("ENDCHAT_LIVE_MESSAGES").ok().and_then(|v| v.parse().ok()).unwrap_or(300);
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(dir.path(), Config::default()).unwrap();
    let pump = tokio::spawn(engine.clone().pump());
    let started = Instant::now();
    let mut rng = Rng(7);
    let facts: Vec<_> = FACTS.iter().filter(|f| f.0 < total).collect();
    for n in 0..total {
        let (kind, text) = match facts.iter().find(|f| f.0 == n) {
            Some(f) => (Kind::User, format!("Synthetic message {n}. {} {}", f.1, "Everything else about the services stays as discussed. ".repeat(12))),
            None => filler(&mut rng, n),
        };
        engine.mem().append(kind, &text, None).unwrap();
        engine.changed();
        if n % 10 == 9 {
            let mut rx = engine.subscribe();
            let wait = async {
                loop {
                    {
                        let m = engine.mem();
                        if m.sched.unbuilt_before(m.log.len()) < 4 {
                            return;
                        }
                    }
                    let _ = rx.changed().await;
                }
            };
            tokio::time::timeout(Duration::from_secs(900), wait).await.expect("compactions stalled");
        }
    }
    assert!(engine.wait_idle(Duration::from_secs(1800)).await, "{}", engine.status().render());
    println!("summarized {total} messages in {:?}", started.elapsed());
    println!("{}", engine.status().render());
    let ledger = engine.ledger();
    let usage = ledger.iter().filter_map(|l| l.usage).fold((0, 0, 0), |a, u| (a.0 + u.input_tokens, a.1 + u.cached_input_tokens, a.2 + u.output_tokens));
    println!("{} compaction calls: {} input tokens, {} cached ({:.1}%), {} output tokens", ledger.len(), usage.0, usage.1, 100.0 * usage.1 as f64 / usage.0.max(1) as f64, usage.2);
    assert!(engine.mem().check().unwrap().is_empty());

    let mut right = 0;
    for f in &facts {
        let (tx, mut rx) = unbounded_channel();
        say(engine.clone(), f.2.to_owned(), None, tx).await.unwrap();
        let mut reply = String::new();
        let mut zooms = 0;
        while let Some(e) = rx.recv().await {
            match e {
                TurnEvent::Agent(t) => reply += &t,
                TurnEvent::Tool(_) => zooms += 1,
                TurnEvent::Done => break,
                _ => {}
            }
        }
        let ok = reply.contains(f.3);
        right += usize::from(ok);
        println!("recall of message {}: {} ({zooms} tool calls) {:?}", f.0, if ok { "found" } else { "MISSED" }, reply.chars().take(120).collect::<String>());
    }
    engine.stop();
    pump.await.unwrap();
    println!("turn cache read: {:?}", engine.cache_ratio("turn"));
    assert_eq!(right, facts.len(), "a fact stated early was not recovered");
}
