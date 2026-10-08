# endchat specification

Status: draft 1, 2026-10-08. Nothing here is implemented.

## Summary

endchat is a command-line chat that never ends. Every message is logged word for word, a cheap model compresses the log into a binary tree of one-line summaries, and each user message starts a fresh model call whose context is a bounded view of that tree plus the new message. The model reads originals through a `zoom` tool. The design is [Victor Taelin's UniiChat recipe](https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449) (the gist), implemented in Rust, with every model call and every session handled by AOP instead of by direct provider calls.

Two existing implementations were read against the gist before this spec: the UniiChat 1.0.137 bundle (its JavaScript, read from the Linux executable without running it, referred to here as Unii) and [pi-optchat](https://github.com/jonaslsaa/pi-optchat) 0.7.2. Each follows the gist's core and departs from it in different places. endchat keeps the gist's numbers and algorithm, takes the better answer where the two differ, and fixes the gaps both share.

Short result of that comparison, which drives this spec:

- Both implement the 512-byte nodes, the due-order merge, the 128 KB to 64 KB batched view, the saved view, and the compaction view.
- Unii splits long messages into pages; pi-optchat does not. pi-optchat runs 8 compaction workers as the gist says; Unii runs 64. pi-optchat fsyncs writes and detects a second writer; Unii does not.
- Neither lets compactions share the turns' tool and system-prompt cache entry, which the gist relies on.
- Unii stores a clipped raw prefix as a permanent summary when a compaction fails for good; pi-optchat retries and lets the turn proceed with a placeholder.

## Goals

1. Follow the gist's algorithm and constants exactly, and prove it with tests that run the gist's own claims.
2. Never lose a message and never store a degraded summary as if it were a real one.
3. Make prompt-cache behavior measurable on every call, and fix the cache design to what AOP and the chosen harness can actually deliver.
4. Use AOP for every call: no provider credentials, no provider HTTP clients, no session files read by endchat.
5. Stay small. One binary, one profile per process, no user interface beyond a line-based command line.

## Non-goals for version 1

Each has a trigger for adding it later, so the cost of waiting is visible.

| Left out | Add it when |
| --- | --- |
| Terminal user interface, web interface | the line-based interface is the reported bottleneck |
| Multiple profiles in one process | one process per profile (a systemd user unit each) proves awkward |
| Subagents | the user asks for them; AOP `batch` is the natural base |
| Importing old chat history | a real archive needs to be loaded; it would be a `note` kind with a historical flag |
| Computer use, device routing | a second machine must run tools |
| Memory text search | zoom is measured to fail on a real recall task |

## How endchat uses AOP

AOP runs bounded, non-interactive jobs across agent harnesses (Claude Code, Codex, and others) and owns isolation, deadlines, usage accounting, and exact session resume. endchat is a controller above it: it builds prompts, spawns `aop run` and `aop resume`, and reads the normalized `--json` result. It never reads a harness's own session files and never holds provider credentials.

What AOP provides today, from its documentation and source:

| Need | AOP feature |
| --- | --- |
| One model call | `aop run --agent A --model M --effort E --profile P --prompt-file F --json` |
| Same-conversation retry | `aop resume RUN_ID --prompt-file F --json` |
| Token and cost accounting | normalized `usage` with `cached_input_tokens`, schema `aop-token-usage-v3` |
| Replace the system prompt, set exact tools | `--system-prompt-file`, `--tools` (Claude only) |
| No inherited instructions or tools | `--profile sealed`, which also empties MCP configuration |
| No web | `--no-web` |
| Deadlines and process cleanup | `--timeout` |
| Request recorder for inspecting traffic | `--provider-base-url` (Claude) |
| Disposable state | `aop cleanup RUN_ID` |

What AOP does not provide, and endchat needs. These are dependencies on AOP, not endchat code, and each is a change request to that project in line with its rule to prefer small native extension points:

D1, a tool bridge. The model must call `zoom` and `date` during a turn. A sealed AOP run has an empty, strict Model Context Protocol (MCP) configuration, so no tool server can be added today. Requested contract: `aop run --tool-socket NAME=PATH` where PATH is a Unix socket served by the caller outside the sandbox speaking MCP over a stream, which AOP relays into the sandbox the same way it already relays its egress proxy and registers with the harness under NAME. Claude first, Codex second. The tool surface is exactly the tools the socket lists. AOP records the socket name and tool list in `request.json` and rejects harnesses that cannot enforce it.

D2, multi-part prompts with cache marks. The cache design in the gist needs the view split into blocks of 4 lines with marks on specific blocks. AOP carries the prompt as one string. Whether this matters depends on the harness: Anthropic caching works from marks (see Prompt-cache design), OpenAI caching is automatic over any stable prefix. Requested contract: a prompt file format of ordered parts, each marked cacheable or not, passed through where the harness supports it and rejected where it does not. Phase 0 decides whether D2 is needed at all.

Until D1 exists, endchat cannot run a real turn. Compactions need neither (they call no tools), so they can start first.

## Architecture

One binary, `endchat`, with these subcommands:

| Command | Action |
| --- | --- |
| `endchat serve PROFILE` | Owns the profile: holds its lock, runs the scheduler and the tool socket, serves a control socket. Run as a systemd user service. |
| `endchat say PROFILE TEXT` | Sends a message through the control socket and streams the reply to stdout. |
| `endchat status PROFILE` | Message count, view size, unsummarized count, running calls, last error. |
| `endchat zoom PROFILE ID N` | Prints what the model would see for `zoom(ID, N)`. For humans debugging memory. |
| `endchat sim` | Runs the deterministic simulator (see Verification). |
| `endchat check PROFILE` | Verifies the log, tree, and view files against each other and exits nonzero on a mismatch. |

Crates: `tokio` for processes and sockets, `serde` and `serde_json` for records, `clap` for the command line, `rustix` for `flock`, `thiserror` for errors. No database. Modules: `log`, `tree`, `view`, `sched`, `compact`, `turn`, `aop` (process wrapper and result parsing), `bridge` (the tool socket), `cache` (accounting), `sim`.

A profile lives in `~/.endchat/PROFILE/` and is also the working directory for every `aop` call, so AOP's `.aop/runs` records for this chat sit beside its log and are found by `aop resume` and `aop cleanup`.

```text
main/YYYY-MM-DD.jsonl   messages, one JSON per line
main.idx                fixed 16-byte records, rebuilt from the log when missing or stale
tree/YYYY-MM-DD.jsonl   tree nodes
tree.idx                same, keyed by node slot
view.json               the chat view and the compaction view, with the log length they were saved at
config.toml             models, efforts, profile, limits
lock                    flock target
.aop/                   AOP records
```

## The log

Record: `{i, kind, text, size, date, run?, more?}`. `i` is the permanent id and is dense from 0. `size` is the UTF-8 byte length of `kind: text`. `run` is the AOP run id that produced the message, for audit. `more` is true on every page of a split message except the last.

Kinds: `user`, `agent`, `tool`, `echo`, `work`, `note`. Their meanings are the gist's (`agent` is the gist's `unii`).

Rules:

1. Append only, one record per write, `fsync` after each append, never edit or delete a line. A last line missing its newline is ended before the next append.
2. One owner process per profile, enforced by `flock` on `lock`. In addition, every append checks that the file size equals the size this process last wrote, and refuses with a clear error otherwise. This catches a writer that bypassed the lock; pi-optchat does this and Unii does not.
3. Thoughts are never logged.
4. Tool output is clipped to its head and tail, 30,000 characters in all, with a notice stating how many characters were cut.
5. Any other text over 30,000 characters is split into pages of at most 30,000 characters, at the last newline inside the limit when there is one, otherwise at a character boundary, and logged as consecutive messages with `more` set on all but the last. A page run is summarized and shown only once its last page is logged, so the model never acts on half a message. Unii does this; pi-optchat instead pages only at zoom time, which leaves the whole message as one compaction input.
6. Index files are derived. If an index record disagrees with its log line (offset, length, or `i`), the index is rebuilt from the log; the log is never repaired from the index.

Reading message `i` is one `pread` using `main.idx`, as in Unii, so zoom into an old message does not parse a day of logs.

## The tree

`node(0, i)` is message `i` in at most 512 bytes. `node(l, i)` is `node(l-1, 2i)` and `node(l-1, 2i+1)` merged in at most 512 bytes. A node is named `id+n` with `id = i * 2^l` and `n = 2^l`.

A node's slot is `(2i + 1) * 2^l - 1`, which is unique per node and numbers nodes in order of their last message, so `tree.idx` is a flat array. Unii uses the same slot formula.

Rules:

1. A source that fits in 512 bytes is its own node, with no model call. Two short lines are joined by a newline.
2. Each node is built once and logged, and never rebuilt.
3. A tree record is `{l, i, text, size, run, model, usage}` where `usage` is the AOP normalized usage for the call that built it, or null for free nodes. The tree therefore doubles as the cost ledger for memory.
4. A node produced by a failed or empty call is never written. Unii writes a clipped raw prefix as a permanent summary when a call fails for good; that stores a degraded line that nothing can later tell from a real summary, so endchat does not.

## The view

The view is a list of nodes that tile the whole log, oldest first. It is rendered as the gist specifies:

```text
<chat>
0+1024|...
1024+512|...
9627+1|...
</chat>
```

One line per node, newlines turned into spaces, no dates. A node not yet built renders as `(not summarized yet: zoom it)`.

### Merge order

The next merge is the pair of sibling lines `(l, i)` and `(l, i+1)`, `i` even, whose parent is built and whose due value is largest, taking the oldest pair on ties. With `T` messages in the chat and `last` the pair's last message, due is `(T - last) / 2^l`. endchat compares due values as exact rationals, `(T - last) * 2^lb` against `(T - last_b) * 2^la`, in `u128`, and never uses floating point. Both existing implementations use floating point; it is harmless at realistic sizes but needs no argument if it is absent.

A batch is a max-heap of eligible pairs keyed by due, built once over the current view, with a doubly linked list of lines. Taking a pair merges it, relinks, and offers its two new neighbor pairs. A batch costs O(n log n) in the view length. pi-optchat scans the view for the best pair on every merge, which is O(n) per merge; Unii uses a heap.

### When the view merges

1. A new message appends its line to the view and changes nothing else.
2. When the view passes 128,000 bytes, one batch merges until the view is at most 64,000 bytes.
3. A batch merges only pairs whose parent is built. If that cannot reach 64,000 bytes yet, the view enters forced mode and merges what it can after each new message until it reaches 64,000.

The view is written to `view.json` atomically (temporary file, `fsync`, rename, `fsync` of the directory) whenever it merges, with the log length it covers. At start it is loaded, not rebuilt. If the saved view does not tile the log from message 0, or names a parent that is not built, endchat replays the log through the same push, batch, and forced-mode rules to rebuild it, logs a warning that every cache entry will miss once, and continues.

### The compaction view

A second view of the same kind with a 32,000-byte high mark and a 16,000-byte low mark. It is merged down once when created, gets each new line appended, and is merged again when it passes 32,000 bytes or whenever the chat view merges. Each compaction sees the prefix of this view that ends at its node: lines before the message for a leaf, lines up to the merge's last message for a merge.

A line not built yet inside that prefix is shown as the placeholder and the lines after it are kept. The gist cuts the view at the first unbuilt line. pi-optchat reports that cutting there also hid short lines that follow it, such as an echo's own tool call, and that an A/B test on real turns scored those summaries lower (its issue #101). endchat adopts the placeholder behavior and treats that result as reported, not verified here; Phase 2 re-measures it.

## Scheduling compactions

State: for each level, an ordered set of node indexes ready to build. A leaf is ready when its message is complete (last page logged). A merge is ready when both halves are built. Work is taken from these sets and the tree is never scanned.

Rules, as in the gist:

1. At most 8 AOP calls run at once. The number is a config value with default 8.
2. A leaf starts only when fewer than 8 lines before it are still unbuilt.
3. A merge starts when both halves are built.
4. Leaves go before merges, oldest first, except that when merges are due and unbuilt leaves keep filling every worker, one worker is reserved for merges so the views can shrink. pi-optchat found this starvation during imports (its `ahead` mode); a steady message stream can cause it in chat too.

Failure: a failed call leaves the node in its ready set with a retry time of 10 seconds, then 30, then 60 and 60 thereafter, and the failure is recorded in `status`. It is retried at the next message as the gist says, and also on the timer.

## A compaction call

Each node is built by one AOP call:

```text
aop run --agent AGENT --model M --effort E --profile sealed --no-web --tools '' \
        --system-prompt-file PROMPT --prompt-file TASK --timeout 120 --json
```

`TASK` holds the compaction view as in the gist, then the task text, in this order: `<chat>` block, then `Compaction: ...` with the 512-dash ruler, then `<input>`.

The leaf and merge task texts are the gist's, verbatim, including the ruler. The retry text is the gist's "Too long" message with the first 512 bytes of the reply cut at a character boundary and `| ← LIMIT` appended. A retry is `aop resume RUN_ID` so it stays in the same conversation.

Acceptance of a reply, from pi-optchat's improvement on the gist:

1. Strip a leading `id+n|` the model may have copied from the view.
2. Accept when the reply is at most `accept_bytes` (default 640, config) and strictly smaller than its source. The size check stops a merge of two short lines from coming back as large as both.
3. Otherwise retry up to 5 attempts in total and keep the shortest reply.
4. An empty reply is an error, not a node.

The gist says a few bytes over 512 are fine because the view measures real sizes. `accept_bytes = 512` restores the strict rule.

AOP records the call under `.aop/runs/RUN_ID`. endchat runs `aop cleanup RUN_ID` after each finished compaction, because AOP keeps a private harness home per sealed run until cleanup. Disk growth from retained run records, about two per message, is an open question for AOP (see Open questions).

## A turn

```text
on user message m:
  log m (pages if long)
  wait until every message before m is summarized,
       or every node still unbuilt has failed at least 3 times, then continue with placeholders
  render the view before m
  aop run --profile TURN_PROFILE --system-prompt-file PROMPT --tools TOOLS --tool-socket zoom=SOCK
          --prompt-file [view][per-turn state][m]
  log each reply, tool call, and tool result as the events arrive
```

The view is rendered before m is logged, as in the gist and in both implementations. Per-turn state (current date, any device list) goes after the view and never in the system prompt or the tool definitions.

Nothing carries over between turns: each turn is a new AOP run, never `aop resume`. The only continuity is the view. pi-optchat adds an optional replay of the previous exchange; endchat does not, because the gist treats the view as the sole continuity and the replay adds 16 KB to a cached prefix whenever it changes.

Messages sent while a turn runs are queued. The tool bridge appends any queued messages to the next `zoom` or `date` result, which is how the gist's "reach you between tool calls" works under AOP, and logs them as `user` messages at that moment. If the turn makes no more tool calls, they start the next turn.

Events are parsed from the AOP result and its retained `events.jsonl` through one adapter per harness. Version 1 ships the Claude adapter. A turn whose events cannot be parsed is logged as one `agent` message holding AOP's final message, with a warning, so no turn is silently empty.

### Tools

`zoom(id, n)`: opens line `id+n` into the two lines of `n/2` under it. `n = 1` returns message `id` whole. `n` must be a power of 2 and `id` a multiple of `n`; otherwise the error text names the valid roots, as Unii does. A message page is `zoom(id, 1)` on a page run, and a single message over 25,000 characters returns a page with an explicit next-offset line, as pi-optchat does, so a tool-output clip never removes its middle.

`date(id)`: the date and time of message id.

The tool bridge is the only place these are implemented. Tool calls are logged as `tool` messages and their results as `echo` messages, with the 30,000-character clip.

### Turn profile and tools

The turn runs with `--profile` from `config.toml`. The recommended default is `edit`, bound to a configured repository, because AOP then isolates writes and records the effective policy. `host` is allowed only when the config says so explicitly. This is a decision for the user (see Open questions). Tools other than zoom and date are the harness's built-in set, chosen by `--tools`.

## Prompts

There is one system prompt text for turns and compactions, as in the gist, kept in `prompts/system.md` and compiled in with `include_str!`. For Claude it is passed with `--system-prompt-file`. For harnesses that cannot replace their base prompt, the same text is the first part of the prompt, ahead of the view, and is identical across calls.

The text starts from the gist's revised prompt with the agent renamed and the kinds table matching the log. The gist has no license; commenters on it raised this. Shipping its text verbatim needs the author's permission or a written adaptation, and `THIRD_PARTY_NOTICES.md` must name the source either way (see Open questions).

Only these departures from the gist's prompt are intended, each in the prompt file with a comment: the agent name, `agent` in place of `unii`, and no paragraph on computers. Anything else added to it is a bug. Unii's version also ends with a paragraph telling the compactor to be fast and spend few tokens; that is not in the gist and is not carried over until measured.

## Prompt-cache design

The gist's cache numbers (98.6% of turn prefixes read from cache, 96.2% for compactions) come from controlling cache marks directly. Under AOP, the harness owns the request, so what endchat can promise depends on the harness.

Anthropic caching stores entries only at marked block boundaries and looks back up to 20 blocks from a mark. A view sent as one text block gets no reuse beyond whatever precedes it (system prompt and tools), because any new line changes the only block. Reuse of the view needs it split into blocks of 4 lines with a mark on the last whole block, and pi-optchat also marks the blocks 20 and 40 before it so a turn with many tool calls still finds the last call's entry. That needs D2.

OpenAI-style caching is automatic over any stable prefix, so an append-only view works as one block.

Consequences, in priority order:

1. The view only grows at its end between batches, tools and system prompt never change, per-turn state sits after the view. This holds for every harness and costs nothing.
2. Compactions share a prefix with each other: the same system prompt, the same tool set (none), the same model, and views that are prefixes of one another. Eight parallel calls on one cold prefix would each pay to write it, so the first call on a cold prefix is sent alone and the others wait until it responds, as `AskAI.wait` in Unii and `primeFirst` in pi-optchat do. With AOP a call is a process, so the wait is on the AOP result stream's first event.
3. Turns and compactions do not share a cache entry by default: a cache is per model, and the default compactor is a cheaper model than the turn. The gist's statement that compactions read the turns' entry only holds when both use the same model and the same tools. `compactor.model = "turn"` with the turn's tool set is allowed by config and costs more per node; it is a measured choice, not a default.
4. Every call's `cached_input_tokens` and `input_tokens` from AOP are stored with the call. `endchat status` reports the rolling cache-read ratio for turns and for compactions separately.

A harness adds its own context to every request. With Claude, AOP notes that Claude still adds environment and account context. Phase 0 measures this floor per harness so every later number can be read against it.

## Verification

All tests use the standard Rust test harness. Tests that touch AOP's interface run against the real `aop` binary with `--dry-run` or against a recorded `request.json` and `result.json` from a real run; a fake `aop` used for speed must reject invalid flags, models, and malformed JSON the way the real one does, so it cannot pass a contract the real one fails.

1. Push equivalence. A reference implementation of Taelin's `rollback_state_list.js` push, and a test that with the view budget set to the push list's own length, endchat's merge order matches it at every step for t = 0 to 20,000. The gist states this result; the test checks it for this code.
2. Age measured from the last message. A test with the gist's counterexample: at T=10, view `0+4, 4+4, 8+1, 9+1`, the merge picks `8-9`, not `0-7`.
3. Sawtooth. Over a long simulated chat the chat view stays between 64,000 and 128,000 bytes after the first batch, grows only at its end between batches, and never merges a pair whose parent is unbuilt.
4. Log. A torn last line, a second writer, a stale index, and a crash between the log append and the index write each leave a state that `endchat check` classifies and recovers or refuses; none loses a message.
5. Long messages. A 100,000-character message becomes pages that rejoin to the original byte for byte, and its leaf is not started until the last page is logged.
6. Failure. A compaction that fails 3 times lets a waiting turn go on with placeholders, writes no node, and is retried; when it later succeeds, the node is written once.
7. Simulator. `endchat sim` replays N messages against a model of Anthropic's cache with the gist's rules (stored at marks only, 20-block lookback, 5-minute expiry), the real view and scheduler code, and a stub compactor with configurable summary sizes. It reports, per message, how many view lines are rewritten, and the cache-read ratio for turns and compactions. It reproduces the gist's reported comparison (merging at every message rewrites about 53 of 192 lines per message, batching about 2) as an acceptance test for the scheduler, and it is how a change to block size or marks is judged before it costs money.
8. Live tests behind a flag, against real AOP and a real harness on synthetic data: a 300-message chat with scripted recall questions, checking that a fact stated early is recoverable by zoom at 300, and recording per-call cached-token ratios.

## Build phases

Each phase has an exit condition and a stop limit. If the limit is hit, stop and re-read the premise before adding anything.

Phase 0, AOP probes (limit: 2 days). With `--provider-base-url` and a Claude sealed run, record the request body the harness sends. Answer: does it add cache marks, where, and what is the fixed context floor in tokens; does a long prompt split into parts reuse a prefix across two runs whose prompts share a start; does a Codex run reuse a stable prefix. Output: a one-page note with token counts, which decides whether D2 is needed and which harness is the default. Exit: numbers for the three questions.

Phase 1, memory without a model (limit: 1 week). Log, indexes, tree, view, merge order, scheduler, simulator, tests 1 to 7 with a stub compactor. Exit: all tests pass, simulator reproduces the gist's comparison.

Phase 2, compactions through AOP (limit: 1 week). The compaction call, acceptance rules, retry, cache priming, cleanup, usage ledger. Exit: a 300-message synthetic chat is fully summarized with the cache-read ratio for compactions recorded, and the placeholder-versus-cut question measured on the same chat.

Phase 3, turns (blocked on D1). The tool bridge, turn flow, event parsing for Claude, queued messages. Exit: the live recall test passes.

Phase 4, hardening. `serve` as a systemd user unit, `status`, `check`, restart recovery, then a one-week dogfood on a real chat before adding anything from the non-goals table.

## Open questions

1. Default turn profile: `edit` bound to a repository (recommended, since AOP records and enforces the boundary) or `host`. The agent in the gist works across the user's files, which fits `host` but gives up AOP's isolation. This changes what the chat can do on day one, and the user decides.
2. Default compactor: the cheapest model that passes the 300-message recall test. The gist uses Claude Haiku at xhigh effort; pi-optchat defaults to Sonnet at medium; Unii to Haiku at high. Phase 2 decides with measured cost and recall, not a default carried over.
3. AOP run-record growth. A chat of N messages makes about 2N sealed compaction runs. If `.aop/runs` growth is a problem, ask AOP for an option that keeps only `result.json` and the final message for finished runs.
4. Prompt licensing, as above.
5. Whether a harness's request has acceptable fixed overhead for a call that exists to produce 70 words. If Phase 0 shows a large floor per call, compactions may need a direct-API path, which would break goal 4 and needs an explicit decision from the user.

## Sources

| Source | Use |
| --- | --- |
| [The gist](https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449) | Algorithm, constants, prompts, test claims |
| [OptMem](https://github.com/VictorTaelin/OptMem) | Origin of the push-based merge order |
| Unii 1.0.137 | Page splitting, slot-indexed files, heap merge, first-call cache wait |
| [pi-optchat](https://github.com/jonaslsaa/pi-optchat) 0.7.2 | Durable writes and writer detection, exact acceptance rule, extra cache marks, placeholder compaction view, failure handling |
| [AOP](https://github.com/wakamex/agent-orchestration-process) | Call interface, sessions, accounting, sandbox profiles |

The comparison findings are from static reading of Unii and pi-optchat; neither was run.
