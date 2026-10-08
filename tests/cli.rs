//! Smoke tests for the command line: each subcommand starts and gives useful output.

use std::process::{Command, Output};

fn endchat(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_endchat")).args(args).output().unwrap()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

#[test]
fn help_lists_every_subcommand() {
    let o = endchat(&["--help"]);
    assert!(o.status.success());
    let t = text(&o);
    for cmd in ["serve", "say", "status", "zoom", "check", "sim"] {
        assert!(t.contains(cmd), "{cmd} missing from help:\n{t}");
    }
}

#[test]
fn status_zoom_and_check_open_an_idle_profile_directly() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().to_str().unwrap();
    let o = endchat(&["status", p]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).starts_with("messages: 0\n"), "{}", text(&o));
    let o = endchat(&["check", p]);
    assert!(o.status.success() && text(&o).starts_with("ok:"), "{}", text(&o));
    let o = endchat(&["zoom", p, "0", "1"]);
    assert!(!o.status.success());
    assert!(text(&o).contains("the chat has 0 messages"), "{}", text(&o));
}

#[test]
fn say_without_a_server_says_how_to_start_one() {
    let dir = tempfile::tempdir().unwrap();
    let o = endchat(&["say", dir.path().to_str().unwrap(), "hello"]);
    assert!(!o.status.success());
    assert!(text(&o).contains("endchat serve"), "{}", text(&o));
}

#[test]
fn sim_prints_its_report() {
    let o = endchat(&["sim", "--messages", "120"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("mean_rewritten_lines"), "{}", text(&o));
    let o = endchat(&["sim", "--compare-gist", "--messages", "4000"]);
    assert!(o.status.success() && text(&o).contains("batched between 64 KB and 128 KB"), "{}", text(&o));
}
