## Additions
### Lanes and settlement

Graph tasks can run named commands from `[domains.fabric.commands]` through the
model-less `command` adapter, with read-only confinement, deadlines, descendant
cleanup and a bounded output tail ending in `VERDICT: PASS|FAIL` (#2119).
