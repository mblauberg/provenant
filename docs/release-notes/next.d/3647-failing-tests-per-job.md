## Fixes

### Pipelines and graphs

- `ci failures` now reports each failed job's failed step, failing tests with panic locations, and error lines from the job's own log as soon as that job ends, classifies cancelled and dependency-cancelled jobs separately from real failures, and labels failures in files the pull request did not touch as possibly unrelated; `pr status` and `wait --pr` point at the same digest (#3647)
