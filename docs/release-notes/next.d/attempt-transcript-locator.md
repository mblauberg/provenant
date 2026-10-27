## Additions

- Each attempt now points at its provider transcript. `provenant lanes --json`
  gains a `transcript` object (`adapter`, `session` and, when the retained
  provider state holds the file, its `path`), and telemetry attempt records
  carry the adapter and session. `provenant output <attempt> --part transcript
  --tail N` reads the redacted, bounded tail in place. Provenant stores a
  pointer, never a copy; retention stays with the provider.
