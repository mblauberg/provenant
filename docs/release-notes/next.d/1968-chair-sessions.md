## Additions
### Chair, inbox and console

- Chair sessions can append retry-safe updates and corrections and read them in pages, with a per-session retention setting. GC prunes old payloads (#1968).
- Direct dispatches are bound to an explicit chair identity, which nested launches and continuations inherit (#1968).
- Chair summaries can be read and edited by field and compared across revisions with `summary diff` (#1968).
