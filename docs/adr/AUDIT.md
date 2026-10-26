# ADR audit for #1754

Temporary review evidence: the chair moves this table into the PR and deletes
this file. Architecture means module/crate boundaries, trust boundaries,
protocols, storage or major dependencies. Git preserves removed records.

| ADR | Verdict | Reason | Destination |
|---|---|---|---|
| 0001 | Move | Product posture and local-history process, not architecture. | [#1754](https://github.com/mblauberg/provenant/issues/1754) closing note; [HARNESS memory section](../../HARNESS.md#risk-authority-routing-and-memory) |
| 0002 | Delete | Retired daemon capability compiler has no live owner. | Git history; current boundary in 0020 |
| 0003 | Merge | Keep modular composition, remove retired daemon extraction plan. | 0027 |
| 0004 | Move | Source-owner and projection authoring procedure. | [engineering-docs binding rules](../../skills/engineering-docs/SKILL.md#binding-rules) |
| 0005 | Keep | Workflow owns delivery minima separately from execution. | Amended in place |
| 0006 | Move | Superseded backlog deferral and repository workflow call. | [#156](https://github.com/mblauberg/provenant/issues/156) closing note; existing [repository process](../../MAINTAINING.md#repository-process) |
| 0007 | Keep | Current deletion custody and disposable-storage boundaries. | Amended in place; deferral in #1754 closing note |
| 0008 | Move | Review and communication procedure already governed by constitution. | [HARNESS review ladder](../../HARNESS.md#review-ladder) and [evidence](../../HARNESS.md#risk-authority-routing-and-memory) |
| 0009 | Move | Specification placement and integrity procedure. | [engineering-docs placement policy](../../skills/engineering-docs/SKILL.md#binding-rules) |
| 0010 | Merge | Preserve evidence trust distinction; remove retired external ledger. | 0005 |
| 0011 | Move | Repository tracker choice and work-state authoring procedure. | Existing [repository process](../../MAINTAINING.md#repository-process); [engineering-docs](../../skills/engineering-docs/SKILL.md); [#156](https://github.com/mblauberg/provenant/issues/156)/[#711](https://github.com/mblauberg/provenant/issues/711) closing note |
| 0012 | Move | Retired Node/Bun product deferral. | [#198](https://github.com/mblauberg/provenant/issues/198), [#199](https://github.com/mblauberg/provenant/issues/199), [#200](https://github.com/mblauberg/provenant/issues/200), [#201](https://github.com/mblauberg/provenant/issues/201) closing note |
| 0013 | Keep | Thin CLI composition boundary. | Amended in place |
| 0014 | Keep | Explicit task-directed exception for the #1761 in-place process amendment. | Amended in place; new process belongs in skills |
| 0015 | Delete | Retired daemon bootstrap completion protocol. | Git history; current boundary in 0020 |
| 0016 | Move | Retired test/product choice, not live architecture. | [#354](https://github.com/mblauberg/provenant/issues/354) closing note |
| 0017 | Move | Specification content and projection procedure. | [engineering-docs binding rules](../../skills/engineering-docs/SKILL.md#binding-rules), which load placement depth |
| 0018 | Delete | Retired review-portal launch protocol. | Git history; current boundary in 0020 |
| 0019 | Keep | Product/user/machine-local installation ownership. | Amended in place |
| 0020 | Keep | Daemonless SQLite coordination and same-user trust model. | Amended in place |
| 0021 | Keep | Configured access and execution/assurance boundaries. | Amended in place |
| 0022 | Keep | MCP transport delegates to the run owner. | Amended in place |
| 0023 | Merge | Retired Python endpoint modes; retain clean launch-config boundary. | 0021; current providers/src/codex.rs |
| 0024 | Move | Change-history and living-record procedure. | [engineering-docs living ADRs](../../skills/engineering-docs/SKILL.md#living-adrs) and its placement depth |
| 0025 | Keep | Upstream lineage is distinct from broker identity. | Amended in place |
| 0026 | Keep | SSH federation and single-host authority. | Amended in place |
| 0027 | Keep | One Rust binary and domain-owned crate composition. | Amended in place |
| 0028 | Keep | Major dependencies and deliberate in-house primitives. | Amended in place |
| 0029 | Merge | Keep sole Rust runtime and fresh-state boundary, remove one-off rollout. | 0027 |
| 0030 | Move | Unadopted trials and product deferral. | [#1452](https://github.com/mblauberg/provenant/issues/1452) closing note |
| 0031 | Keep | Provider-history retention versus resumable state. | Amended in place |

Closing notes are supplied in the lane's final report for the chair to post;
this audit does not claim that an issue comment has already been sent.
