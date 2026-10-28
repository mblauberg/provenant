## Fixes

- Keep writer lane Git objects private until the host hash-verifies them at settlement. Lanes cannot alter existing shared loose objects, packs or indices; integration and delivery refuse corrupt object content.
- Drain writer lanes before upgrading: running policies before v11 retain their old shared-object write grants. The host names the legacy policy and fully checks the shared store at settlement or delivery, without force-cancelling the lane.
- Bound object imports to 64 packs and one 120-second budget, stage copying and indexing outside the shared-store publication lock, and reuse strict verification for each head and store generation within a settlement or delivery.
