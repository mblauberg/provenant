## Behaviour changes
### Accounts, quota and routing

- A stop from a spent quota, an unavailable model or an expired login now hands the work to another agent at once instead of waiting for a reset: the same model on another account, then another provider or family at the same tier, then an adjacent tier, then a host-native sub-agent, and the chair only when no route is eligible. Exclusions and enforced model policy bind every step, and a route whose readiness was not observed is never chosen. The order and tier adjacency are `domains.routing.depletion`; `routing.depletion_wait_seconds` (default 0, off) lets a reset inside that many seconds be waited for instead (#3171).
