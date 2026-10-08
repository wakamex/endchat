# Decisions

Choices made while implementing [SPEC.md](SPEC.md), each with the reason. They were made without asking, as instructed, and any of them can be reversed. The spec's own open questions are answered in the first section.

## Answers to the spec's open questions

1. Default turn profile is `sealed`, not `edit` or `host`. `edit` needs a git repository and creates a worktree per run, and `host` gives up AOP's isolation. `sealed` works from any directory, cannot touch files, and still lets the model read memory through `zoom`. A different profile is one line in `config.toml` (`profile`, `workdir`, `tools`). `host` is refused unless `allow_host = true`. Only `sealed` was run live. This decision changes what the chat can do on day one: with `sealed` and no tools it is a conversation with memory, not a coding agent.
2. Default compactor is Claude Haiku at medium effort. The gist uses xhigh, pi-optchat uses Sonnet at medium, Unii uses Haiku at high. At medium, the live check below recovered all 6 planted facts; no other setting was compared. Raising the effort is a config change.
3. AOP run-record growth is left as the spec describes it. endchat runs `aop cleanup` for every run of a compaction and of a sealed turn, which removes the private runtime but keeps `.aop/runs/RUN_ID`. No AOP change was requested.
4. Prompt licensing: `prompts/system.md` follows the gist's prompt and `THIRD_PARTY_NOTICES.md` names it. The repository must not be published until the author's permission is obtained or the prompt is rewritten.
5. Fixed overhead per call: a sealed Haiku call with no tools and a one-line system prompt sent 547 input tokens for a 6-token prompt, measured with a real `aop run`. That is the harness floor, small against a 16 to 32 KB compaction view, so no direct-API path was built and goal 4 holds. The live check below measured the cache effect of D2 directly.

## Decisions about AOP

1. The tool bridge (D1) does not exist, so turns call tools as text. The model writes `zoom(40, 8)`, `zoom(7, 1, 25000)`, or `date(7)` alone on a line. endchat answers each call, logs it as a `tool` message and its answer as an `echo` message, and continues the same conversation with `aop resume`. A reply without a call line ends the turn. The user asked for no new AOP features, and this needs none. The cost is one `aop resume` per round of calls instead of tool calls inside one run, and the system prompt gets one extra paragraph (see Decisions about the prompt and turns, item 1). `src/turn.rs` is the only place that knows the protocol, so a tool socket replaces it without touching memory. Harness tools (Bash, Read, and so on) still run inside the run, controlled by `turn.tools`, and their events are logged from the run's `events.jsonl`.
2. Within one turn endchat uses `aop resume`; between turns it never does. The spec forbids resuming across turns, and it does not say how to answer a tool call without D1. Each turn is a new `aop run` over a fresh view.
3. D2 (multi-part prompts with cache marks) is not requested and Phase 0's probe of the request body was not done. Compaction views that are prefixes of each other still share whatever prefix the harness caches automatically, and the ledger records the cached share of every call, so the effect is measured from the first day. `endchat sim` models the marked-block design the spec describes, and its results are the reference for deciding whether D2 is worth asking for.
4. Prompts are written to files under `PROFILE/.tmp/` and passed with `--prompt-file`, then deleted. The system prompt is written to `PROFILE/system-prompt.txt` at start and passed with `--system-prompt-file`.
5. Events of a run are read from `.aop/runs/RUN_ID/events.jsonl` after the run finishes, so a long turn shows its log entries when each `aop` invocation returns, not while it runs. A run whose events cannot be read is logged as one `agent` message holding AOP's final message, with a warning on stderr.
6. Cache priming waits for the first call's AOP result, not its first streamed event, because `aop run --json` prints once at the end. A burst on a cold prefix therefore starts its first call, waits for it to finish (or 150 seconds), and then starts the rest. A prefix counts as cold after 240 seconds without a call. In steady chat a burst of two or more is rare, because each message produces a leaf followed by merges that depend on it.
7. Usage on a tree node is the sum over every attempt of that node (the first call and its retries), so the tree is a complete ledger of what memory cost. The per-call detail is in `ledger.jsonl`, which also records failed calls and every turn call. The ledger is an addition to the spec's file list; it is the simplest place to compute the rolling cache-read ratios for turns and compactions separately.

