## Additions
### Pipelines and graphs

- Graph CI-state gates wait for pull request or branch checks and record their evidence. They fail closed when checks are unavailable or time out, and `status` reads the saved snapshot (#2075).
- `pr status` reports an empty check result as `checks=unavailable`, with null counts in `--json`, so a missing check run is never read as passing (#2075).
