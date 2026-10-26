# Architecture decision records

This index lists live architectural decisions. The engineering-docs skill owns
their authoring and retirement policy. Git retains history; surviving IDs stay
stable.

| ADR | Decision |
|---|---|
| [0005](0005-lifecycle-kernel-extends-delivery.md) | One delivery kernel, separate from execution |
| [0007](0007-defer-universal-retention-and-deletion.md) | Deletion requires custody of disposable state |
| [0013](0013-thin-provenant-cli.md) | Thin CLI composition edge |
| [0014](0014-comparative-skill-evals-on-suspicion.md) | Comparative skill evals on behaviour change or suspicion |
| [0019](0019-installed-file-class-ownership.md) | Product, user and machine-local installation ownership |
| [0020](0020-retire-the-daemon-fabric.md) | Daemonless project coordination in SQLite |
| [0021](0021-configured-workspace-dispatch-boundaries.md) | Configured-workspace access and one execution owner |
| [0022](0022-thin-fabric-mcp-execution-facade.md) | Thin Fabric MCP execution façade |
| [0025](0025-broker-upstream-family-attribution.md) | Verified upstream lineage for brokered providers |
| [0026](0026-federate-fabric-across-own-hosts-over-ssh.md) | Federate the user's own hosts over SSH |
| [0027](0027-rust-front-door.md) | One Rust binary and domain-owned workspace |
| [0028](0028-rust-port-dependencies.md) | A small allowlist of major Rust dependencies |
| [0031](0031-retain-provider-history-and-expire-resume-state.md) | Retain provider history and expire resume state |

ADR 0014 is the explicit process-record exception required by #1761.
Temporary [audit evidence](AUDIT.md) moves to the PR body before landing;
remove this paragraph with that file.

Run `scripts/check-adr-index` to check file coverage and index targets.
