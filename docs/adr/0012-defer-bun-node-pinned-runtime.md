# ADR 0012 — Defer Bun; Node stays the pinned runtime family

**Status:** Accepted 2026-07-16 (dual-family runtime evaluation; follow-ups
[#198](https://github.com/mblauberg/provenant/issues/198),
[#199](https://github.com/mblauberg/provenant/issues/199),
[#200](https://github.com/mblauberg/provenant/issues/200),
[#201](https://github.com/mblauberg/provenant/issues/201))

> **Current-reader note.** The daemon and its native-addon analysis are
> historical after ADR 0020. The current Node workspace and lockfile owners are
> `runtime/fabric/package.json` and the root lockfile. Provider CLI versions and
> digests remain diagnostics, not admission gates.

## Context

Bun promises faster installs, faster TypeScript startup and a bundled
toolchain. A dual-family evaluation on 2026-07-16 examined replacing or
augmenting the Node runtime for the fabric workspaces and concluded defer.

Three findings carried the decision:

1. **The adapter provenance trust boundary is proven around Node.** The
   compatibility registry (`runtime/agent-fabric/src/adapters/compatibility.ts`)
   verifies wrapper Git provenance against Node+tsx execution semantics, while
   `fabric_node >=24.15.0 <25` and npm-lockfile integrity bind the
   runtime/dependency closure. Swapping the runtime family invalidates that
   proof, not just a version number.
2. **Native addons sit on correctness-critical paths.** `fs-ext` `flock`
   backs daemon bootstrap election, and `better-sqlite3` backs every store.
   Both are Node-ABI native modules whose failure modes under a different
   runtime are exactly where fail-closed guarantees live.
3. **The measured benefit is narrow.** Production runs compiled JS through a
   persistent daemon with cached transports, so Bun's advantage is limited to
   clean-install and first-source-start latency. The user-visible CLI
   cold-start cost was instead fixed on Node: lazy subcommand imports plus
   the `agent-fabric-warm` helper (PR #202, roughly 3.4x faster).

## Decision

Defer Bun. Node remains the supported runtime family for all fabric workspaces,
with npm-lockfile integrity and runtime adapter conformance as the verification
boundary. Provider CLI versions and digests are not compatibility gates.
Revisit only with a concrete requirement that the Node-based fixes cannot meet,
carried through a fresh compatibility proof.

Alternatives rejected for now:

- **Bun as package manager only** — splits install provenance from runtime
  provenance for little retained benefit.
- **npm to pnpm** — same split, different tool; the lockfile integrity pin
  would need re-proving.
- **tsx to `node --strip-types`** — plausible simplification inside the Node
  family, but deferred until a separately scoped equivalence experiment is
  justified.

## Consequences

- `fabric_node` and lockfile-integrity pins stay authoritative; no
  Bun-conditional paths are added.
- The linked #198–#201 follow-ups preserve the delivery-time investigation;
  GitHub issues and Project Status, not this ADR, own their terminal state.
- A future runtime-family proposal needs a separately approved scope and a
  new provenance/compatibility proof before any swap.
