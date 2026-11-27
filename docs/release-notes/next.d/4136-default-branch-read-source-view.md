## Fixes
### Pipelines and graphs

- A read-only lane launched with `--source default_branch` now reads an attempt-private detached view of the commit its record pins, including under the default `host` access. Previously the recorded pin could name the fetched default while the lane still read the live checkout. The tree a pinned reader gets no longer depends on an access grant flag, and `lanes.read_source` reports a commit for every choice except `live` (#4136).
