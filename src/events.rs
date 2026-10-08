//! The Claude adapter: turns a run's retained `events.jsonl` into log entries.
//!
//! Only the order of text, tool calls, and tool results is used. Thinking is dropped.

use serde_json::Value;

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Text(String),
    ToolCall { name: String, input: String },
    ToolResult(String),
}

fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"),
        other => other.to_string(),
    }
}

/// Events in order. Lines that are not JSON are skipped.
pub fn parse_claude(events_jsonl: &str) -> Vec<Event> {
    let mut out = Vec::new();
    for line in events_jsonl.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) else { continue };
        for b in blocks {
            match (v.get("type").and_then(Value::as_str), b.get("type").and_then(Value::as_str)) {
                (Some("assistant"), Some("text")) => {
                    if let Some(t) = b.get("text").and_then(Value::as_str).filter(|t| !t.trim().is_empty()) {
                        out.push(Event::Text(t.to_owned()));
                    }
                }
                (Some("assistant"), Some("tool_use")) => out.push(Event::ToolCall {
                    name: b.get("name").and_then(Value::as_str).unwrap_or("?").to_owned(),
                    input: b.get("input").map(Value::to_string).unwrap_or_default(),
                }),
                (Some("user"), Some("tool_result")) => out.push(Event::ToolResult(result_text(b.get("content").unwrap_or(&Value::Null)))),
                _ => {}
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_text_calls_and_results_and_drops_thinking() {
        let events = [
            r#"{"type":"system","subtype":"init"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"Looking."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#,
            "not json",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"a.txt","is_error":false}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":[{"type":"text","text":"x"},{"type":"text","text":"y"}]}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Done."}]}}"#,
            r#"{"type":"result","usage":{}}"#,
        ]
        .join("\n");
        assert_eq!(
            parse_claude(&events),
            [
                Event::Text("Looking.".into()),
                Event::ToolCall { name: "Bash".into(), input: r#"{"command":"ls"}"#.into() },
                Event::ToolResult("a.txt".into()),
                Event::ToolResult("x\ny".into()),
                Event::Text("Done.".into()),
            ]
        );
    }
}
