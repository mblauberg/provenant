## Additions

- Graph `loop` nodes repeat a bounded body until its gate passes, carry outputs between rounds and keep writers on the same branch and worktree. Graph status shows each round and its outputs. (#2007)
- Exhausted loops pause for stop, extension or acceptance through explicit resume options, with an inbox decision when an originating session is configured. Saved rounds and decisions survive replay.
- Extensions default to at most five extra rounds per decision and must fit effective host limits before they are saved. The three-node, two-round runbook example needs host depth 6; extending it to four rounds needs depth and ceiling 12.
