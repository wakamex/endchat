//! The reasoning effort of a profile's turns. With `turn.effort_control` each turn's state block
//! names the effort the turn runs at and the command that changes it, so the model can match its
//! effort to the work: `endchat effort PROFILE LEVEL` sets a default that holds until changed, and
//! `--next` sets the next turn only. Kept in `effort.json`; `turn.effort` applies when neither is set.

use crate::error::{Result, msg};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

const FILE: &str = "effort.json";

/// The levels Claude accepts.
pub const CLAUDE_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Effort {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

pub fn load(dir: &Path) -> Effort {
    fs::read_to_string(dir.join(FILE)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save(dir: &Path, e: &Effort) -> Result<()> {
    let tmp = dir.join(format!("{FILE}.tmp"));
    fs::write(&tmp, serde_json::to_string(e)?)?;
    fs::rename(tmp, dir.join(FILE))?;
    Ok(())
}

/// The effort for the turn starting now; a next-turn effort is used up by it.
pub fn take(dir: &Path, configured: &str) -> Result<String> {
    let mut e = load(dir);
    let Some(next) = e.next.take() else {
        return Ok(e.default.unwrap_or_else(|| configured.to_owned()));
    };
    save(dir, &e)?;
    Ok(next)
}

/// Set the default, or the next turn's effort, checking the level when the agent is Claude.
pub fn set(dir: &Path, agent: &str, level: &str, next: bool) -> Result<()> {
    if agent == "claude" && !CLAUDE_LEVELS.contains(&level) {
        return msg(format!("effort must be one of: {}", CLAUDE_LEVELS.join(", ")));
    }
    let mut e = load(dir);
    *(if next { &mut e.next } else { &mut e.default }) = Some(level.to_owned());
    save(dir, &e)
}

pub fn describe(dir: &Path, configured: &str) -> String {
    let e = load(dir);
    let mut s = match &e.default {
        Some(d) => format!("effort: {d} (set; config {configured})"),
        None => format!("effort: {configured} (config)"),
    };
    if let Some(n) = &e.next {
        s += &format!("; next turn: {n}");
    }
    s + "\n"
}

/// The state-block line that tells the model its effort and how to change it.
pub fn state_line(dir: &Path, now: &str) -> String {
    format!(
        "effort: {now}. Set it with `endchat effort {} LEVEL` ({}), which holds until changed, or add --next for the next turn only.",
        dir.display(),
        CLAUDE_LEVELS.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_next_turn_effort_is_used_once_and_a_default_holds() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(take(d.path(), "medium").unwrap(), "medium");
        set(d.path(), "claude", "xhigh", false).unwrap();
        set(d.path(), "claude", "max", true).unwrap();
        assert_eq!(describe(d.path(), "medium"), "effort: xhigh (set; config medium); next turn: max\n");
        assert_eq!(take(d.path(), "medium").unwrap(), "max");
        assert_eq!(take(d.path(), "medium").unwrap(), "xhigh");
        assert!(set(d.path(), "claude", "huge", false).is_err());
    }
}
