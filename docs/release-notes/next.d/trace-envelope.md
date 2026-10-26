## Additions

- `provenant output RUN/TASK/ATTEMPT --trace` prints the attempt's trace (host,
  project, run, attempt, operation, actor and parent) and then each parent's,
  one line per hop, however long the chain; `--json` returns a `trace` array.
  A chain whose parents loop fails instead of printing. Attempt records, events
  and attempt facts now carry the same trace. A field the writer did not know
  is `null`. Older records show no trace. Peer-placed runs do not support
  `--trace` yet (#1571).
