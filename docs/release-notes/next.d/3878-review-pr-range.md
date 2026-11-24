## Fixes

### Pipelines and graphs

- `provenant run review --pr N --set review_base=SHA` is accepted: the review keeps its pull-request binding and diffs `review_base...head`, so a repair re-check stays linked to its pull request. (#3878)
