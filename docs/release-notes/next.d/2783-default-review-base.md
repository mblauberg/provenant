## Fixes
### Pipelines and graphs

- Sourced review nodes with no configured base continue to launch in repositories without `origin/HEAD`. Reviews without an integration base retain the empty-range warning; set the preset’s `base` to compare changes (#2783).
