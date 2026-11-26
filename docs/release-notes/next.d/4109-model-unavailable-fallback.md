## Fixes
### Routing
- `model_unavailable` keeps its typed identity through settlement and is treated as a route-level depletion: the run falls back to the next eligible candidate in the same tier, with the hand-off's rung, target and tier recorded on the restart. It is no longer collapsed into an adapter failure, which stranded a lens pinned to an exact model. (#4109)
- An exact model pin with no pool selection now derives a stand-in tier from its family and the configured `depletion.tiers`, so a capacity stop can hand off instead of going straight to the chair. (#4109)
- A pinned route with no fallback now names cause-neutral options: another eligible entry in `node.routes`, an applicable depletion ladder, or removing the route's account/model pin. (#4109)
