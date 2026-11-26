## Fixes
### Chair, inbox and console
- `wait --until complete`, `pr status` and landing now share one CI completion definition. A merged or closed pull request settles the wait before a pending check, and review-gate contexts declared in the `ci_observation` port are no longer counted as CI. (architecture review F4, 26 Nov)
