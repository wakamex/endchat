//! The systemd units a turn's tools start. Each is polled until it stops, and its end is sent to the
//! chat as a message, which starts a turn. Watched names are kept in `units.txt` so a restarted
//! server goes on watching them.

use crate::engine::Engine;
use std::sync::Arc;
use std::time::Duration;

const POLL: Duration = Duration::from_secs(15);
const FILE: &str = "units.txt";

/// Unit names in `systemd-run` output (`Running as unit: NAME; invocation ID: ...`).
pub fn started(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("Running as unit: "))
        .filter_map(|r| r.split_whitespace().next())
        .map(|n| n.trim_end_matches(';').to_owned())
        .collect()
}

fn watched(engine: &Engine) -> Vec<String> {
    std::fs::read_to_string(engine.dir.join(FILE)).unwrap_or_default().lines().filter(|l| !l.is_empty()).map(str::to_owned).collect()
}

fn save(engine: &Engine, names: &[String]) {
    let _ = std::fs::write(engine.dir.join(FILE), names.iter().map(|n| format!("{n}\n")).collect::<String>());
}

/// The message saying how `name` ended, or None while it is still running.
async fn ended(name: &str) -> Option<String> {
    let out = tokio::process::Command::new("systemctl")
        .args(["--user", "show", name, "--property=LoadState,ActiveState,Result,ExecMainStatus"])
        .output()
        .await
        .ok()?;
    let props: std::collections::BTreeMap<&str, &str> = std::str::from_utf8(&out.stdout).ok()?.lines().filter_map(|l| l.split_once('=')).collect();
    let state = props.get("ActiveState").copied().unwrap_or("");
    if matches!(state, "active" | "activating" | "deactivating" | "reloading" | "refreshing") {
        return None;
    }
    Some(match props.get("LoadState").copied() {
        Some("not-found") => format!("[systemd] unit {name} has stopped and is no longer loaded, so its result is not known."),
        _ => format!(
            "[systemd] unit {name} has stopped: state {state}, result {}, exit status {}.",
            props.get("Result").copied().unwrap_or("unknown"),
            props.get("ExecMainStatus").copied().unwrap_or("unknown")
        ),
    })
}

/// Watch `name` until it stops, then say so in the chat.
pub fn watch(engine: Arc<Engine>, name: String) {
    let mut names = watched(&engine);
    if !names.contains(&name) {
        names.push(name.clone());
        save(&engine, &names);
    }
    tokio::spawn(async move {
        while !engine.stopped() {
            if let Some(text) = ended(&name).await {
                save(&engine, &watched(&engine).into_iter().filter(|n| *n != name).collect::<Vec<_>>());
                let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
                let _ = crate::turn::say(engine.clone(), text, None, tx).await;
                return;
            }
            tokio::time::sleep(POLL).await;
        }
    });
}

/// Go on watching the units a previous server was watching.
pub fn resume(engine: &Arc<Engine>) {
    for name in watched(engine) {
        watch(engine.clone(), name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_names_come_from_systemd_run_output() {
        let out = "Running as unit: replay-gist.service; invocation ID: 1f\nok\n  Running as unit: run-u42.service\n";
        assert_eq!(started(out), ["replay-gist.service", "run-u42.service"]);
    }
}
