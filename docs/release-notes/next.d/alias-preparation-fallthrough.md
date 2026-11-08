## Fixes
### Accounts, quota and routing

- A named route such as `senior` or `junior` no longer stops when local preparation excludes its first candidate. `run` tries the route's next permitted member, whatever the exclusion, and `skipped_routes` lists every excluded candidate with its cause. A candidate is reported `disclosure_denied` only when the disclosure policy refuses it; a missing confinement plan reports `confinement_unavailable` with its guard. `domains.routing.settlement_retries` bounds launched attempts, not candidates that never launched.
