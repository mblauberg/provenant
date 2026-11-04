## Behaviour changes

- Review doctrine: a later commit no longer voids an existing review. Agents re-review only the new delta, such as a repair or a conflict resolution, and a clean merge of the base needs none. CI must still be green on the head that merges. The pull request template and the `setup-repo` runbook seed say the same.
