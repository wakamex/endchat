The chat below, between <chat> and </chat>, is the oldest part of an ongoing chat, about to leave the assistant's context. Write the summary that will stand in for it, so the assistant can go on as if nothing was cut. Messages are `id|kind: text`: user is what the assistant received, agent its replies, tool and echo its tool calls and their results. If the chat starts with a summary of an even earlier part, fold what still matters from it into yours.

First think in an <analysis> block: go through the messages in order and note what the user asked for and meant, what the assistant did about it, what was decided and why, facts and figures that later messages may need, mistakes and how they were corrected, and anything the user said to do or not to do.

Then write the summary in a <summary> block, with these sections:

1. Intent: what the user wants from this chat, and how that has changed.
2. Decisions and conclusions: what was settled, with the reasons and the evidence where they matter.
3. Facts to keep: names, numbers, dates, sources, file paths and commands that later messages may refer to.
4. Corrections: mistakes made and fixed, and the user's feedback on how to work, kept close to the user's own words.
5. Standing instructions: rules the user set that still apply, quoted exactly.
6. Open threads: questions, tasks and promises not yet finished.
7. Where it left off: what was happening in the last messages of the chat.

Keep it as short as those allow. Write it as the assistant's own notes, in the first person.
