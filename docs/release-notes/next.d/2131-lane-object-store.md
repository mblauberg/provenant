## Fixes

- Keep writer lane Git objects private until the host hash-verifies them at settlement. Lanes cannot alter existing shared loose objects, packs or indices; integration and delivery refuse corrupt object content.
- Drain writer lanes before upgrading: running policies before v11 retain their old shared-object write grants. Interrupted attempts need their private objects for verified import; otherwise the host names the recovery path and requires a fresh run.
- Receive quarantine checks new objects before promotion, with a byte cap and timeout derived from the private store. Delivery checks the lane's graph delta and reads its object payloads, retaining refusal of corrupt reachable shared content without scanning unrelated history.
