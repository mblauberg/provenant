## Additions

- Graph CI-state gates wait for pull request or branch checks, record their evidence and fail closed when checks are unavailable or time out. Pending gates are re-evaluated on advance, resume and attempt settlement; status reads the saved snapshot.
