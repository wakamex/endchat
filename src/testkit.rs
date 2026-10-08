//! Test support: a scripted stand-in for `aop` (tests/fake_aop.py) in a temporary directory.

use crate::aop::Aop;
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub struct FakeAop {
    pub dir: tempfile::TempDir,
    pub cwd: PathBuf,
}

impl FakeAop {
    pub fn new(scenario: Value) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let cwd = dir.path().join("cwd");
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(state.join("scenario.json"), scenario.to_string()).unwrap();
        let script = format!(
            "#!/bin/sh\nexport FAKE_AOP_DIR='{}'\nexec python3 '{}/tests/fake_aop.py' \"$@\"\n",
            state.display(),
            env!("CARGO_MANIFEST_DIR")
        );
        let bin = dir.path().join("aop");
        fs::write(&bin, script).unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir, cwd }
    }

    pub fn aop(&self) -> Aop {
        Aop { bin: self.dir.path().join("aop").display().to_string(), cwd: self.cwd.clone() }
    }

    pub fn set_scenario(&self, scenario: Value) {
        fs::write(self.dir.path().join("state/scenario.json"), scenario.to_string()).unwrap();
    }

    /// Every call the fake saw, in order.
    pub fn calls(&self) -> Vec<Value> {
        let text = fs::read_to_string(self.dir.path().join("state/calls.jsonl")).unwrap_or_default();
        text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

pub fn real_aop_installed() -> bool {
    std::process::Command::new("aop").arg("--version").output().is_ok_and(|o| o.status.success())
}
