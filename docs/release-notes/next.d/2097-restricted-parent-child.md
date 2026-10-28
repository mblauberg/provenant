## Fixes

- Allow restricted-network parents to launch children while preserving their network restriction and stable child identity.
- Refuse child-store continuations until their prior host-sealed network can be recovered; dispatch a new child instead.
