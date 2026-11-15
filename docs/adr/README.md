# Architecture decision records

This index lists live architectural decisions. The engineering-docs skill owns
their authoring and retirement policy. Git retains history; surviving IDs stay
stable.

| ADR | Status | Decision |
|---|---|---|
| [0005](0005-lifecycle-kernel-extends-delivery.md) | Accepted | One delivery kernel, separate from execution |
| [0007](0007-defer-universal-retention-and-deletion.md) | Accepted | Retain durable results and collect disposable state under custody |
| [0019](0019-installed-file-class-ownership.md) | Accepted | Separate product, user and machine-local installation ownership |
| [0020](0020-retire-the-daemon-fabric.md) | Accepted | Daemonless project coordination in SQLite |
| [0021](0021-configured-workspace-dispatch-boundaries.md) | Accepted | Crate layering, one execution owner and delegation bounds |
| [0025](0025-broker-upstream-family-attribution.md) | Accepted | Verified upstream lineage for brokered providers |
| [0026](0026-federate-fabric-across-own-hosts-over-ssh.md) | Accepted | Federate the user's own hosts over SSH |
| [0028](0028-rust-port-dependencies.md) | Accepted | A small allowlist of major Rust dependencies |
| [0033](0033-weighted-route-pools-are-the-routing-unit.md) | Accepted | Weighted route pools are the routing unit |
| [0034](0034-refusals-carry-stable-typed-codes.md) | Accepted | Refusals carry stable typed codes |
| [0035](0035-four-hard-boundaries-and-flexible-everything-else.md) | Accepted | Four hard boundaries, flexible everything else |
| [0037](0037-batched-exact-candidate-landing.md) | Accepted | Gate an exact batched landing candidate |
| [0039](0039-graph-of-templates.md) | Accepted | One graph of template nodes: a plain lane is one node, composites are data, one patch path changes a graph |
| [0040](0040-open-lane-policy-with-explicit-boundaries.md) | Accepted | Ambient lane access with explicit mandatory boundaries |
