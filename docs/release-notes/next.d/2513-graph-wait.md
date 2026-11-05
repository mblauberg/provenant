## Behaviour changes
### Pipelines and graphs

- `fabric graph wait` observes a graph until it settles or the wait expires. A preset still in flight now reports `waiting` instead of `timeout` (#2513).
