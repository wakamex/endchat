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
    for cmd in ["serve", "say", "chat", "status", "zoom", "check", "search", "sim"] {
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
fn say_without_a_server_runs_the_turn_itself_and_reports_aop_failures() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), "aop_bin = \"/nonexistent/aop\"\n").unwrap();
    let o = endchat(&["say", dir.path().to_str().unwrap(), "hello"]);
    assert!(!o.status.success());
    assert!(text(&o).contains("cannot start /nonexistent/aop"), "{}", text(&o));
}

#[test]
fn sim_prints_its_report() {
    let o = endchat(&["sim", "--messages", "120"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("mean_rewritten_lines"), "{}", text(&o));
    let o = endchat(&["sim", "--compare-gist", "--messages", "4000"]);
    assert!(o.status.success() && text(&o).contains("batched between 64 KB and 128 KB"), "{}", text(&o));
}

#[test]
fn import_loads_dated_history_and_summarizes_short_messages_for_free() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().to_str().unwrap();
    let file = dir.path().join("history.json");
    std::fs::write(
        &file,
        r#"[{"kind":"user","text":"I graduated in Business Administration","date":"2023-05-20T02:21:00Z"},
            {"kind":"agent","text":"Congratulations!","date":"2023-05-20T02:22:00Z"},
            {"kind":"user","text":"older note filed after","date":"2023-05-01T00:00:00Z"}]"#,
    )
    .unwrap();
    let o = endchat(&["import", p, file.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    let report: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!((report["messages"].as_u64(), report["calls"].as_u64(), report["complete"].as_bool()), (Some(3), Some(0), Some(true)));
    let o = endchat(&["zoom", p, "0", "1"]);
    assert_eq!(text(&o), "user: I graduated in Business Administration");
    assert!(text(&endchat(&["check", p])).starts_with("ok:"));
}

#[test]
fn chat_without_a_server_answers_each_line_and_keeps_its_history() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("scenario.json"), r#"{"turn_replies": ["Hi there.", "Still here."]}"#).unwrap();
    let aop = dir.path().join("aop");
    std::fs::write(&aop, format!("#!/bin/sh\nexport FAKE_AOP_DIR='{}'\nexec python3 '{}/tests/fake_aop.py' \"$@\"\n", state.display(), env!("CARGO_MANIFEST_DIR"))).unwrap();
    std::fs::set_permissions(&aop, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let profile = dir.path().join("profile");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("config.toml"), format!("aop_bin = \"{}\"\n", aop.display())).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_endchat"))
        .args(["chat", profile.to_str().unwrap()])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"hello\n\nare you there?\n").unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", text(&o));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("Hi there.") && out.contains("Still here."), "{}", text(&o));
    assert!(text(&o).contains("no server running, so this process owns the profile"));
    assert!(text(&endchat(&["status", profile.to_str().unwrap()])).starts_with("messages: 4\n"), "two exchanges are logged");
}
