## Fixes
### Graphs

- Graph launch failures retain their attempt identity and custody through retry exhaustion or refusal, and blocked launch refusals make `fabric graph run` exit non-zero with the reason. Transient launch failures, including an unobserved writer disk check, retry with backoff measured from when the failure returns; a disk-floor refusal remains permanent. Blocked-node remedies now give the supported recovery: fix the cause, cancel the graph, then rerun the document under a new `graph_id` (#2403).
