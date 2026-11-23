## Fixes
### OpenCode stream

- OpenCode lanes record per-step token usage and provider-stated cost, end on
  endless provider retries at the adapter's bound instead of the wall timeout,
  and treat upstream 5xx as capacity rather than a generic error. (#3706 #3708 #3709)
