#!/usr/bin/env python3
"""Turn a Claude Code session transcript into a history file for `endchat import`.

    contrib/claude_session.py SESSION.jsonl [--since 2026-10-01T00:00:00Z] [--until ...] > history.json
    contrib/claude_session.py SESSION.jsonl --after-compaction summary.md > history.json

With --after-compaction, only the records after the session's last compaction are kept, and that
compaction's summary is written to the given file, for `endchat import --summary`, so endchat starts
from the state the session was in.

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


# Claude Code wraps a compaction summary in one-time instructions for the resumed session; sent with
# every endchat turn they would mislead, so only the summary is kept.
WRAP_END = ("\nIf you need specific details from before compaction", "\nContinue the conversation from where it left off", "\nThe messages after this summary are the most recent")


def summary_only(text):
    body = text.split("\nSummary:\n", 1)[1] if "\nSummary:\n" in text else text
    for marker in WRAP_END:
        body = body.split(marker, 1)[0]
    return body.strip()


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("session")
    ap.add_argument("--since", default="", help="keep records at or after this ISO timestamp")
    ap.add_argument("--until", default="9", help="keep records before this ISO timestamp")
    ap.add_argument("--after-compaction", metavar="FILE", help="keep only what follows the last compaction, and write its summary to FILE")
    a = ap.parse_args()
    lines = open(a.session, encoding="utf-8").read().splitlines()
    if a.after_compaction:
        at = max((k for k, l in enumerate(lines) if '"isCompactSummary":true' in l.replace(" ", "")), default=None)
        if at is None:
            sys.exit("the session has no compaction")
        open(a.after_compaction, "w", encoding="utf-8").write(summary_only(text_of(json.loads(lines[at])["message"]["content"])) + "\n")
        lines = lines[at + 1:]
    out, seen = [], set()
    for line in lines:
        r = json.loads(line)
        m = r.get("message")
        if r.get("type") not in ("user", "assistant") or not isinstance(m, dict) or r.get("isSidechain") or r.get("isMeta") or r.get("isCompactSummary"):
            continue
        if r.get("uuid") in seen or not a.since <= r.get("timestamp", "") < a.until:
            continue
        seen.add(r.get("uuid"))
        date = r["timestamp"]
        content = m.get("content")
        kind = "agent" if r["type"] == "assistant" else "user"
        if isinstance(content, str):
            if content.strip() and not content.lstrip().startswith(SKIP):
                out.append({"kind": kind, "text": content, "date": date})
            continue
        # Text beside tool results is the harness's own (reminders, hook output), not a message the
        # user sent; as a user message it would also start a turn in the middle of one.
        results = any(b.get("type") == "tool_result" for b in content or [])
        for b in content or []:
            t = b.get("type")
            if t == "text" and kind == "user" and results:
                continue
            if t == "text" and b.get("text", "").strip() and not b["text"].lstrip().startswith(SKIP):
                out.append({"kind": kind, "text": b["text"], "date": date})
            elif t == "tool_use":
                out.append({"kind": "tool", "text": f"{b.get('name')} {json.dumps(b.get('input'), separators=(',', ':'), ensure_ascii=False)}", "date": date})
            elif t == "tool_result":
                out.append({"kind": "echo", "text": text_of(b.get("content")), "date": date})
    json.dump(out, sys.stdout, ensure_ascii=False)


if __name__ == "__main__":
    main()
