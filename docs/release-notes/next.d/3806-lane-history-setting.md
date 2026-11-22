## Behaviour changes
### Lanes and settlement

- A lane may rebase or merge its own branch. When its history diverged from the
  host lane branch, publication now merges the host branch into the lane tip and
  publishes the merge; only a real conflict retains the private commits. Four
  sourced settings control this: `lanes.history.rebase` (`allow` by default),
  `lanes.history.unrooted_tip` (`warn`), `lanes.history.conflict_markers`
  (`warn`) and `lanes.history.default_branch_rewrite` (`refuse`, host-only owner
  gate). Every warning and refusal names the setting and its source, and
  `domains.run.push_conflict_markers` is replaced by
  `lanes.history.conflict_markers`. (#3806)
