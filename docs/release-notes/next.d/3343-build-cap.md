## Behaviour changes

### Lanes and settlement

- Lane cargo builds share one host cap, `domains.fabric.lanes.max_concurrent_builds`. Unset, the count comes from cores and memory. A busy cap waits a bounded time, then warns and continues. Set the value to `0` to disable it. (#3343)
