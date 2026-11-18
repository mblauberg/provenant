## Fixes
### Lanes and settlement
- Pin review lanes to a PR with `run review-panel --pr N` or to explicit subject and base parameters. Host diffs use the merge base, including branches that merge main; launches without a subject warn about the chosen range and reason. (#3465)
