#!/usr/bin/env python3
"""Turn a Claude Code session transcript into a history file for `endchat import`.

    contrib/claude_session.py SESSION.jsonl [--since 2026-10-01T00:00:00Z] [--until ...] > history.json

The user's text becomes user messages, the assistant's text agent messages, its tool calls tool
messages (`Name {input}`, as endchat logs them) and their results echo messages. Thinking,
sidechains, compaction summaries, slash-command output and the harness's own records are left out.
"""
import argparse, json, sys

SKIP = ("<local-command", "<command-name>", "<command-message>", "Caveat:", "[Request interrupted")


def text_of(content):
    if isinstance(content, str):
        return content
    return "\n".join(b.get("text", "") for b in content or [] if isinstance(b, dict) and b.get("type") == "text")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("session")
    ap.add_argument("--since", default="", help="keep records at or after this ISO timestamp")
    ap.add_argument("--until", default="9", help="keep records before this ISO timestamp")
    a = ap.parse_args()
    out, seen = [], set()
    for line in open(a.session, encoding="utf-8"):
        r = json.loads(line)
        m = r.get("message")
        if r.get("type") not in ("user", "assistant") or not isinstance(m, dict) or r.get("isSidechain") or r.get("isMeta") or r.get("isCompactSummary"):
            continue
        if r.get("uuid") in seen or not a.since <= r.get("timestamp", "") < a.until:
            continue
        seen.add(r.get("uuid"))
        date = r["timestamp"]
        content = m.get("content")
        if isinstance(content, str):
            if content.strip() and not content.lstrip().startswith(SKIP):
                out.append({"kind": "user", "text": content, "date": date})
            continue
        for b in content or []:
            t = b.get("type")
            if t == "text" and b.get("text", "").strip() and not b["text"].lstrip().startswith(SKIP):
                out.append({"kind": "agent" if r["type"] == "assistant" else "user", "text": b["text"], "date": date})
            elif t == "tool_use":
                out.append({"kind": "tool", "text": f"{b.get('name')} {json.dumps(b.get('input'), separators=(',', ':'), ensure_ascii=False)}", "date": date})
            elif t == "tool_result":
                out.append({"kind": "echo", "text": text_of(b.get("content")), "date": date})
    json.dump(out, sys.stdout, ensure_ascii=False)


if __name__ == "__main__":
    main()
