# Third-party notices

## UniiChat design and system prompt

The memory design (the summary tree, the due-order merge, the 64 to 128 KB batched view, the compaction tasks) and the text of `prompts/system.md` come from [Victor Taelin's UniiChat recipe](https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449). The gist carries no license. `prompts/system.md` follows its section 5 with four marked departures. It is published here with credit to the author and is not covered by this repository's MIT license.

## Taelin's rollback push

The merge order is checked against `push` from `rollback_state_list.js`, as reproduced in the gist and in [OptMem](https://github.com/VictorTaelin/OptMem). The check is a test in `src/view.rs`; no code from either project is included.
