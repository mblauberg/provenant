# ADR 0010 — The lifecycle receipt authority is a trust boundary distinct from provider authority compilation

**Status:** Accepted 2026-07-15 (human, W005/W008 dependency call)

> **Current-reader note.** ADR 0020 retired the external daemon-era lifecycle
> authority and paths described below. Current delivery evidence is the local
> canonical receipt owned by `deliver`; the retained decision is that unlike
> evidence types must not be conflated merely because each is called a receipt.

## Context

"Receipt" now names three unrelated things, and the collision is manufacturing a
false circular dependency:

1. **`lifecycle_authority_receipts`** — attestation of lifecycle *transitions*
   by an external append-only ledger, with `lifecycle_receipt_batches`,
   `_batch_completions`, `_batch_authorizations`, `lifecycle_transition_applies`
   and `lifecycle_admitted_run_scopes`.

   [PR #24](https://github.com/mblauberg/provenant/pull/24) merged this schema
   and its runtime port into
   `runtime/agent-fabric/migrations/0001-current-baseline.sql` and the current
   lifecycle implementation.
2. **`AuthorityEnvelopeV2`** / the provider authority-compilation receipt —
   *provider capability compilation* (ADR 0002). [PR
   #38](https://github.com/mblauberg/provenant/pull/38) merged the V2 schema,
   parser, stored-envelope validation and provider payload compiler.
3. **"receipt portability"** — publication of *release evidence*. Unrelated to
   both.

The historical false circularity made W005 appear to need W008 because both used
the word "receipt". The merged implementations remain distinct trust
boundaries: one attests that a lifecycle transition happened; the other
compiles what a provider is permitted to do.

## Decision

The three concepts have distinct owners and are named distinctly in all new
text. Specifically, for the lifecycle receipt authority:

- It is an **external, append-only ledger, read outside the resealable lifecycle
  snapshot**. It is exposed as an optional port on `FabricRuntimeOpenOptions`
  (`lifecycleReceiptAuthority?: LifecycleIntegrityReceiptAuthorityPort`),
  following the existing `externalEffects` ports/adapters precedent in the same
  options block.

  PR #24 exposes the port on `FabricRuntimeOpenOptions`, wires it through
  `runtime/agent-fabric/src/core/fabric.ts`, and keeps the external contract in
  `runtime/agent-fabric/src/lifecycle/receipt-authority.ts`. The live
  receipt/custody persistence path is repository-backed; the retired in-memory
  lifecycle aggregate is not an authority fallback.
- It **cannot mint its own identity**. `authority_id` is FK-bound to
  `lifecycle_admitted_run_scopes`, which permits exactly one authority per
  `(project_session_id, run_id)` — the receipt chain is also hash-linked, with
  sequence 1 forbidden a predecessor and every later sequence required to name
  one. An in-process default may never forge an authority, and must not claim
  third-party attestation it does not hold.
- PR #24 landed a labelled transitional provider-action arm. PR #38 then made
  `AuthorityEnvelopeV2` the current `AuthorityInput`, validated stored V2
  envelopes and compiled provider payloads from them. Current generic
  provider-action admission still calls
  `admitUnroutedInCurrentTransaction`; this ADR therefore does not claim that
  every transitional provider-action route was removed or is unreachable.

There is therefore no circular dependency.

## Consequences

- W005 and W008 are merged through PR #24 and PR #38 respectively.
- Any remaining generic or unrouted provider-action path must be assessed from
  live reachability and authority evidence; it does not create a lifecycle
  receipt-authority dependency.
- The vocabulary collision is retired in new text; "receipt" is never used
  unqualified across these three boundaries.

## Rejected

- Serialising W005 behind W008 to "resolve" the dependency: the dependency is an
  artefact of the shared word, not of the schema.
- An in-process default that mints its own `authority_id`: the schema forbids it,
  and a self-issued attestation is not an attestation.
- Collapsing lifecycle receipts and the provider authority envelope into one
  contract: they answer different questions at different trust boundaries.
