## Fixes
### Lanes and settlement

- A lane that ends without a final handoff now settles as `review_required` instead of `succeeded`, and `provenant output` says why: "no handoff: the lane ended without reporting results" (#2707).
- A graph repair node is skipped, not launched, when the CI check it recovers from reported no failing checks (#2707).
- Every terminal attempt record, refusals before launch included, carries a `settlement_reason` (`completed`, `handoff_missing`, `declared_stop`, `provider_stop`, `uncommitted_work`, `pre_launch_refusal`, `cancelled`, `lost`, `unsettled`, `failed`) that waiters can match on (#2464).
- `provenant dispatch` exits non-zero and names the reason when every lane is refused because its route is held back, such as by the pacing reserve (#2464).
