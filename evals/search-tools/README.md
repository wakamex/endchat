# Zoom compared with two search tools for reading original messages

endchat shows the model one-line summaries of the chat and lets it read the original messages through `zoom`, which opens a summary line into the lines or the message beneath it. This evaluation asks whether a search tool finds the messages that hold an answer more often than zoom does, and what each costs per turn.

With zoom switched off and a search tool in its place, the model answered 116 of 133 questions correctly with either of two search tools, against 108 with zoom. Both search tools were called on more turns than zoom and returned about twice the share of the messages that hold the answer. They cost 4 to 13 points more per turn than zoom at Claude Opus 5.5 prices.

The test used the 133 multi-session questions of [LongMemEval](https://github.com/xiaowu0162/LongMemEval), each asked once against the same imported chat of about 490 messages, with Claude Sonnet 5.5 as the turn model and the same judge for all three runs.

## The three tools

- zoom: `zoom(id, n)` opens a summary line into the two lines it was made from, and `zoom(id, 1)` gives one message whole. The model navigates by the ids shown in its view.
- search: `search("QUERY")` ranks every original message by relevance to a query in plain words and returns the 10 best whole. The ranker is [ColBERT-Zero](https://huggingface.co/lightonai/ColBERT-Zero), an embedding model that stores one vector per token, run locally with its query and document prompts.
- sch: `sch PATTERN` is a regular-expression search over the original messages, run by [search-clanker-history (sch)](https://github.com/wakamex/search-clanker-history) itself. By default it prints 200 characters around each match.

## Results

Each column is one run over the same 133 questions. Evidence means the turns LongMemEval marks as holding the answer; 125 of the 133 questions have them, and the other 8 are among the questions whose correct answer is that the chat does not say.

| Measure | zoom | search, zoom off | sch, zoom off |
|---|---|---|---|
| Questions judged correct, of 133 | 108 (81.2%) | 116 (87.2%) | 116 (87.2%) |
| Turns in which the model called the tool | 87 (65%) | 115 (86%) | 117 (88%) |
| Tool calls per turn | not counted | 2.03 | 1.35 |
| Model calls per turn | 1.85 | 1.89 | 2.07 |
| Share of evidence the tool returned, over the 125 questions | 0.40 | 0.81 | 0.78 |
| Share of evidence the tool returned, over turns that called it | 0.63 | 0.95 | 0.90 |
| Distinct messages returned per turn | 1.9 | 12.4 | 14.7 |
| Correct among turns that called the tool | 68 of 87 | 100 of 115 | 101 of 117 |
| Correct among turns that did not | 40 of 46 | 16 of 18 | 15 of 16 |

Question by question, search was right where zoom was wrong 13 times and wrong where zoom was right 5 times. For sch the counts are 12 and 4. An exact sign test on those pairs gives p = 0.10 for search and p = 0.08 for sch, so the gain in correct answers is likely but not established at this sample size. Search and sch disagree with each other on 12 questions, 6 each way.

The gain in evidence returned is large in both cases. Zoom opened 1.9 messages per turn and reached 0.40 of the evidence. The search tools returned 12 to 15 messages per turn and reached about 0.8.

## Did low zoom rate hurt results?

Only slightly. The model zoomed on 65% of turns and searched on 86% to 88%, under instructions of the same strength, but the turns where it skipped zoom are not where zoom lost. The table splits the 125 questions that have evidence by what happened on zoom's turn, and shows how many each tool answered correctly in that group.

| Zoom's turn | Questions | zoom correct | search correct | sch correct |
|---|---|---|---|---|
| Did not zoom | 46 | 40 | 43 | 41 |
| Zoomed and reached part or none of the evidence | 47 | 33 | 39 | 41 |
| Zoomed and reached all the evidence | 32 | 31 | 31 | 29 |

- On the turns without a zoom the model was right 40 times of 46 from the summaries alone. Zooming on every one of them and doing as well as search would add 3 correct answers.
- Most of the gap is in the second row. Zoom was used and missed evidence on 47 turns, which cost it 6 to 8 questions against the search tools. The model opened about 3 messages on a turn that zoomed, although the prompt tells it to open every line that could touch the subject.
- When zoom reached all the evidence it was right 97% of the time, against 93% for search and 88% for sch on their own turns that returned all the evidence. Zoom read 3.8 messages on those turns and search returned 14.5.

So zoom's shortfall is coverage on the turns that use it, and its strength is precision. Whether a stronger prompt can raise coverage depends on something this evaluation did not measure: how often the summary line covering an evidence message still shows the fact. Evidence here is usually a short user message that mentions a fact in passing, and a line that merges 4 or 8 messages may drop it. Where it does, no instruction tells the model which line to open, and wider opening costs one more call that rereads the view for each round.

## Cost per turn

Token counts are the ones the Sonnet runs reported. They are priced here at Claude Opus 5.5 rates per million tokens: $4 for input, $20 for output, and $0.20 for a cache read plus the $1 premium paid when that prefix was written to the cache. The increase is measured against one call that sends the view and nothing else, which costs $0.140.

| Measure, mean per turn | zoom | search, zoom off | sch, zoom off |
|---|---|---|---|
| Fresh input tokens | 32,800 | 38,400 | 33,400 |
| Cache-read tokens | 31,500 | 31,900 | 37,700 |
| Output tokens | 402 | 148 | 149 |
| Cost | $0.177 | $0.195 | $0.182 |
| Increase over a view-only call | 26% | 39% | 30% |

Most of every figure is the view, about 33,000 tokens sent with each call. A tool's cost is the second call that rereads the view from the cache, plus what the tool returned. Search returns whole messages, about 10,800 characters per call. sch returns snippets, with a median of 2,300 characters per call, which is why it adds little fresh input.

## How the model used each tool

Search queries averaged 37 characters and read as short descriptions of what a message would say, such as "bought vinyl album" or "antique vanity worth at least how much". The model usually split a question into two or three searches: 37 turns made two calls and 33 made three.

Run with the question itself as the single query, the same ranker puts 0.90 of the evidence in its first 10 messages. With the model's own queries it returned 0.95 on the turns that called it, because several narrower searches cover more than one broad one.

sch patterns were alternations of likely words, such as `"taxi|train|\$10|\$60"` or `"neon tetras|pleco|betta|Bubbles"`. Of 179 calls, 101 used `-t user` and 56 used `-C 0` to print whole messages. None used `-A`, `-B`, or `-t assistant`. Twelve calls matched nothing, and one broad pattern produced 150,794 characters, which endchat cuts to 30,000.

In none of the turns that skipped the tool did the reply hold a call that endchat failed to read.

## The ranker behind search, on questions it was not chosen on

ColBERT-Zero was picked for search because it returned the most evidence of nine rankers on the 133 multi-session questions, the same questions the runs above use. To check that pick, five local rankers were scored on four other LongMemEval question types, 324 questions with evidence turns, which played no part in the choice: questions about what the user said, what the assistant said, facts that changed over time, and the order or timing of events. The 30 preference questions are left out, since they ask for a recommendation and test whether a request recalls an unstated taste.

Each cell is the share of evidence turns among the first N messages a ranker returns for the question as written. Higher is better.

| Ranker | First 1 | First 2 | First 5 | First 10 | First 50 |
|---|---|---|---|---|---|
| ColBERT-Zero, with its query and document prompts | 0.511 | 0.688 | 0.871 | 0.943 | 0.986 |
| ColBERT-Zero, prompts omitted | 0.482 | 0.680 | 0.856 | 0.927 | 0.987 |
| [LateOn](https://huggingface.co/lightonai/LateOn) | 0.411 | 0.588 | 0.793 | 0.917 | 0.984 |
| [EmbeddingGemma 2](https://ai.google.dev/gemma/docs/embeddinggemma/model_card_2), text only | 0.393 | 0.581 | 0.803 | 0.903 | 0.989 |
| [DenseOn](https://huggingface.co/lightonai/DenseOn) | 0.377 | 0.550 | 0.731 | 0.866 | 0.980 |

ColBERT-Zero with its prompts leads at every size up to 10 messages, and on each of the four types separately ColBERT-Zero is ahead of the other three rankers, so the choice does not depend on the questions it was made on. sch involved no such choice and reached the same 116 correct.

ColBERT-Zero embeds the first 519 tokens of a message as shipped. Raising that setting to 8,192 leaves these rankings unchanged (0.901 and 0.903 at 10 messages on the multi-session questions) and lets a query match text anywhere in a long message: for 184 sentences taken from past token 600 of long messages, the source message ranked first 49% of the time at 519 tokens and 93% at 8,192.

## Method

Each question's chat had been imported into endchat and summarized for the zoom run. For the two search runs, a copy of each profile was cut back to the messages before the question, with the tree nodes and view lines that covered later messages removed. In 131 of 133 profiles the model saw the exact view the zoom run saw. In the other 2, the question's turn had merged view lines across the cut, and endchat rebuilt the view from the log.

The turn model was Claude Sonnet 5.5 at medium effort in every run. Answers were scored by the strict LongMemEval judge prompt with gemini-3.8-flash.

A message counts as returned when the tool printed it. For zoom that is a message opened whole with `zoom(id, 1)`. For sch it includes messages shown only as a snippet.

The search runs used a build of endchat with three additions that are not in this repository's configuration: text tools that run a configured command, a switch that turns `zoom` and `date` off, and a system prompt read from a file. The harness and both tool commands are in the longmem repository: `eval_endchat_tools.py`, `search_server.py`, `search_tool.py`, `sch_tool.py`, and `compare_tools.py`, which prints the result and cost tables. `embed_rank.py` there produces the rankings for the held-out comparison.

## Prompt changes

The zoom run used `prompts/system.md` as it stands. Each search run replaced the tool list and reworded the passages that tell the model to zoom, at the same strength, so that the tool is the only difference between runs.

Tool entry for search:

```
- search("QUERY") ranks the original messages of this chat by how well they answer QUERY and
  gives the 10 best whole, each with its id, date and kind (QUERY is plain words, such as a
  question or what the message would say; not keywords, not a regex)
```

Tool entry for sch:

```
- sch PATTERN searches the original messages of this chat (regex, case-insensitive;
  `-t user|assistant` limits it to the user's messages or yours, `-A/-B N` context messages,
  `-n N` for only the N most recent matches, `-C CHARS` text shown around each match
  (default 200, 0 for the whole message), `--since`/`--until` with ISO dates). Each result
  header gives the message's date.
```

The paragraph that requires reading originals before answering, as worded for search (the sch version says "sch calls" and "other patterns"):

```
Your first reply to such a question is search calls and
nothing else: one search for each thing the question could turn on. Answer only after
you have read the messages themselves. For a total, a count, a difference or a "how many",
there may be many separate mentions: find them all, list each in your reply with its
value, then combine. If the messages you read lack the answer, search again in other words
before you say you don't know. Before you answer, scan the view once more for subjects you
did not search that could hold another mention, and search them.
```

The note sent with every user message changed from "Zoom to the original messages before you answer anything about the chat" to "Search the original messages before you answer anything about the chat".

## Limits

- These questions share words with the messages that answer them, which suits a regular expression. A question about something half-remembered, phrased differently from the original, may suit sch less and the embedding search more. This benchmark does not separate the two.
- No run offered zoom and a search tool together, so the results say nothing about whether zoom adds anything once search exists.
- Every question asks about the chat's past. In ordinary use most turns do not, and how often the model would call each tool then was not measured.
- Output tokens come from Sonnet at medium effort. Opus 5.5 cannot turn thinking off and would produce more.
