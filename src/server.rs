//! The control socket: line-delimited JSON between `endchat say/status/zoom/check` and `serve`.

use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::turn::{TurnEvent, say};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc::unbounded_channel;

pub fn socket_path(profile: &Path) -> PathBuf {
    profile.join("control.sock")
}

#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "lowercase")]
pub enum Request {
    Say { text: String },
    Status,
    Zoom { id: u64, n: u64, #[serde(default)] offset: usize },
    Check,
}

fn event_json(e: &TurnEvent) -> Value {
    match e {
        TurnEvent::Queued => json!({"event": "queued"}),
        TurnEvent::Agent(t) => json!({"event": "agent", "text": t}),
        TurnEvent::Tool(t) => json!({"event": "tool", "text": t}),
        TurnEvent::Echo(t) => json!({"event": "echo", "text": t}),
        TurnEvent::Error(t) => json!({"event": "error", "text": t}),
        TurnEvent::Done => json!({"event": "done"}),
    }
}

/// Answer one request, sending each reply line to `out`. Shared by the server and the local fallback.
pub async fn handle(engine: &Arc<Engine>, req: Request, out: tokio::sync::mpsc::UnboundedSender<Value>) {
    match req {
        Request::Say { text } => {
            let (tx, mut rx) = unbounded_channel();
            let turn = tokio::spawn(say(engine.clone(), text, tx));
            while let Some(e) = rx.recv().await {
                let _ = out.send(event_json(&e));
            }
            let _ = turn.await;
        }
        Request::Status => {
            let _ = out.send(json!({"ok": true, "text": engine.status().render()}));
        }
        Request::Zoom { id, n, offset } => {
            let r = engine.mem().zoom(id, n, offset);
            let _ = out.send(match r {
                Ok(t) => json!({"ok": true, "text": t}),
                Err(e) => json!({"ok": false, "text": e.to_string()}),
            });
        }
        Request::Check => {
            let r = engine.mem().check();
            let _ = out.send(match r {
                Ok(p) if p.is_empty() => json!({"ok": true, "text": "ok: log, tree, and views agree\n"}),
                Ok(p) => json!({"ok": false, "text": format!("{}\n", p.join("\n"))}),
                Err(e) => json!({"ok": false, "text": e.to_string()}),
            });
        }
    }
}

async fn serve_connection(engine: Arc<Engine>, stream: UnixStream) {
    let (r, mut w) = stream.into_split();
    let mut line = String::new();
    if BufReader::new(r).read_line(&mut line).await.unwrap_or(0) == 0 {
        return;
    }
    let (tx, mut rx) = unbounded_channel();
    match serde_json::from_str::<Request>(&line) {
        Ok(req) => {
            let engine = engine.clone();
            tokio::spawn(async move { handle(&engine, req, tx).await });
        }
        Err(e) => {
            let _ = tx.send(json!({"ok": false, "text": format!("bad request: {e}")}));
            drop(tx);
        }
    }
    while let Some(v) = rx.recv().await {
        if w.write_all(format!("{v}\n").as_bytes()).await.is_err() {
            return;
        }
    }
}

/// Run the profile until SIGINT or SIGTERM.
pub async fn serve(engine: Arc<Engine>) -> Result<()> {
    let path = socket_path(&engine.dir);
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).map_err(Error::from)?;
    let pump = tokio::spawn(engine.clone().pump());
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    eprintln!("endchat: serving {} ({} messages)", engine.dir.display(), engine.status().messages);
    loop {
        tokio::select! {
            conn = listener.accept() => {
                if let Ok((stream, _)) = conn {
                    tokio::spawn(serve_connection(engine.clone(), stream));
                }
            }
            _ = tokio::signal::ctrl_c() => break,
            _ = term.recv() => break,
        }
    }
    engine.stop();
    let _ = pump.await;
    engine.mem().save_views()?;
    let _ = std::fs::remove_file(&path);
    Ok(())
}

/// Send one request to a running server and hand each reply line to `on_line`.
/// Returns false when no server is listening.
pub async fn ask(profile: &Path, req: &Value, mut on_line: impl FnMut(Value)) -> Result<bool> {
    let Ok(stream) = UnixStream::connect(socket_path(profile)).await else { return Ok(false) };
    let (r, mut w) = stream.into_split();
    w.write_all(format!("{req}\n").as_bytes()).await?;
    let mut lines = BufReader::new(r).lines();
    while let Some(l) = lines.next_line().await? {
        on_line(serde_json::from_str(&l)?);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::Kind;
    use crate::profile::Config;
    use crate::testkit::FakeAop;

    #[tokio::test]
    async fn say_status_zoom_and_check_over_the_socket() {
        let fake = FakeAop::new(json!({"turn_replies": ["Hello back."]}));
        let d = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.aop_bin = fake.aop().bin;
        let engine = Engine::open(d.path(), cfg).unwrap();
        engine.mem().append(Kind::User, "earlier", None).unwrap();
        let server = tokio::spawn(serve(engine.clone()));
        for _ in 0..100 {
            if socket_path(d.path()).exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let mut lines = Vec::new();
        assert!(ask(d.path(), &json!({"cmd": "say", "text": "hello"}), |v| lines.push(v)).await.unwrap());
        assert_eq!(lines, [json!({"event": "agent", "text": "Hello back."}), json!({"event": "done"})]);
        let mut status = Vec::new();
        ask(d.path(), &json!({"cmd": "status"}), |v| status.push(v)).await.unwrap();
        assert!(status[0]["text"].as_str().unwrap().starts_with("messages: 3\n"));
        let mut z = Vec::new();
        ask(d.path(), &json!({"cmd": "zoom", "id": 0, "n": 1}), |v| z.push(v)).await.unwrap();
        assert_eq!(z[0], json!({"ok": true, "text": "user: earlier"}));
        ask(d.path(), &json!({"cmd": "zoom", "id": 1, "n": 4}), |v| z.push(v)).await.unwrap();
        assert_eq!(z[1]["ok"], false);
        let mut c = Vec::new();
        ask(d.path(), &json!({"cmd": "check"}), |v| c.push(v)).await.unwrap();
        assert_eq!(c[0]["ok"], true);
        ask(d.path(), &json!({"cmd": "nonsense"}), |v| c.push(v)).await.unwrap();
        assert_eq!(c[1]["ok"], false);
        server.abort();
    }
}
