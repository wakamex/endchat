//! A profile directory: its lock and its configuration.

use crate::error::{Error, Result, msg};
use rustix::fs::{FlockOperation, flock};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Compactor {
    pub agent: String,
    pub model: String,
    pub effort: String,
    /// Concurrent AOP calls.
    pub workers: usize,
    /// Largest reply accepted without a retry. 512 restores the gist's strict rule.
    pub accept_bytes: usize,
    /// Largest reply kept after the last retry when every attempt was over `accept_bytes`.
    pub hard_bytes: usize,
    pub timeout: u64,
    /// Send the view as blocks with one cache mark (`aop --prompt-parts`).
    pub prompt_parts: bool,
}

impl Default for Compactor {
    fn default() -> Self {
        Self {
            agent: "claude".into(),
            model: "haiku".into(),
            effort: "medium".into(),
            workers: 8,
            accept_bytes: 640,
            hard_bytes: 1024,
            timeout: 120,
            prompt_parts: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Turn {
    pub agent: String,
    pub model: String,
    pub effort: String,
    /// AOP profile: sealed, edit, review, or host.
    pub profile: String,
    /// Allows `host`; refused otherwise.
    pub allow_host: bool,
    /// Directory AOP runs in for non-sealed profiles, such as the repository `edit` is bound to.
    pub workdir: Option<PathBuf>,
    /// Built-in harness tools, comma separated. Empty means none.
    pub tools: String,
    pub no_web: bool,
    pub timeout: u64,
    /// Upper bound on tool rounds in one turn.
    pub max_rounds: usize,
    /// Send the view as blocks with one cache mark (`aop --prompt-parts`).
    pub prompt_parts: bool,
    /// Answer `zoom` and `date` calls. Off leaves the model with `commands` only.
    pub zoom: bool,
    /// Extra text tools: name to program. A reply line `name(ARG)` or `name ARG` runs
    /// `program PROFILE_DIR ARG` and returns its output.
    pub commands: BTreeMap<String, String>,
    /// Replaces the note sent with every message.
    pub tail_note: Option<String>,
    /// Send the chat view with every message. Off sends the message alone.
    pub view: bool,
}

impl Default for Turn {
    fn default() -> Self {
        Self {
            agent: "claude".into(),
            model: "sonnet".into(),
            effort: "medium".into(),
            profile: "sealed".into(),
            allow_host: false,
            workdir: None,
            tools: String::new(),
            no_web: true,
            timeout: 900,
            max_rounds: 40,
            prompt_parts: true,
            zoom: true,
            commands: BTreeMap::new(),
            tail_note: None,
            view: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Limits {
    pub view_high: usize,
    pub view_low: usize,
    pub compaction_high: usize,
    pub compaction_low: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { view_high: 128_000, view_low: 64_000, compaction_high: 32_000, compaction_low: 16_000 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub aop_bin: String,
    /// A file that replaces the built-in system prompt.
    pub system_prompt: Option<PathBuf>,
    pub compactor: Compactor,
    pub turn: Turn,
    pub limits: Limits,
}

impl Default for Config {
    fn default() -> Self {
        Self { aop_bin: "aop".into(), system_prompt: None, compactor: Compactor::default(), turn: Turn::default(), limits: Limits::default() }
    }
}

impl Config {
    pub fn load(dir: &Path) -> Result<Self> {
        let cfg: Config = match fs::read_to_string(dir.join("config.toml")) {
            Ok(s) => toml::from_str(&s).map_err(|e| Error::Msg(format!("config.toml: {e}")))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => return Err(e.into()),
        };
        if cfg.turn.profile == "host" && !cfg.turn.allow_host {
            return msg("turn.profile = \"host\" needs turn.allow_host = true");
        }
        if cfg.compactor.workers == 0 || cfg.limits.view_low >= cfg.limits.view_high || cfg.limits.compaction_low >= cfg.limits.compaction_high {
            return msg("config: workers must be positive and each low mark below its high mark");
        }
        Ok(cfg)
    }
}

/// An opened profile directory. Holds the exclusive lock until dropped.
pub struct Profile {
    pub config: Config,
    _lock: File,
}

impl Profile {
    /// The directory of a named profile under `~/.endchat`, or a path when the name contains `/`.
    pub fn locate(name: &str) -> Result<PathBuf> {
        if name.contains('/') {
            return Ok(PathBuf::from(name));
        }
        let home = std::env::var_os("HOME").ok_or_else(|| Error::Msg("HOME is not set".into()))?;
        Ok(PathBuf::from(home).join(".endchat").join(name))
    }

    pub fn open(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let lock = OpenOptions::new().create(true).read(true).write(true).truncate(false).open(dir.join("lock"))?;
        flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|_| Error::Locked)?;
        let config = Config::load(dir)?;
        Ok(Self { config, _lock: lock })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_open_is_locked_until_first_drops() {
        let d = tempfile::tempdir().unwrap();
        let first = Profile::open(d.path()).unwrap();
        assert!(matches!(Profile::open(d.path()), Err(Error::Locked)));
        drop(first);
        Profile::open(d.path()).unwrap();
    }

    #[test]
    fn config_overrides_and_host_guard() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("config.toml"), "[compactor]\nmodel = \"x\"\n[turn]\nprofile = \"host\"\n").unwrap();
        assert!(Config::load(d.path()).is_err());
        fs::write(d.path().join("config.toml"), "[compactor]\nmodel = \"x\"\n[turn]\nprofile = \"host\"\nallow_host = true\n").unwrap();
        let c = Config::load(d.path()).unwrap();
        assert_eq!((c.compactor.model.as_str(), c.compactor.workers), ("x", 8));
    }
}
