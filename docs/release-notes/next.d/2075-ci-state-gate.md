## Additions

- Graph CI-state gates wait for pull request or branch checks, record their evidence and fail closed when checks are unavailable or time out, including after more than three clock re-anchors from host restarts or backwards monotonic steps. Pending gates are re-evaluated on advance, resume and attempt settlement; status reads the saved snapshot.
- An empty check result in `pr status` is unavailable, with null JSON counts.
