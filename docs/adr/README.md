# Architecture decision records

This index lists live architectural decisions. The engineering-docs skill owns
their authoring and retirement policy. Git retains history; surviving IDs stay
stable.

| ADR | Decision |
|---|---|
| [0005](0005-lifecycle-kernel-extends-delivery.md) | One delivery kernel, separate from execution |
| [0007](0007-defer-universal-retention-and-deletion.md) | Deletion requires custody of disposable state |
| [0019](0019-installed-file-class-ownership.md) | Separate product, user and machine-local installation ownership |
| [0020](0020-retire-the-daemon-fabric.md) | Daemonless project coordination in SQLite |
| [0021](0021-configured-workspace-dispatch-boundaries.md) | Configured-workspace access and one execution owner |
| [0022](0022-thin-fabric-mcp-execution-facade.md) | Thin Fabric MCP execution façade |
| [0025](0025-broker-upstream-family-attribution.md) | Verified upstream lineage for brokered providers |
| [0026](0026-federate-fabric-across-own-hosts-over-ssh.md) | Federate the user's own hosts over SSH |
| [0027](0027-rust-front-door.md) | One Rust binary and domain-owned workspace |
| [0028](0028-rust-port-dependencies.md) | A small allowlist of major Rust dependencies |
| [0031](0031-retain-provider-history-and-expire-resume-state.md) | Retain provider history and expire resume state |
| [0032](0032-fabric-graphs-and-presets-are-the-pipeline-runtime.md) | Fabric graphs, with presets lowered into them, run pipelines |
| [0033](0033-weighted-route-pools-are-the-routing-unit.md) | Weighted route pools are the routing unit |
| [0034](0034-refusals-carry-stable-typed-codes.md) | Refusals carry stable typed codes |
| [0035](0035-four-hard-boundaries-and-flexible-everything-else.md) | Four hard boundaries, flexible everything else |

Run `scripts/check-adr-index` to check file coverage and index targets.
