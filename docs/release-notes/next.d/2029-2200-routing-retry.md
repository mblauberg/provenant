## Behaviour changes
### Accounts, quota and routing

- Provider capacity errors from Codex, Claude, OpenCode and Agy now settle as `model_unavailable`, and a spent Claude or Codex account settles as `quota_limited`. Neither is a generic failure any more. A Claude 429 that names no usage limit or reset counts as transient capacity. A capacity stop resumes the same session after a short, jittered backoff, or redraws the route when there is no session. A quota stop retries the same model at once on another account, provided one is registered, unpinned and not spent. `domains.routing.settlement_retries` (default 2) and `domains.routing.capacity_retry_seconds` (default 30) control this (#2029).
- Account selection skips an account whose latest reading is exhausted, whatever its pacing policy, and falls through to an available account for the same model when admission refuses the first one (#2200).
- `route pacing` and `route pacing override` now confirm what they recorded. `route pacing override` with no value lists the active overrides (#2200).
