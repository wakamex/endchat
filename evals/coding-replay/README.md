# Decision turns for paired replays of coding sessions

To compare how two context settings change what a coding agent does next, a turn is replayed from the same point under each setting and the two next steps are judged. `select_turns.py` picks the turns, the same way every time, from Claude Code session files and endchat profile logs.

## Selection

- A turn starts at each message the agent receives: a person's, another agent's, or a job notice.
- Its decision is its first tool call that changes something, and a decision turn makes it within six tool calls, so it rests mostly on what the agent already knew. Only decisions that choose what to do next are kept: selecting a task, starting a job, or starting an edit. Commits and landings finish work already chosen.
- Its reach-back is how far before the turn the agent last saw the oldest distinctive token (a path, an identifier, a hash, a long number) that it uses on the way to the decision, leaving out tokens in the turn's own message and tokens common across the session. A far reach-back marks a decision that depends on old history, which is what a smaller history risks.
- A decision that starts a job takes the job's later notice as its outcome; whether the step was right is read from what happened after it.
- Each old token the decision used is marked by what the harness showed when the turn began: shown; stubbed, where endchat showed only a stub of the result that held it; summarized, behind a compaction or cut but named in its summary; or gone. A decision that used something no longer shown had to recover it, the clearest case of what forgetting costs. Claude Code's compactions are read from its session file. endchat keeps only its last cut, so the earlier ones are rebuilt from its rules and the profile's limits (on trial 5 the rebuilt cuts end at the stored one), and its summaries are not checked.

Turns are split by reach-back (under 50,000 characters, up to 400,000, beyond) and a seeded draw takes up to four per session and band, so the same inputs and seed give the same turns.

## Use

`trials.json` names each session: a Claude Code session file or an endchat profile, and the window to draw from. `python3 select_turns.py trials.json OUT` writes `OUT/turns.json`, every decision turn with its features, and `OUT/selection.md`, the drawn turns for reading. The sessions hold private work, so selections are kept out of the repository, under `data/`.

## Limits

A shell command that does several things is labelled by the first kind it matches, so a commit that also starts a job counts as a job start. Reach-back tokens are found by pattern, so a generic one, such as a C type name, can make a decision look more dependent on old history than it is; read the drawn turns before replaying them.
