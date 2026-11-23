## Fixes
### Lanes and settlement

- Resuming settlement after a publication that merged a diverged host branch now
  converges the lane worktree on that merge and clears its import pins. The
  convergence gate recognises the lane's own merge from its parents — the
  verified head is one of them — instead of relying on the in-memory merge flag,
  which is false on the replay. A tip another writer advanced past the merge is
  not a merge of the verified head and is still left alone. (#3831)
- The branch-rewound publication refusal no longer claims the lane tip descends
  from its launch base: that path checks only the host branch's position and
  never inspects the head's ancestry, so the message now states only what was
  checked. (#3831)
- A merged publication records the ancestor it established on the judged lane
  head, not one inferred from the merge's host parent. A cleanly merged unrooted
  head is now recorded as unrooted, agreeing with the warning already written
  about it, instead of being mislabelled as descending from the lane branch.
  (#3831)