## Decisions about the log, tree, and views

1. The index record is 16 bytes: offset (u64), line length (u32), day number (u32). Both `main.idx` and `tree.idx` use it, and a zero length means empty. The tree index is addressed by slot, so it is sparse until the tree fills in.
2. An index is trusted only after its last record matches the log line it points at. A log line that is newer than the index is indexed at start, and anything else rebuilds the index from the log. A read that finds a record whose line disagrees rebuilds once and retries.
3. The torn-line rule: a last line without its newline is ended with one before the next append. The torn line stays in the file, is skipped by readers, and is reported by `endchat check` as a torn line. It is never removed, because the log is never edited.
4. A message is complete when it is not a page with more pages to come, or when a later message exists. A page run that was cut by a crash is therefore summarized as it stands rather than blocking forever. Pages of a run are logged and registered together inside one call, so no turn can see half of one.
5. A free leaf (a source of at most 512 bytes) and a free merge (two lines that fit together in 512 bytes with a newline) are written when their inputs are built, with no usage. On start, missing free nodes are built and missing model nodes are queued, which scans the tree once and then never again.
6. Dates are formatted by a short civil-date routine in `src/util.rs` instead of a date crate, so the dependency list stays the spec's plus `toml` (config) and `tempfile` (the simulator command).
7. The view stores bytes per line and a running total. A leaf that is built after it was shown changes from the placeholder to its text and the total is corrected. Placeholder lines count their real rendered length.
8. Forced mode runs after each new message only, as the spec says, not whenever a node finishes building.
9. `view.json` is written when a view merges and at clean shutdown. After a crash the saved view is loaded and the messages logged after it are pushed through the same rules; a view that does not tile the log or names an unbuilt parent is replayed from message 0 with a warning that every cache entry will miss once.
10. When the chat view merges and the compaction view did not, the compaction view is merged down to its low mark too, as the spec says ("whenever the chat view merges").
11. The page rule for zoom: `zoom(id, 1)` returns at most 25,000 characters and ends with a line naming the call that continues it (`zoom(id, 1, 25000)`). A bad `zoom` returns an error that names the view line holding the message.

## Decisions about scheduling and compactions

1. A leaf may start when fewer than 8 unbuilt leaves precede it, counting leaves that are running or waiting out a failure, because they are still unbuilt lines in the view. This matches "fewer than 8 lines before it are still unbuilt".
2. The reserved worker: when the last free worker would go to a leaf, a ready merge takes it instead if no merge is running. With one worker there is no reservation. Merges run oldest first by their last message.
3. Failed jobs go back into their ready set with a retry time (10, 30, 60, 60 seconds), and a new message clears all retry times. A turn waits for unbuilt leaves before its message, up to 10 minutes, but goes on as soon as every such leaf has failed three times.
4. A reply is accepted at 640 bytes or less and strictly smaller than its source, as the spec says. A retry quotes the first `min(512, source - 1)` bytes, so a reply that fits 512 bytes but is not smaller than a short source still gets the gist's "Too long" message with the true limit.
5. After the fifth attempt the shortest reply stands only if it is smaller than its source and at most `hard_bytes` (default 1024). Anything longer, an empty reply, or a failed call writes no node and the job is retried later. The spec says "keep the shortest reply" and also forbids storing degraded summaries; the cap reconciles them.
6. Compactions run `--profile sealed --no-web --tools ''` with the shared system prompt file, so a compaction sees the same system prompt as a turn. The compactor model differs from the turn model by default, so the two do not share a cache entry (spec, Prompt-cache design, item 3).
7. Unbuilt lines inside a compaction view show the placeholder and the lines after them are kept, as the spec adopts from pi-optchat. The spec asks Phase 2 to re-measure this against cutting at the first unbuilt line; that A/B was not run.

## Decisions about the prompt and turns

