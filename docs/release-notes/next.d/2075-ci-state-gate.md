## Additions
### Pipelines and graphs

- Graph CI-state gates wait for pull request or branch checks and record their evidence, failing closed when checks are unavailable or time out (#2075).
- `pr status` reports an empty check result as `checks=unavailable`, so a missing check run is never read as passing (#2075).
