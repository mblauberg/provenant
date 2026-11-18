## Behaviour changes

### CLI and install

- `wait --pr` absorbs the useful parts of the retired command. At a timeout it
  now prints the last known readable check summary (head, check counts, failing
  names, merge and review) and, when no check was ever reported, the cause and
  next step: check the workflow triggers or push an empty commit to start CI.
  Its budget is `--timeout` or `domains.wait.timeout_seconds`, clamped to
  `domains.wait.max_timeout_seconds`, with the source shown; the fixed 24-hour
  `pr watch` budget is deliberately dropped rather than copied into another
  hard-coded path. (#3528)
- Head pinning and end-of-wait head revalidation (#3373), the merge-conflict
  diagnosis of missing checks (#3492) and the wait final-line format (#3437)
  are deliberately not moved into `wait --pr`; they belong to their own lanes
  in this batch. (#3528)