1. The agent is named Endchat. `prompts/system.md` holds the gist's prompt with three departures, each commented in the file: the name and the `agent` kind, no paragraph on the user's computers, and one paragraph on calling tools as text (decision 1 about AOP). The paragraph is the only addition. HTML comments are removed before the prompt is used, so the model never sees them.
2. The user's own instructions come from `PROFILE/instructions.md` when it exists and follow the prompt after a blank line, as the gist describes.
3. A turn waits, renders the view, logs the user message, and only then calls the model, so the view in the prompt is the view before the message, and the message follows it after a `<state>` block holding the current date. Nothing else is sent as per-turn state.
4. Reply text containing call lines is logged without them: the `agent` message keeps the prose, and each call is its own `tool` message. A reply that is only calls logs no `agent` message.
5. Messages sent during a turn are logged as `user` messages when they are delivered with a tool result. Messages that arrive when the turn makes no more calls start the next turn, in order.
6. A turn stops after `turn.max_rounds` (40) rounds of calls and reports an error rather than looping.

## Decisions about verification

1. The push-equivalence test checks the merge order against Taelin's `push` at every step for t = 0 to 20,000, with the view budget set to the push list's length each step, as the spec says.
2. The gist's batching comparison is reproduced on the view alone, over 30,000 messages, because that is the size the gist reports it at: merging to 192 lines after every message rewrites about 51 lines per message (the gist says 53) and batching rewrites about 2. The full simulator, which includes compaction latency and the cache model, is used for cache ratios, not for that comparison, because in it parents of the newest pairs are often unbuilt and the every-message baseline is no longer the push model.
3. The simulator's cache model stores an entry at the system prompt, at the last whole block of 4 lines, and at the request end; it looks back up to 20 blocks, expires entries 5 minutes after their last read, and makes a request's entries readable 1.5 seconds after it starts. Message sizes, summary sizes, the system prompt size (6,000 bytes), and the 4-second compaction latency are stand-ins chosen to be plausible, not measurements.
4. Tests that use AOP run `tests/fake_aop.py`, which validates every `run` invocation with the real `aop run --dry-run` when `aop` is installed (a test checks that a bad `--effort` is rejected that way) and answers from a scenario file. The test double is serialized with a file lock because the workers call it in parallel.
5. The live check is `src/live.rs`, ignored by default, so `cargo test` never spends model quota.

## Live results

The live check (`src/live.rs`) generated a labeled synthetic 300-message chat with 6 facts planted at messages 6, 41, 96, 151, 221, and 286, summarized it through the real `aop` with Claude Haiku at medium effort, then asked 6 recall questions as real turns with Claude Sonnet at medium effort.

- Summarizing took 351 seconds with 8 workers and made 735 AOP calls (10.4 million input tokens, 223,000 output tokens). No node failed permanently.
- All 6 facts were recovered. Three answers needed no tool call because the summaries already held the fact; three used one `zoom`. Because the planted facts are prominent, this shows the pipeline works end to end, not how well deep recall works on subtler facts.
- Cache reads were low: 9.9% of compaction input tokens (the ledger's rolling ratio, 6.5%, covers the last 200 calls) and 35% for turns, against 96% and 98.6% in the gist. The harness caches the system prompt and, within a turn, the earlier rounds that `aop resume` continues, but not the view, because AOP sends the prompt as one block and cache entries are stored only at marks. This is the situation D2 describes. Asking AOP for ordered prompt parts with cache marks is the follow-up with the largest cost effect; `endchat sim` shows what it would recover (97.8% for turns, 92.4% for compactions in its model).

## Not done

- D1 (tool bridge) and D2 (cache marks) are not built or requested, as above.
- Phase 0's probes of the request body and of Codex prefix reuse were not run.
- The placeholder versus cut measurement, and the choice of compactor by cost and recall, were not run beyond the single live check recorded below.
- Subagents, imports, a terminal interface, multiple profiles per process, and memory text search stay out, per the spec's non-goals table.
- Nothing is pushed anywhere. The repository was initialized with `git init` and committed locally.
