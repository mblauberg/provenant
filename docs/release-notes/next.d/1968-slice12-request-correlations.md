## Additions

- Add optional claimed run/task/attempt, repository and launch-head links to CLI/MCP inbox requests, with host-only coverage in records and receipts. Only verified coverage permits work navigation. Contradictions are refused even for run-only selectors; index waits happen outside Fabric write transactions. Unlinked version-1 records retain their wire shape; correlated records declare version 2. Part of #1968.
- Replace a viewed request atomically while retaining saved answers and delivery. Explicit future UTC deadlines report unresolved requests as expired with a reason on read and reject late answers, without a timer service or inferred approval. Part of #1968.
