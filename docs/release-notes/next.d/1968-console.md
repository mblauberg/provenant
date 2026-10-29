## Additions

- `console projects` and `console sessions` list local projects and registered chair sessions. A session without a summary reports it as unavailable. Discovery examines up to 16,384 entries across projects and sets `scan_truncated` when it reaches that limit. A result over 1 MiB is refused (#1968).
- `console overview` shows pending request titles and lane attention for the local host, with project and task labels and a report of which sources were read. Inbox rows include the addressed chair session (#1968).
- `console board` lists local runs and attempts with their metadata and usage coverage, 50 rows per page by default and 100 at most, with filters and cursor paging (#1968).
- `console tree` shows nested runs on the local host, with separate dispatch and continuation edges, deduplicated usage and any coverage gaps (#1968).
- `console board` and `console tree` take `--hosts` to add peer observations. Local results are kept, and the output reports peers that are incomplete, unreachable or do not support the read (#1968).
- The menu's console overview refreshes when its records change. If a refresh fails, the last successful view stays (#1968).
- Each viewer keeps its own request presentation state and last-rendered summary revisions, without resolving or unblocking requests (#1968).
