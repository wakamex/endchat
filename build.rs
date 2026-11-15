//! Record the commit the binary is built from, and whether tracked files differed from it, as
//! `ENDCHAT_COMMIT` for `endchat --version` and `endchat status`. Outside a Git checkout, such as a
//! build from a published package, it is `unknown`.

use std::process::Command;

fn git(args: &[&str]) -> Option<std::process::Output> {
    Command::new("git").args(args).output().ok().filter(|o| o.status.success())
}

fn main() {
    let commit = git(&["rev-parse", "--short=7", "HEAD"]).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|o| !o.stdout.is_empty());
    let build = match commit {
        Some(c) => format!("{c}{}", if dirty { " with uncommitted changes" } else { "" }),
        None => "unknown".to_owned(),
    };
    println!("cargo:rustc-env=ENDCHAT_COMMIT={build}");
    // A new commit moves HEAD's ref; staging or editing a tracked file changes the index or the file.
    for path in [".git/HEAD", ".git/index", ".git/refs/heads", ".git/packed-refs", "src", "tests", "Cargo.toml", "README.md", "prompts", "contrib"] {
        println!("cargo:rerun-if-changed={path}");
    }
}
