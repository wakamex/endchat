#!/usr/bin/env python3
"""Report how each turn of a native endchat profile used the prompt cache on its first call, and
why a turn missed it.

    python3 contrib/turn_cache.py ~/.endchat/w3end [--since 2026-11-16T20:00]

A turn's first call should read the history the previous turn cached and write only what is new.
For each turn (each AOP run that starts one, not its resumes) this prints the history size, the
first call's cache read and write, and, when the read is small, what differed from the previous
turn: a gap of over an hour (the cache expired), a cut (the history shrank), the system prompt or
tools (Claude Code's `api-request-shape` hash), or the context Claude Code puts ahead of the
history in the first message. It reads the run records under PROFILE/.aop/runs and the turns'
Claude Code session files under ~/.claude/projects, and ends with totals.
"""
import argparse
import datetime
import glob
import json
import os

MISS = 20_000  # a first call reading less than this did not reuse the history


def session_file(run_dir):
    try:
        sid = json.loads(open(os.path.join(run_dir, "events.jsonl")).readline())["session_id"]
    except (OSError, ValueError, KeyError):
        return None
    found = glob.glob(os.path.expanduser(f"~/.claude/projects/*/{sid}.jsonl"))
    return found[0] if found else None


def turn(run_dir):
    try:
        req = json.load(open(os.path.join(run_dir, "request.json")))
    except (OSError, ValueError):
        return None
    if req.get("parent_run_id"):
        return None
    f = session_file(run_dir)
    if not f:
        return None
    shape = context = first = None
    for line in open(f):
        r = json.loads(line)
        t = r.get("type")
        if t == "api-request-shape" and shape is None:
            shape = r.get("shapeHash")
        elif t == "api-request-blob" and context is None:
            content = (r.get("message") or {}).get("content") or []
            # Everything before endchat's history: Claude Code's own context blocks.
            context = []
            for b in content:
                text = b.get("text", "") if isinstance(b, dict) else ""
                if text.startswith(("<chat>", "[endchat history")):
                    break
                context.append(text)
        elif t == "assistant" and first is None and (r.get("message") or {}).get("usage"):
            u = r["message"]["usage"]
            first = (u.get("cache_read_input_tokens", 0), u.get("cache_creation_input_tokens", 0))
    if first is None:
        return None
    return {"date": req.get("created_at", ""), "run": os.path.basename(run_dir), "chars": len(req.get("prompt") or ""), "shape": shape, "context": context, "read": first[0], "write": first[1]}


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("profile")
    ap.add_argument("--since", default="")
    a = ap.parse_args()
    turns = [t for d in glob.glob(os.path.join(os.path.expanduser(a.profile), ".aop", "runs", "*")) if (t := turn(d)) and t["date"] >= a.since]
    turns.sort(key=lambda t: t["date"])
    causes = {}
    print(f"{'started':19}  {'run':8}  {'history':>9}  {'read':>9}  {'write':>9}  why the read was small")
    for prev, cur in zip([None] + turns, turns):
        why = ""
        if cur["read"] < MISS and prev:
            parts = []
            gap = datetime.datetime.fromisoformat(cur["date"]) - datetime.datetime.fromisoformat(prev["date"])
            if gap.total_seconds() > 3600:
                parts.append("gap over 1h")
            if cur["chars"] < 0.6 * prev["chars"]:
                parts.append("cut")
            if cur["shape"] != prev["shape"]:
                parts.append("system prompt or tools")
            if cur["context"] != prev["context"]:
                parts.append("context before the history")
            why = ", ".join(parts) or "unexplained"
            causes.setdefault(why, [0, 0])
            causes[why][0] += 1
            causes[why][1] += cur["write"]
        print(f"{cur['date'][:19]}  {cur['run'][:8]}  {cur['chars']:>9}  {cur['read']:>9}  {cur['write']:>9}  {why}")
    print(f"\n{len(turns)} turns; first calls wrote {sum(t['write'] for t in turns):,} tokens and read {sum(t['read'] for t in turns):,}")
    for why, (n, w) in sorted(causes.items(), key=lambda x: -x[1][1]):
        print(f"  {w:>11,} tokens written by {n} turns missing the cache: {why}")


if __name__ == "__main__":
    main()
