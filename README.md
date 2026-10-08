# endchat

endchat is a command-line chat that never ends. Every message is logged word for word, a cheap model compresses the log into a binary tree of one-line summaries, and each user message starts a fresh model call whose context is a bounded view of that tree plus the new message. The model reads originals through a `zoom` tool. The design is [Victor Taelin's UniiChat recipe](https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449), written in Rust, with every model call and session handled by the [Agent Orchestration Process (AOP)](https://github.com/wakamex/agent-orchestration-process).

The design is in [SPEC.md](SPEC.md). Every decision made while implementing it, with the reasons, is in [decisions.md](decisions.md).

## Install

```
cargo install --path .
```

endchat needs `aop` on the path and a logged-in harness for it to drive (version 1 ships the Claude adapter). It holds no provider credentials and makes no provider requests itself.

## Use

A profile is one chat. It lives in `~/.endchat/NAME/`, or in any directory when the name contains a `/`.

```
endchat serve main          # owns the profile: lock, compaction workers, control socket
endchat say main "what did I decide about the staging database?"
endchat status main
endchat zoom main 40 8      # what the model sees for zoom(40, 8)
endchat check main          # log, tree, and views against each other
endchat sim                 # synthetic chat against a model of the prompt cache
```

`contrib/endchat@.service` is a systemd user unit that runs one `serve` per profile.

`say` prints the reply. A message sent while a turn runs is queued and reaches that turn between tool calls, or starts the next turn.

## Configuration

`~/.endchat/NAME/config.toml` is optional. Every key has a default:

```toml
aop_bin = "aop"

[compactor]
agent = "claude"
model = "haiku"
effort = "medium"
workers = 8          # concurrent AOP calls
accept_bytes = 640   # largest reply taken without a retry; 512 is the gist's strict rule
hard_bytes = 1024    # largest reply kept after the last of 5 attempts
timeout = 120

[turn]
agent = "claude"
model = "sonnet"
effort = "medium"
profile = "sealed"   # sealed, edit, review, or host
allow_host = false   # host needs this set to true
workdir = "/path/to/repo"   # where non-sealed profiles run
tools = ""           # built-in harness tools for turns, comma separated
no_web = true
timeout = 900
max_rounds = 40

[limits]
view_high = 128000
view_low = 64000
compaction_high = 32000
compaction_low = 16000
```

`instructions.md` in the profile directory is appended to the system prompt: who you are and how you want work done.

## What is stored

```
main/YYYY-MM-DD.jsonl   messages, one JSON record per line, never edited
main.idx                16-byte records locating each message; rebuilt from the log when stale
tree/YYYY-MM-DD.jsonl   summary nodes with the token usage of the calls that built them
tree.idx                the same, keyed by node slot
view.json               the chat view and the compaction view as last saved
ledger.jsonl            every AOP call with its usage, for the cache-read ratios
system-prompt.txt       the system prompt in use
.aop/                   AOP's own run records
```

## Limitations

- Turns call `zoom` and `date` as text lines because AOP cannot yet relay a tool server into a run. See decision 1.
- Only the Claude harness is supported for turns.
- The default turn profile is `sealed`: the model can read the chat's memory but touches no files.
- `prompts/system.md` follows the UniiChat prompt, which has no license. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Development

```
cargo test
ENDCHAT_LIVE_MESSAGES=300 cargo test live -- --ignored --nocapture   # real model, synthetic chat
endchat sim --compare-gist --messages 30000
```

`cargo test` runs the scheduler, view, log, and turn flow against `tests/fake_aop.py`, which checks every `aop run` invocation with the real `aop run --dry-run` when `aop` is installed.

## License

MIT, except `prompts/system.md`, which follows the UniiChat prompt and is covered by the notice in THIRD_PARTY_NOTICES.md.
