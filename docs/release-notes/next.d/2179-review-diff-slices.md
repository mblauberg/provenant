## Additions

- A `--base` review now gets a `git-diff.stat` summary and one patch slice per changed file (`git-diff-slices/`, with an `index.txt`) beside the whole `git-diff.patch`. The review prompt points at the stat and the slices first, as navigation aids: reviewers still read the surrounding code, and an incomplete set of slices lists the files it lacks. `git-context.json` reports `slices`, `slices_omitted`, `slices_complete` and `stat_truncated` (#2179).
