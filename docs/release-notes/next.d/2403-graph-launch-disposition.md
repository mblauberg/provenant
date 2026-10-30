## Fixes
### Graphs

- A graph node whose launch fails now gets a persisted disposition instead of stopping for good: a transient error kind (store contention, wait expiry, home unavailability, I/O, unconfirmed launch handoff) schedules a bounded retry with backoff on the node and a `wake_at` the host tick honours, while access, confinement and invalid-input refusals mark the node blocked with the reason and a remedy in graph status; after the retry limit the node is blocked too (#2403).
