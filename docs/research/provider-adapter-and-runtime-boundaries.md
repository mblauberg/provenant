# Provider adapter and runtime boundaries

Status: Durable research reference

Evidence snapshot: [July 2026 continuity and routing evidence](evidence-snapshots/agent-continuity-routing-2026-07.md)

Normative owners: [provider actions and adapters](../specs/agent-fabric/provider-actions-and-adapters.md),
[activation](../specs/agent-fabric/activation.md), and
[daemon and wire hardening](../specs/agent-fabric/daemon-and-wire.md)

## Conclusions

- Adapters publish versioned capability snapshots and exact effective
  configuration. Product names and previous success do not establish current
  model, effort, context, native-mode or permission capability.
- Runtime discovery and capability-fixture conformance are distinct evidence
  sources. Unsupported or unobserved values are unavailable/unknown, never
  silently inferred.
- The provider-neutral route contract is generated and shared across the
  TypeScript daemon and offline Python resolver. Capability data is explicit
  resolver input; the resolver does not read daemon activation state behind the
  caller.
- Node 24/TypeScript remains the protocol, daemon, adapter and Console stack;
  SQLite/WAL remains the one-machine transaction store; Python remains useful
  for offline evaluation. Rust is a narrow process/FD/terminal isolation or
  measured performance fallback, not an architectural rewrite.
- Host-global configuration is user-owned. Per-run overlays are minimal,
  recorded and reversible; ignored/unsupported fields stay visible. Persistent
  hooks or global default changes need explicit human authority.
- Operational spans are content-free and never substitute for authority,
  review, disclosure or artifact receipts.
- OpenCode is an optional host/adapter, not a model family or load-bearing
  route. OpenCode's current activation state is owned by
  `config/agent-fabric.yaml` and `config/adapter-compatibility.yaml`. Login,
  paid/account changes, credentials, region/data policy and any activation or
  deactivation remain explicit human gates.

## Evidence

| Evidence | Durable lesson | Boundary |
|---|---|---|
| Local TypeScript/Python routing seam | One generated request/receipt schema and explicit capability input fix ownership direction. | A language rewrite does not fix a contract leak. |
| Current provider capability differences | Actual model/effort/context/native-mode evidence must be source-labelled and may remain unknown. | Provider state is volatile and must be rediscovered. |
| OpenCode ACP and server documentation | ACP/stdio is the preferred first optional seam; loopback HTTP requires authentication and isolation. | No login, subscription or activation is authorised by research. |
| GitHub Agentic Workflows | Separate stochastic proposal from constrained privileged apply. | Does not add another workflow schema or state owner. |
| Console review-portal supervisor | Rust is justified for bounded opaque framing, FD/process/peer identity and crash custody while TypeScript owns JSON semantics. | No general Rust daemon rewrite follows. |

## Adapter admission checklist

An active adapter binds executable/package identity, wrapper closure, protocol
fixture, capability snapshot, effective configuration, exact provider/model
allowlist, permission source, cancellation/lookup semantics, actual-route
evidence and expiry. New action admission and final pre-dispatch CAS recheck the
same snapshot and contract. Ambiguity stays with the original pair-keyed
recovery owner.

When active, the OpenCode route remains optional and non-blocking. Any new
capability or future reactivation begins disabled and must prove validated ACP
framing, deny-first permissions, cancellation/timeout/duplicate/crash
behaviour, actual provider/model/fallback receipts, credential isolation and
task-local objective evaluation before advisory use. Open models do not satisfy
a mandatory other-primary gate under the current constitution.

## Unknowns

- OpenCode account, quota and fallback availability at the point of use, plus
  acceptable data region/policy for any changed route.
- Current official discovery surfaces across the enabled Claude, Codex, Agy,
  Cursor, Kiro and OpenCode routes.
- Whether a remote/multi-machine workload justifies a gateway, service
  database or workflow engine.
- Which optional lifecycle hooks are worth manifest-managed installation.
- Cross-provider comparable quota/cost attestation.

## Refresh triggers

Refresh after a provider CLI/SDK/protocol release, adapter binary/wrapper
change, capability or permission drift, observed hidden fallback, host-config
schema change, OpenCode ACP/server change, or measured SQLite/Node bottleneck.
Provider login, paid activation, persistent hooks and global configuration
changes remain explicit human decisions.
