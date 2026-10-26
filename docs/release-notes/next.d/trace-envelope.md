## Additions

- `provenant output RUN/TASK/ATTEMPT --trace` prints the attempt's trace (host,
  project, run, attempt, operation, actor and parent) and then each parent's,
  one line per hop; `--json` returns a `trace` array. Attempt records, events
  and attempt facts now carry the same trace. A field the writer did not know
  is `null`, as is every field of an attempt created before this release.
  Peer-placed runs do not support `--trace` yet (#1571).
