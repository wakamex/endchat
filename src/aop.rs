//! The only way endchat calls a model: `aop run` and `aop resume` with `--json`.
//!
//! No provider credentials, HTTP clients, or session files are read here; the normalized result
//! is the whole interface.

use crate::error::{Error, Result};
use crate::tree::Usage;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

#[derive(Clone, Debug)]
pub struct Aop {
    pub bin: String,
    /// Directory the calls run in; AOP keeps its `.aop/runs` records beneath it.
    pub cwd: PathBuf,
}

#[derive(Clone, Debug, Default)]
pub struct RunArgs {
    pub agent: String,
    pub model: String,
    pub effort: String,
    pub profile: String,
    pub no_web: bool,
    /// Exact built-in tool set; `Some("")` means none.
    pub tools: Option<String>,
    pub system_prompt_file: Option<PathBuf>,
    /// The prompt: text, or a JSON list of blocks when `parts` is set.
    pub prompt_file: PathBuf,
    /// `prompt_file` holds `--prompt-parts` blocks, one of them marked for caching.
    pub parts: bool,
    pub timeout: u64,
    /// Task label, needed by profiles that make a worktree.
    pub task: Option<String>,
    /// `host` only: run the harness in this directory itself, with no worktree (`--workdir`).
    pub workdir: Option<PathBuf>,
    /// `host` only: the harness uses the real home, its own system prompt, settings and sessions (`--native-state`).
    pub native_state: bool,
}

/// The fields of AOP's normalized result that endchat uses.
#[derive(Clone, Debug, Deserialize)]
pub struct AopResult {
    pub run_id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub succeeded: bool,
    #[serde(default)]
    pub timed_out: bool,
    #[serde(default)]
    pub final_message: Option<String>,
    #[serde(default)]
    pub usage: Option<Usage>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub error: Option<serde_json::Value>,
}

impl AopResult {
    pub fn reply(&self) -> &str {
        self.final_message.as_deref().unwrap_or("")
    }

    pub fn failure(&self) -> String {
        let why = self.error.as_ref().map(|e| e.to_string()).unwrap_or_default();
        format!("run {} {}{}{}", self.run_id, self.status, if self.timed_out { " (timed out)" } else { "" }, if why.is_empty() { String::new() } else { format!(": {why}") })
    }
}

pub fn run_argv(a: &RunArgs) -> Vec<String> {
    let mut v: Vec<String> = ["run", "--agent", &a.agent, "--model", &a.model, "--effort", &a.effort, "--profile", &a.profile].map(String::from).into();
    if a.no_web {
        v.push("--no-web".into());
    }
    if let Some(t) = &a.tools {
        v.extend(["--tools".into(), t.clone()]);
    }
    if let Some(p) = &a.system_prompt_file {
        v.extend(["--system-prompt-file".into(), p.display().to_string()]);
    }
    if let Some(w) = &a.workdir {
        v.extend(["--workdir".into(), w.display().to_string()]);
    }
    if a.native_state {
        v.push("--native-state".into());
    }
    v.extend([if a.parts { "--prompt-parts" } else { "--prompt-file" }.into(), a.prompt_file.display().to_string(), "--timeout".into(), a.timeout.to_string(), "--json".into()]);
    if let Some(t) = &a.task {
        v.push(t.clone());
    }
    v
}

pub fn resume_argv(run_id: &str, prompt_file: &Path, timeout: u64) -> Vec<String> {
    ["resume", "--prompt-file", &prompt_file.display().to_string(), "--timeout", &timeout.to_string(), "--json", run_id].map(String::from).into()
}

pub fn parse_result(stdout: &str) -> Result<AopResult> {
    serde_json::from_str(stdout.trim()).map_err(|e| Error::Aop(format!("unreadable result ({e}): {}", stdout.chars().take(300).collect::<String>())))
}

impl Aop {
    async fn exec(&self, argv: &[String], timeout: u64) -> Result<AopResult> {
        let child = Command::new(&self.bin)
            .args(argv)
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| Error::Aop(format!("cannot start {}: {e}", self.bin)))?;
        let out = tokio::time::timeout(Duration::from_secs(timeout + 60), child.wait_with_output())
            .await
            .map_err(|_| Error::Aop(format!("no result {timeout}s after the deadline")))??;
        let stdout = String::from_utf8_lossy(&out.stdout);
        match parse_result(&stdout) {
            Ok(r) => Ok(r),
            Err(e) if out.status.success() => Err(e),
            Err(_) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                Err(Error::Aop(format!("exit {}: {}", out.status.code().unwrap_or(-1), stderr.trim().chars().rev().take(400).collect::<Vec<_>>().into_iter().rev().collect::<String>())))
            }
        }
    }

    pub async fn run(&self, args: &RunArgs) -> Result<AopResult> {
        self.exec(&run_argv(args), args.timeout).await
    }

    pub async fn resume(&self, run_id: &str, prompt_file: &Path, timeout: u64) -> Result<AopResult> {
        self.exec(&resume_argv(run_id, prompt_file, timeout), timeout).await
    }

    /// Discard a finished run's private runtime. Best effort: a failure only leaves disk in use.
    pub async fn cleanup(&self, run_id: &str) {
        let _ = Command::new(&self.bin).args(["cleanup", run_id]).current_dir(&self.cwd).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().await;
    }

    pub fn events_path(&self, run_id: &str) -> PathBuf {
        self.cwd.join(".aop").join("runs").join(run_id).join("events.jsonl")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_for_a_sealed_compaction() {
        let a = RunArgs {
            agent: "claude".into(),
            model: "haiku".into(),
            effort: "medium".into(),
            profile: "sealed".into(),
            no_web: true,
            tools: Some(String::new()),
            system_prompt_file: Some("/p/system.md".into()),
            prompt_file: "/p/task.txt".into(),
            parts: false,
            timeout: 120,
            task: None,
            workdir: None,
            native_state: false,
        };
        assert_eq!(
            run_argv(&a).join(" "),
            "run --agent claude --model haiku --effort medium --profile sealed --no-web --tools  --system-prompt-file /p/system.md --prompt-file /p/task.txt --timeout 120 --json"
        );
        assert_eq!(run_argv(&a)[11], "");
        let b = RunArgs { parts: true, ..a.clone() };
        assert!(run_argv(&b).join(" ").contains("--prompt-parts /p/task.txt --timeout"));
        assert!(!run_argv(&b).join(" ").contains("--prompt-file"));
        let host = RunArgs { profile: "host".into(), no_web: false, tools: None, system_prompt_file: None, workdir: Some("/code/x".into()), native_state: true, ..b.clone() };
        assert!(run_argv(&host).join(" ").ends_with("--profile host --workdir /code/x --native-state --prompt-parts /p/task.txt --timeout 120 --json"));
        assert_eq!(resume_argv("abc", Path::new("/p/r.txt"), 60).join(" "), "resume --prompt-file /p/r.txt --timeout 60 --json abc");
    }

    #[test]
    fn parses_a_real_result_shape() {
        let json = r#"{"accounting_status":"complete","final_message":"ok","run_id":"r1","status":"succeeded","succeeded":true,"timed_out":false,
            "usage":{"cached_input_tokens":0,"input_tokens":547,"output_tokens":4,"reasoning_output_tokens":0},"model":"claude-haiku-5-5","extra":1}"#;
        let r = parse_result(json).unwrap();
        assert_eq!((r.reply(), r.succeeded, r.usage.unwrap().input_tokens), ("ok", true, 547));
        assert!(parse_result("Traceback...").is_err());
    }
}
