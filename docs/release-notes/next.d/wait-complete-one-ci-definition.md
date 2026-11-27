## Fixes
### Chair, inbox and console
- `wait --until complete`, `pr status` and landing now share one CI completion definition. A merged or closed pull request settles the wait before a pending check, review-gate contexts declared in the `ci_observation` port are no longer counted as CI, and an open pull request with no observed CI checks does not report a passing `0/0`. (architecture review F4, 26 Nov)
