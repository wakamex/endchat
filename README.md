# endchat

endchat is a command-line chat that never ends. Every message is logged word for word, a cheap model compresses the log into a binary tree of one-line summaries, and each user message starts a fresh model call whose context is a bounded view of that tree plus the new message. The model reads originals through a `zoom` tool. The design is [Victor Taelin's UniiChat recipe](https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449), written in Rust, with every model call and session handled by the [Agent Orchestration Process (AOP)](https://github.com/wakamex/agent-orchestration-process).

The design is in [SPEC.md](SPEC.md). Every decision made while implementing it, with the reasons, is in [decisions.md](decisions.md). How well the model reads its history through zoom, against search tools, a smaller view and the whole history in context, is evaluated in [evals/search-tools](evals/search-tools/README.md).

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
endchat search main 'vault|safe' -k user -n 5 -C 1   # regex over the log, each message once
endchat sim                 # synthetic chat against a model of the prompt cache
```

`contrib/endchat@.service` is a systemd user unit that runs one `serve` per profile.

`say` prints the reply. A message sent while a turn runs is queued and reaches that turn between tool calls, or starts the next turn.

## Configuration

`~/.endchat/NAME/config.toml` is optional. Every key has a default:

```toml
aop_bin = "aop"
# system_prompt = "/path/to/system.md"   # replaces the built-in prompts/system.md; instructions.md is then not appended

[compactor]
agent = "claude"
model = "haiku"
effort = "medium"
workers = 8          # concurrent AOP calls
accept_bytes = 640   # largest reply taken without a retry; 512 is the gist's strict rule
hard_bytes = 1024    # largest reply kept after the last of 5 attempts
timeout = 120
tree = true          # summarize every message; false needs turn.raw, which then summarizes once per cut

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
zoom = true          # answer zoom and date calls; false leaves the model with its commands only
view = true          # send the chat view with each message; false sends the message alone
# tail_note = "..."  # replaces the note sent with every message
raw = false          # send the chat word for word instead of the view (see Raw history below)
stub_chars = 1000    # with raw: longer tool results are stubbed once older than the previous turn
native = false       # run turns as plain Claude Code in workdir: its own system prompt, CLAUDE.md, memory, skills, tools and sessions; needs profile = "host" and workdir, and no_web = false for Bash and web tools
watch_units = false  # tell the chat when a systemd unit its tools started stops
prune_sessions = false  # with native: delete each turn's Claude Code session files when it ends

[turn.commands]      # extra text tools: a reply line `name(ARG)` or `name ARG` runs `program PROFILE_DIR ARG`
# search = "/path/to/search_tool"

[limits]
view_high = 128000
view_low = 64000
compaction_high = 32000
compaction_low = 16000
raw_high = 640000    # with raw: bytes of history that start a cut to summary lines
raw_low = 160000     # what the cut leaves word for word
```

`instructions.md` in the profile directory is appended to the system prompt: who you are and how you want work done.

### Raw history

With `raw = true` a turn sends the chat itself, each message as `id|kind: text`, in place of the view. A tool result longer than `stub_chars` is replaced by a one-line stub naming the call once it is older than the previous turn, and a call that long, such as a command writing a whole script, keeps only its first 200 characters. Each turn is one cache block and the cache mark sits on the last block before the previous turn, the part that no later turn changes. When the history passes `raw_high` bytes, its oldest turns are cut off until at most `raw_low` bytes remain, never cutting into the previous turn, and one call to the compactor model summarizes what was cut, together with the summary of any earlier cut, as `prompts/cut.md` asks: intent, decisions, facts to keep, corrections, standing instructions, open threads, and where it left off. The cut and its summary are kept in `raw.json`, so the prefix stays the same until the next cut; if the summary call fails, the history stays whole until a later turn. With `compactor.tree = false` nothing else is summarized, so imports need no summaries either.

With `native = true` as well, turns run as Claude Code does on its own in `workdir`, through `aop --workdir --native-state`, while the run records stay in the profile. Each turn is a Claude Code session of its own whose file holds the whole prompt; `prune_sessions = true` deletes those files when the turn ends, so they neither pile up nor repeat the chat in session searches. `endchat search` searches the profile's own log instead.

`contrib/claude_session.py SESSION.jsonl > history.json` turns a Claude Code session transcript into a file for `endchat import`, so an existing session's history can continue in endchat.

With `watch_units = true`, a tool result containing `Running as unit: NAME`, which `systemd-run` prints, makes endchat poll that unit and send a `[systemd]` message when it stops, which starts a turn. Watched units are kept in `units.txt` across restarts.

## What is stored

```
main/YYYY-MM-DD.jsonl   messages, one JSON record per line, never edited
main.idx                16-byte records locating each message; rebuilt from the log when stale
tree/YYYY-MM-DD.jsonl   summary nodes with the token usage of the calls that built them
tree.idx                the same, keyed by node slot
view.json               the chat view and the compaction view as last saved
raw.json                with raw: where the raw history starts
units.txt               with watch_units: units still being watched
ledger.jsonl            every AOP call with its usage, for the cache-read ratios
turns.jsonl             one line per finished turn: the log length at its end and whether it succeeded
system-prompt.txt       the system prompt in use
.aop/                   AOP's own run records
```

## Limitations

- Turns call `zoom` and `date` as text lines because AOP cannot yet relay a tool server into a run. See decision 1.
- Only the Claude harness is supported for turns.
- The default turn profile is `sealed`: the model can read the chat's memory but touches no files.
- `prompts/system.md` follows the UniiChat prompt and is under the Apache License 2.0. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Development

```
cargo test
ENDCHAT_LIVE_MESSAGES=300 cargo test live -- --ignored --nocapture   # real model, synthetic chat
endchat sim --compare-gist --messages 30000
```

`cargo test` runs the scheduler, view, log, and turn flow against `tests/fake_aop.py`, which checks every `aop run` invocation with the real `aop run --dry-run` when `aop` is installed.

## License

MIT, except `prompts/system.md`, which follows the UniiChat prompt and is under the Apache License 2.0 (`prompts/LICENSE-UniiChat`); see THIRD_PARTY_NOTICES.md.
