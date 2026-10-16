# How a never-ending chat should read its own history

endchat keeps a chat that never ends by sending the model a view of one-line summaries of the whole history, and lets it open any summary into the original messages with `zoom`. This evaluation asks whether that is the right way to give the model its past: how often each design finds what a question needs, and what each costs per turn.

The short answer: letting the model search the original messages beats drilling down through summaries, a plain regular-expression search does as well as an embedding model, and the cost that decides between designs is what every turn pays for the view, not what a lookup pays. With 10 runs of each design on the same 133 questions, summaries plus a search tool answered about 114 to 115 correctly, endchat's own zoom about 109, and summaries alone about 100. Sending the whole history in the prompt scored the same as summaries plus search, at about four times the cost per turn.

Each design answered the 133 multi-session questions of [LongMemEval](https://github.com/xiaowu0162/LongMemEval), each asked against the same imported chat of about 490 messages, with Claude Sonnet 5.5 answering, through the [Agent Orchestration Process (AOP)](https://github.com/wakamex/agent-orchestration-process).

![Recall questions correct against the cost of a turn that needs recall](recall_turn.png)

![Recall questions correct against the cost of an average turn](average_turn.png)

## The designs compared

Every design except the last two keeps endchat's view, about 32,000 tokens of summaries, and differs only in how the model reaches the original messages:

- no tool: the view alone
- zoom: endchat as it ships
- sch: [search-clanker-history (sch)](https://github.com/wakamex/search-clanker-history), a regular-expression search over the raw log that prints 200 characters around each match
- search: the 10 messages ranked best for a query in plain words by [ColBERT-Zero](https://huggingface.co/lightonai/ColBERT-Zero), an embedding model run locally; with Haiku reads results, Claude Haiku 5.5 condenses those 10 messages to the passages that bear on the question
- Haiku scouts: Haiku takes the turn first with one tool (zoom, sch or search) and reports quotes, and the main model answers in a single call without tools
- sch, small view: the view cut to about 4,000 tokens by endchat's view limits
- sch, no view: no view at all, so the chat is reachable only through sch, as in a fresh coding-agent session
- whole history in context: the full chat, about 165,000 tokens, in one call, without endchat

## Results

The first column is questions correct out of 133, averaged over 10 runs (6 for the whole history), with its 95% interval. The next two are cost per turn at Claude Opus 5.5 prices: a turn that needs recall, and an average turn, of which 3.6% need recall.

| Design | Correct, of 133 | 95% interval | Cost of a recall turn | Cost of an average turn |
|---|---|---|---|---|
| search, Haiku reads results | 115.5 | 114.0 to 117.0 | $0.063 | $0.038 |
| sch | 113.9 | 112.4 to 115.4 | $0.069 | $0.038 |
| whole history in context | 113.3 | 112.0 to 114.7 | $0.154 | $0.154 |
| sch, small view | 110.5 | 108.9 to 112.0 | $0.054 | $0.016 |
| zoom | 108.8 | 107.4 to 110.2 | $0.058 | $0.038 |
| sch, no view | 108.8 | 107.4 to 110.2 | $0.064 | $0.012 |
| Haiku scouts with search | 108.8 | 107.0 to 110.6 | $0.041 | $0.038 |
| Haiku scouts with sch | 107.3 | 105.5 to 109.1 | $0.039 | $0.037 |
| Haiku scouts with zoom | 100.7 | 98.9 to 102.5 | $0.038 | $0.037 |
| no tool | 99.7 | 98.5 to 100.9 | $0.037 | $0.037 |

Paired by question, the comparisons between neighbors in score that the runs settle are: the whole history is equivalent to sch (within 3 questions either way) and better than the small view; sch without a view, zoom and the search scout are equivalent to each other; the sch scout beats the zoom scout. The Haiku reader against sch, the small view against no view, and the zoom scout against no tool remain within noise.

## Why these quantities

### Questions that need the history

The test is a question whose answer lies in earlier messages, often in several of them: "how many weddings have I attended this year". That is the one job the view and its tools exist for, so the score is the share of such questions answered correctly, judged by the strict LongMemEval prompt with gemini-3.8-flash. Questions that need only the current message would score every design the same.

### Evidence found, alongside answers

LongMemEval marks the messages that hold each answer. Counting how many of them a tool returned separates two failures that a wrong answer conflates: the tool never showed the evidence, or the model had it and still answered wrongly. It also needs no judge.

### Cost per turn, with the prompt cache as it is really used

Cost is in dollars per turn, priced at Opus 5.5 rates, the model endchat would run in daily use. Token counts come from the Sonnet runs, so the figures assume Opus would read and write about as many tokens. Every call goes through the one-hour [prompt cache](https://docs.claude.com/en/docs/build-with-claude/prompt-caching): input not yet cached is written at twice the input price and read back later at a twentieth of it. Whether a turn finds the view cached decides most of its cost, so each turn is priced at the measured share of my own prompts that found the cache warm, 92% (91% to 93%).

### An average turn, not only a recall turn

Each benchmark question is a turn that needs the history, but in real use most turns do not. From 30 days of my Claude Code and Codex logs, 3.6% of turns (3.2% to 4.1%) ran sch or read a past session's log. The second chart prices an average turn: 3.6% of turns cost what the design's recall runs cost, and the rest cost one call over the view with no tool. That assumes no design calls its tool on an ordinary turn, which these runs do not test.

### Ten runs per design

The same design on the same questions gives a different score each run: two runs disagree on 7 to 20 of the 133 questions, and the judge alone flips about 3. One run per design cannot separate designs a few questions apart, so each design ran 10 times (the whole history stopped at 6, once its comparison with each neighbor was settled). Intervals come from resampling a design's runs separately for each question, which measures how well its average on these questions is pinned down.

### Clouds and the frontier

Each chart draws every design as 3,000 bootstrap draws of its accuracy and cost, with cost drawn over its runs, the warm share and, on the second chart, the recall rate. The figure beside each name is the share of draws in which the design is on the efficient frontier, where no other design is cheaper and at least as accurate. It says how often a design is worth choosing given the uncertainty, which a single frontier line cannot.

### Reworded questions

The benchmark's questions repeat the words of their evidence, which suits a regular expression. A second set of the same 125 questions, rewritten by Claude Sonnet to share no content word with the messages that answer them, tests whether sch survives a question asked in other words. On it the model answers 110 correctly when handed the evidence directly, against 116 for the originals, so the rewording costs about 6 questions before any tool is involved.

### Ranker chosen on other questions

ColBERT-Zero was the best of nine rankers at returning evidence on these 133 questions. On four other LongMemEval question types, 324 questions not used to choose it, it was still the best, by 7 to 10 points at 5 messages.

## Takeaways

### Searching the raw messages beats drilling through summaries

A summary line keeps a topic and drops what the summarizer thought minor, so zoom can only open what a line points to. On single runs, zoom reached 0.40 of the evidence messages and the search tools about 0.8. When zoom did reach everything it was the most precise design, right 97% of the time on about 4 messages, but it reached everything on 32 of 125 questions. Offered both, the model almost never zooms after searching.

### A regular expression matches an embedding model here

sch and ColBERT search score the same. On reworded questions, sch with the view scored 106.6 and the view-free version 103.4: the model reads the chat's own vocabulary off the view and writes its patterns from that, so the question's wording barely reaches the tool. With the reworded question itself as the query, ColBERT still ranks 0.71 of the evidence in its top 10 and a plain keyword match ranks none. The embedding model needs a GPU and an index that grows with the history; sch needs neither.

### What the tool returns matters more than how it finds it

Whole messages returned by search averaged 10,800 characters a call, mostly long assistant replies around a fact mentioned in passing by the user. Having Haiku condense them to quotes cut that to about 450 characters and kept the score. Cutting each long message to the sentences that best match the query kept only 41% of the answer's words, because the answer usually sits in the sentence beside the match; adding one sentence on each side kept 90%.

### Delegating the lookup to a cheaper model loses accuracy

A Haiku scout saves the main model's second call but passes on only what it chose to quote, and scores about 6 questions below the designs where the main model searches. With zoom as its tool the scout barely beats no tool. Once recall turns are rare, its saving disappears and its Haiku call on every turn remains.

### The view is the cost; lookups are nearly free

Every design with the full view pays for about 32,000 tokens on every turn. At a 3.6% recall rate the lookup's own cost shrinks to a tenth of a cent, and all full-view designs cost about $0.038 per average turn. The one lever left is the size of the view: cutting it to about 4,000 tokens costs about 3 questions and brings an average turn under half the cost, and dropping it costs about 5 questions and leaves a turn at about a third of the cost.

### The whole history in context is no better and costs four times as much

At about 165,000 tokens these chats fit easily in the context window, and sending all of it scored the same as summaries plus sch. Even with the cache warm, rereading the whole history on every turn costs about four times as much as a 32,000-token view, and the gap grows with the history until it no longer fits.

## Limits

- The view also carries the recent conversation, which ordinary turns depend on ("make it shorter", "do the same for the other file"). The benchmark scores only recall, so what the small view and no view lose on ordinary turns is not measured. The per-turn chart assumes nothing.
- Each chat is about 490 messages of synthetic conversation. Longer histories favor designs that do not send everything, and real chats differ in how often facts recur.
- The recall rate comes from one person's coding-agent use, where the current session stays in context, and may not match a chat that never ends.
- Answers come from Sonnet and costs from Opus prices; Opus would likely think longer and score somewhat higher.

## Reproducing

endchat's experimental options for these runs (text tools, switching zoom or the view off, a replaceable system prompt) are in `src/profile.rs`, `src/turn.rs` and `src/engine.rs`. The harness is in the longmem repository: `eval_endchat_tools.py` runs an arm, `replicate_arms.py` repeats it and `replicates.py` compares arms, `plot_tool_arms_cloud.py` draws the charts, `warm_share.py` and `recall_rate.py` measure the warm share and the recall rate from session logs, and `reword_questions.py` builds the reworded set. Prompts for every arm are under `prompts/` there.
