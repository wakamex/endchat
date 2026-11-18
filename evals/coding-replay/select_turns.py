#!/usr/bin/env python3
"""Pick decision turns from long coding sessions, the same way every time, for paired replays.

    python3 select_turns.py trials.json OUT_DIR [--seed 1] [--max-calls 6] [--per-bucket 4]

trials.json lists the sessions to read:

    [{"name": "trial4", "claude_session": "~/.claude/projects/-code-w3sim/2a888f68-....jsonl",
      "from": "2026-11-14T22:40", "to": "2026-11-15T22:40"},
     {"name": "trial5", "endchat_profile": "~/.endchat/backups/w3end-trial5-...", "from": ..., "to": ...}]

Both kinds of record become one stream of messages (user, agent, tool call, tool result) with each
message's position in characters from the start of the session.

- A turn starts at each message the agent receives: a person's, another agent's, or a job notice.
- Its decision is its first tool call that changes something: an edit, a commit, a landing, a task
  selection or a job start (help commands excepted). A decision turn makes that call within
  --max-calls tool calls, so it rests mostly on what the agent already knew, and its decision
  chooses what to do next: a task selection, a job start or an edit. Commits and landings finish
  work already chosen, so they are left out.
- Its reach-back is how far before the turn, in characters, the agent last saw the oldest
  distinctive token (a path, an identifier, a hash, a long number) that it uses on the way to the
  decision. Tokens in the turn's own message, and tokens common across the session, are left out.
  A far reach-back is a decision that depends on old history, which is what a smaller history risks.
- A decision that starts a job takes the job's later notice as its outcome.

Decision turns are split by reach-back (near under 50,000 characters, mid to 400,000, far beyond)
and up to --per-bucket are drawn from each trial and bucket with a seeded generator, so the same
inputs and seed give the same selection. Writes OUT_DIR/turns.json (every decision turn with its
features) and OUT_DIR/selection.md (the drawn turns, for reading).
"""
import argparse
import json
import os
import random
import re
from collections import Counter

TOKEN = re.compile(r"(?:/[\w.-]+){2,}|\b[A-Za-z][\w]*[_.-][\w.-]*[A-Za-z0-9]\b|\b0x[0-9a-f]{6,}\b|\b[0-9a-f]{7,40}\b|\b\d[\d,]{3,}(?:\.\d+)?\b")
NOISE = re.compile(r"^(19|20)\d\d$|^[a-z]+(-[a-z]+)+$|^[\d,]*000$")
COMMON = 40
BUCKETS = [("near", 50_000), ("mid", 400_000), ("far", float("inf"))]
# Tool calls that change something. A Bash command counts by what it runs.
CHANGING_TOOLS = {"Edit", "Write", "MultiEdit", "NotebookEdit"}
CHANGING_COMMAND = re.compile(r"\bagent-job\s+(?!list|log|stop)[\w.-]+|\bgit\s+commit\b|land\.sh|\bdelegate\s+(select|close|done|started)\b|\bgit\s+push\b|apply_patch")
# What kind of decision a changing call is; the drawn turns are the kinds that choose what to do.
KINDS = [("task", re.compile(r"\bdelegate\s+select\b")), ("land", re.compile(r"land\.sh|\bgit\s+push\b")),
         ("job", re.compile(r"\bagent-job\s+(?!list|log|stop)[\w.-]+")), ("commit", re.compile(r"\bgit\s+commit\b|\bdelegate\s+(close|done|started)\b"))]
CHOOSING = {"task", "job", "edit"}
JOB_START = re.compile(r"\bagent-job\s+(?!list|log|stop)([\w.-]+)")
JOB_END = re.compile(r"Job ([\w.-]+) (succeeded|failed)[^\n]*")


def tokens(text):
    return {t for t in TOKEN.findall(text) if len(t) >= 5 and not NOISE.match(t)}


def claude_stream(path):
    """(kind, text, date) from a Claude Code session file, without side chains or meta messages."""
    for line in open(os.path.expanduser(path)):
        r = json.loads(line)
        if r.get("isSidechain") or r.get("isMeta"):
            continue
        m = r.get("message") or {}
        c = m.get("content")
        date = r.get("timestamp", "")
        if r.get("type") == "user":
            if isinstance(c, str):
                if not c.startswith(("<local-command", "<command-name>")):
                    yield "user", c, date
                continue
            for b in c or []:
                if b.get("type") == "tool_result":
                    x = b.get("content")
                    yield "echo", x if isinstance(x, str) else " ".join(i.get("text", "") for i in x or [] if isinstance(i, dict)), date
                elif b.get("type") == "text":
                    yield "user", b.get("text", ""), date
        elif r.get("type") == "assistant":
            for b in c or []:
                if b.get("type") == "text":
                    yield "agent", b["text"], date
                elif b.get("type") == "tool_use":
                    yield "tool", f"{b.get('name')} {json.dumps(b.get('input'))}", date


def endchat_stream(profile):
    """(kind, text, date) from an endchat profile's log; a message delivered during a turn is not a turn start."""
    import glob
    for f in sorted(glob.glob(os.path.join(os.path.expanduser(profile), "main", "*.jsonl"))):
        for line in open(f):
            m = json.loads(line)
            kind = "queued" if m["kind"] == "user" and m.get("run") else m["kind"]
            yield kind, m["text"], m["date"]


