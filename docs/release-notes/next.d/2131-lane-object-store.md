## Fixes

- Keep writer lane Git objects private until the host hash-verifies them at settlement. Lanes cannot alter existing shared loose objects, packs or indices; integration and delivery refuse corrupt object content.
- Drain writer lanes before upgrading: running policies before v11 retain their old shared-object write grants. The host names the legacy policy, run and attempt at settlement or delivery and records the settlement warning in run events, without force-cancelling the lane. Staged-file identity checks refuse changes made by an older lane before publication.
- Bound copying and indexing to 64 packs and 120 seconds, with separate verification deadlines that scale with store size up to one hour per phase. Publication uses a connectivity check and verifies newly published files instead of rehashing the whole store. Lock-wait timeouts leave settlement retryable, and stale import cleanup cannot block settlement.
