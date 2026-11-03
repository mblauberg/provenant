## Fixes
### Pipelines and graphs

- Generator-evaluator reviews receive the changes from the configured base to the implement commit instead of an empty host diff. Sourced review nodes with no configured base still launch in repositories without `origin/HEAD`, and any review whose range is empty warns; set the preset's `base` to compare changes (#2783).
