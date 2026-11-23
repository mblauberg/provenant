## Fixes
### Lanes and settlement

- Resuming settlement after a publication that merged a diverged host branch now
  converges the lane worktree on that merge and clears its import pins. Before
  the branch ref moves, publication durably records the OID it plans in the
  attempt's settlement progress; on resume the worktree advances only when the
  branch tip is that recorded OID (or the verified head), so a foreign two-parent
  merge of the head in either parent order is left alone with its pins intact.
  (#3831)
- The branch-rewound publication refusal no longer claims the lane tip descends
  from its launch base: that path checks only the host branch's position and
  never inspects the head's ancestry, so the message now states only what was
  checked. (#3831)
- A merged publication records the ancestor it established on the judged lane
  head, not one inferred from the merge's host parent. A cleanly merged unrooted
  head is recorded as unrooted in `publication-ancestor.json`, agreeing with the
  warning already written about it, instead of being mislabelled as descending
  from the lane branch. (#3831)