def turns(trial):
    stream = claude_stream(trial["claude_session"]) if "claude_session" in trial else endchat_stream(trial["endchat_profile"])
    msgs, pos = [], 0
    for kind, text, date in stream:
        msgs.append({"kind": kind, "text": text, "date": date, "pos": pos})
        pos += len(text)
    count = Counter(t for m in msgs for t in tokens(m["text"]))
    last = {}  # token -> position of its latest appearance so far
    starts = [k for k, m in enumerate(msgs) if m["kind"] == "user"]
    out = []
    for j, s in enumerate(starts):
        end = starts[j + 1] if j + 1 < len(starts) else len(msgs)
        # Positions seen before this turn.
        before = dict(last)
        for m in msgs[s:end]:
            for t in tokens(m["text"]):
                last[t] = m["pos"]
        trig = msgs[s]
        if not (trial["from"] <= trig["date"][:16] < trial["to"]):
            continue
        calls, decision = 0, None
        used = set()
        for k in range(s + 1, end):
            m = msgs[k]
            if m["kind"] in ("tool", "agent"):
                used |= tokens(m["text"])
            if m["kind"] != "tool":
                continue
            name = m["text"].split(" ", 1)[0]
            if "--help" not in m["text"] and (name in CHANGING_TOOLS or CHANGING_COMMAND.search(m["text"])):
                decision = k
                break
            calls += 1
        own = tokens(trig["text"])
        reach = [(trig["pos"] - before[t], t) for t in used - own if t in before and count[t] <= COMMON]
        far = max(reach, default=(0, None))
        turn = {
            "trial": trial["name"],
            "index": s,
            "date": trig["date"],
            "trigger": "job notice" if "From: agent-job" in trig["text"] else "agent message" if "[Panetone cross-agent message]" in trig["text"] else "person",
            "trigger_text": trig["text"],
            "calls_before_decision": calls,
            "decision": msgs[decision]["text"] if decision is not None else None,
            "decision_kind": None if decision is None else "edit" if msgs[decision]["text"].split(" ", 1)[0] in CHANGING_TOOLS else next((n for n, r in KINDS if r.search(msgs[decision]["text"])), "other"),
            "reach_back_chars": far[0],
            "reach_back_token": far[1],
            "history_chars": trig["pos"],
            "turn_messages": end - s,
        }
        job = JOB_START.search(turn["decision"] or "")
        if job:
            for m in msgs[end:]:
                hit = JOB_END.search(m["text"]) if m["kind"] in ("user", "queued") else None
                if hit and hit.group(1) == job.group(1):
                    turn["outcome"] = hit.group(0)
                    break
        out.append(turn)
    return out


def bucket(chars):
    return next(name for name, top in BUCKETS if chars < top)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("trials")
    ap.add_argument("out")
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--max-calls", type=int, default=6)
    ap.add_argument("--per-bucket", type=int, default=4)
    a = ap.parse_args()
    trials = json.load(open(a.trials))
    os.makedirs(a.out, exist_ok=True)
    all_turns, drawn = [], []
    for trial in trials:
        ts = turns(trial)
        decisions = [t for t in ts if t["decision"] and t["calls_before_decision"] <= a.max_calls and t["decision_kind"] in CHOOSING]
        all_turns += decisions
        for name, _ in BUCKETS:
            pool = sorted((t for t in decisions if bucket(t["reach_back_chars"]) == name), key=lambda t: t["index"])
            pick = random.Random(f"{a.seed}:{trial['name']}:{name}").sample(pool, min(a.per_bucket, len(pool)))
            drawn += sorted(pick, key=lambda t: t["index"])
        print(f"{trial['name']}: {len(ts)} turns in the window, {len(decisions)} decision turns: " + ", ".join(f"{n} {sum(bucket(t['reach_back_chars']) == n for t in decisions)}" for n, _ in BUCKETS))
    json.dump({"seed": a.seed, "max_calls": a.max_calls, "trials": trials, "decision_turns": all_turns, "drawn": [(t["trial"], t["index"]) for t in drawn]}, open(os.path.join(a.out, "turns.json"), "w"), indent=1)
    with open(os.path.join(a.out, "selection.md"), "w") as f:
        f.write(f"# Drawn decision turns (seed {a.seed})\n\n")
        for t in drawn:
            f.write(f"## {t['trial']} turn at {t['index']}, {t['date'][:16]}, {bucket(t['reach_back_chars'])} reach-back\n\n")
            f.write(f"- Trigger ({t['trigger']}): {t['trigger_text'][:300]!r}\n")
            f.write(f"- Decision ({t['decision_kind']}) after {t['calls_before_decision']} calls: {t['decision'][:300]!r}\n")
            f.write(f"- Reach-back: {t['reach_back_chars']:,} characters, token `{t['reach_back_token']}`\n")
            f.write(f"- Outcome: {t.get('outcome', 'none recorded')}\n\n")
    print(f"drew {len(drawn)} turns; wrote {a.out}/turns.json and selection.md")


if __name__ == "__main__":
    main()
