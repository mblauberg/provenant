## Behaviour changes
### Lanes and settlement

- Writer setup now holds the repository lock only for upstream configuration writes and releases it before provider launch. Overlapping origin refresh requests share one fetch. The arrival-order queue and `domains.fabric.lanes.setup_lock_wait_seconds` setting have been removed (#2964).
