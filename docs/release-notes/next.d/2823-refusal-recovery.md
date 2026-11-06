## Fixes
### Lanes and settlement

- Dirty-writer refusals before launch are retained for status and operation replay, with one path-free notice to the bound chair when enabled (#2823).
- Recovery retains a slot-root custody fence until its carried attempt record is confirmed durable, and reports cutover storage errors while checking other work (#2823).
