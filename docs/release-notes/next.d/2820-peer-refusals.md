## Fixes
### Lanes and settlement

- Peer invalid-input refusals disclose details only when accompanied by a validated typed cause.
- Launch preparation and handoff refusals retain their typed security boundary when available.
- Resource refusals retain their typed limiting setting and source in retained lane status after preparation, handoff or guardian admission, including argv prompt limits.
- Continuation copy limits are configurable under `[domains.run]`; refusals name the limiting setting, its source and a corrective action (#2820).
- Host-added prompt preambles above 1 MiB warn and continue; raw prompt admission and nested argv confinement retain their bounds.
