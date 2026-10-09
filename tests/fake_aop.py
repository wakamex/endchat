#!/usr/bin/env python3
"""A stand-in for `aop` used by the tests.

It checks every `run` invocation against the real `aop run --dry-run` when the real binary is
installed (so an invalid flag or model fails here exactly as it would for real), then answers from a
scenario file instead of calling a model. State lives in $FAKE_AOP_DIR.
"""
import fcntl, json, os, shutil, subprocess, sys

D = os.environ["FAKE_AOP_DIR"]
KNOWN_RUN = {"--agent", "--model", "--effort", "--profile", "--tools", "--system-prompt-file", "--prompt-file", "--prompt-parts", "--timeout"}
KNOWN_FLAGS = {"--no-web", "--json"}


def load(name, default):
    try:
        with open(os.path.join(D, name)) as f:
            return json.load(f)
    except FileNotFoundError:
        return default


def save(name, value):
    with open(os.path.join(D, name), "w") as f:
        json.dump(value, f)


def log(entry):
    with open(os.path.join(D, "calls.jsonl"), "a") as f:
        f.write(json.dumps(entry) + "\n")


def die(msg, code=2):
    print(msg, file=sys.stderr)
    sys.exit(code)


def parse(argv):
    opts, pos, i = {}, [], 0
    while i < len(argv):
        a = argv[i]
        if a in KNOWN_FLAGS:
            opts[a] = True
        elif a in KNOWN_RUN:
            if i + 1 >= len(argv):
                die(f"fake aop: {a} needs a value")
            opts[a] = argv[i + 1]
            i += 1
        elif a.startswith("--"):
            die(f"fake aop: unrecognized arguments: {a}")
        else:
            pos.append(a)
        i += 1
    return opts, pos


def validate_with_real_aop(argv):
    real = os.environ.get("FAKE_REAL_AOP") or shutil.which("aop")
    if not real or os.path.abspath(real) == os.path.abspath(sys.argv[0]):
        return
    p = subprocess.run([real, "run", *argv, "--dry-run"], capture_output=True, text=True)
    if p.returncode != 0:
        die(p.stderr.strip() or "aop rejected the arguments", p.returncode)


def usage_for(prompt):
    n = max(1, len(prompt) // 4)
    return {"input_tokens": n + 500, "cached_input_tokens": int(n * 0.6), "output_tokens": 40, "reasoning_output_tokens": 0}


def counter(name):
    n = load(name, 0) + 1
    save(name, n)
    return n


def summary_of(prompt, size):
    body = prompt.split("<input>\n")[-1].split("\n</input>")[0]
    return (body.replace("\n", " ") + " " + "pad " * 1000)[:size]


def reply_for(prompt, attempt, chain, scenario):
    mode = scenario.get("compact", "ok")
    if "Compaction:" in prompt or prompt.startswith("Too long:"):
        if mode == "ok":
            return summary_of(prompt, 300)
        if mode == "always_long":
            return summary_of(prompt, 900)
        if mode == "never_small":
            return summary_of(prompt, 3000)
        if mode.startswith("long:"):
            return summary_of(prompt, 700 if attempt <= int(mode[5:]) else 300)
        if mode == "empty":
            return ""
        if mode == "prefixed":
            return "0+1|" + summary_of(prompt, 300)
        return summary_of(prompt, 300)
    replies = scenario.get("turn_replies", [])
    n = counter("turn_calls") - 1
    return replies[n] if n < len(replies) else "ok"


def finish(run_id, reply, prompt, scenario, ok=True):
    result = {
        "run_id": run_id, "session_id": "sess-" + run_id, "status": "succeeded" if ok else "failed",
        "succeeded": ok, "timed_out": False, "final_message": reply, "model": "claude-fake-1",
        "usage": usage_for(prompt), "accounting_status": "complete", "usage_schema": "aop-token-usage-v3",
        "error": None if ok else {"message": "scripted failure"},
    }
    runs = os.path.join(os.getcwd(), ".aop", "runs", run_id)
    os.makedirs(runs, exist_ok=True)
    events = scenario.get("events", {}).get(str(counter("event_calls")), None)
    with open(os.path.join(runs, "events.jsonl"), "w") as f:
        for ev in events if events is not None else [{"type": "assistant", "message": {"content": [{"type": "text", "text": reply}]}}]:
            f.write(json.dumps(ev) + "\n")
    print(json.dumps(result))
    sys.exit(0 if ok else 1)


def main():
    cmd, argv = sys.argv[1], sys.argv[2:]
    lock = open(os.path.join(D, "lock"), "w")
    fcntl.flock(lock, fcntl.LOCK_EX)  # callers run in parallel; the state files are shared
    scenario = load("scenario.json", {})
    chains = load("chains.json", {})
    if cmd == "run":
        opts, pos = parse(argv)
        validate_with_real_aop(argv)
        if "--prompt-file" not in opts and "--prompt-parts" not in opts:
            die("fake aop: --prompt-file or --prompt-parts is required")
        marks = 0
        if "--prompt-parts" in opts:
            parts = json.load(open(opts["--prompt-parts"]))
            marks = sum(1 for p in parts if p.get("cache"))
            if marks > 1:
                die("fake aop: at most one prompt block may be cached")
            prompt = "".join(p["text"] for p in parts)
        else:
            prompt = open(opts["--prompt-file"]).read()
        system = open(opts["--system-prompt-file"]).read() if "--system-prompt-file" in opts else None
        n = counter("runs")
        run_id = f"fake-{n}"
        chains[run_id] = 1
        save("chains.json", chains)
        log({"cmd": "run", "run_id": run_id, "args": argv, "prompt": prompt, "system": system, "marks": marks})
        if scenario.get("fail_first", 0) >= counter("chain_starts"):
            finish(run_id, "", prompt, scenario, ok=False)
        if scenario.get("compact") == "fail":
            finish(run_id, "", prompt, scenario, ok=False)
        finish(run_id, reply_for(prompt, 1, run_id, scenario), prompt, scenario)
    elif cmd == "resume":
        opts, pos = parse([a for a in argv if a != "--json"] + ["--json"])
        if len(pos) != 1 or "--prompt-file" not in opts:
            die("fake aop: resume needs RUN_ID and --prompt-file")
        prev = pos[0]
        if prev not in chains:
            die(f"fake aop: unknown run {prev}")
        prompt = open(opts["--prompt-file"]).read()
        n = counter("runs")
        run_id = f"fake-{n}"
        chains[run_id] = chains[prev] + 1
        save("chains.json", chains)
        log({"cmd": "resume", "run_id": run_id, "from": prev, "args": argv, "prompt": prompt})
        finish(run_id, reply_for(prompt, chains[run_id], run_id, scenario), prompt, scenario)
    elif cmd == "cleanup":
        log({"cmd": "cleanup", "run_id": argv[0]})
    else:
        die(f"fake aop: unsupported command {cmd}")


main()
