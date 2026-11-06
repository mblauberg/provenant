## Fixes
### Lanes and settlement

- Peer invalid-input refusals disclose details only when accompanied by a validated typed cause.
- Launch preparation and handoff refusals retain their typed security boundary when available.
- Resource refusals retain their typed limiting setting and source during dispatch and handoff, including argv prompt limits.
- Continuation copy limits are configurable under `[domains.run]`; refusals name the limiting setting, its source and a corrective action (#2820).
