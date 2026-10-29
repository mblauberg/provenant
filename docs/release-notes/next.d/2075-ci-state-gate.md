## Additions

- Graph CI-state gates wait for pull request or branch checks and record their evidence. They fail closed when checks are unavailable or time out, including after more than three clock re-anchors from host restarts or backwards monotonic steps. Pending gates are re-evaluated on advance, resume and attempt settlement, and `status` reads the saved snapshot (#2075).
- `pr status` reports an empty check result as `checks=unavailable`, with null counts in `--json`, instead of zero checks, so a missing check run is never read as passing (#2075).
