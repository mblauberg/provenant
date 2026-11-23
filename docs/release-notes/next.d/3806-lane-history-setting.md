## Behaviour changes
### Lanes and settlement

- A lane may rebase or merge its own branch. When its history diverged from the
  host lane branch, publication now merges the host branch into the lane tip and
  publishes the merge. A real conflict, unrelated histories, a configured
  `merge.*.driver` or a branch rewound behind its launch base retains the
  private commits with a typed cause, as does `lanes.history.rebase = "refuse"`.
  The unrooted decision judges the lane head before any merge, so a host merge
  cannot supply ancestry the head lacks; its `warn` and `off` values never
  override an unrelated-history refusal, and a branch rewound behind its launch
  base is judged before the unrooted tip. Four sourced settings control this:
  `lanes.history.rebase` (`allow` by default), `lanes.history.unrooted_tip`
  (`warn`), `lanes.history.conflict_markers` (`warn`) and
  `lanes.history.default_branch_rewrite` (`refuse`, host-only owner gate). Every
  warning and refusal names the setting and its source, an unrecognised value
  warns with its layer and keeps the documented default, and
  `domains.run.push_conflict_markers` is replaced by
  `lanes.history.conflict_markers`. (#3806)
