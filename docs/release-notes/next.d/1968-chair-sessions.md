## Additions
### Chair, inbox and console

- Chair sessions can append updates and corrections that are safe to retry, and read them in bounded pages. Three configurable payload limits and a per-session retention setting prune old payloads, and GC removes excess payloads from idle sessions. A stale retry is refused once its receipt expires (#1968).
- Direct dispatches are bound to an explicit chair Fabric identity, which nested launches and continuations inherit. Runs with no chair binding, peer executions and graph-created runs stay unbound (#1968).
- Chair summaries can be read and edited by field, guarded by revision, and compared across two retained revisions with `summary diff`. Summary requests are checked against the local inbox (#1968).
