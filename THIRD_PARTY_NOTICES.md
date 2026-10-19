# Third-party notices

## UniiChat design and system prompt

The memory design (the summary tree, the due-order merge, the 64 to 128 KB batched view, the compaction tasks) and the text of `prompts/system.md` come from [Victor Taelin's UniiChat recipe](https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449), whose author has since published [UniiChat](https://github.com/VictorTaelin/UniiChat), including that prompt as `prompt.txt`, under the [Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0) (Copyright 2026 Higher Order Company). `prompts/system.md` follows the gist's section 5 with four departures, each marked in the file. It is covered by the Apache License 2.0, whose text is in `prompts/LICENSE-UniiChat`, not by this repository's MIT license. No UniiChat code is included.

## Taelin's rollback push

The merge order is checked against `push` from `rollback_state_list.js`, as reproduced in the gist and in [OptMem](https://github.com/VictorTaelin/OptMem). The check is a test in `src/view.rs`; no code from either project is included.
