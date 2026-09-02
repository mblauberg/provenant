# ADR 0002 — Capability-compiled execution authority (write profiles)

**Status:** Superseded 2026-08-02 by [ADR 0020](0020-retire-the-daemon-fabric.md);
originally architecture accepted 2026-07-13. The compiled write profiles had no
runtime left to enforce them once the daemon was retired.

## Context

Fabric-managed headless provider sessions are compiled read-only, enforced
twice (both provider adapters and `Fabric.#admitProviderPayload`). This is
correct for certifying review but blocks Fabric from serving as the managed
implementation plane. The standalone authority,
workspace-containment and
provider-action
specifications preserve the read-only posture, so enabling writes requires a
normative specification change as well as code.

## Decision

Adopt provider-neutral authority profiles compiled by Fabric into
provider-native settings in four stages:

1. **Authority contract:** protocol-owned, versioned `AuthorityEnvelopeV2`
   carrying the full human-approved envelope (approval binding with evidence
   digest; secrets, deployment, irreversible-action and network dimensions —
   all missing from Fabric's former `AuthorityInput`), plus exact
   characterisation goldens of today's read-only projection. Update the
   authority and provider-action specifications with the profile contract in
   the same change. Risk tier: crucial.
2. **Narrow admission extraction** into a single stateless, write-free
   `AuthorityCompiler`, behaviour unchanged.
3. **One-provider write pilot** (`workspace-write-offline`: one owned
   worktree, no network egress, no external effects), gated by the standalone
   provider-write containment
   specification —
   worktrees are not permission boundaries; provider settings are intent, not
   containment proof; model refusal without a tool attempt is inconclusive.
4. **Second provider, then structural extraction** from the merged
   `ProviderActionDispatchInputV1` contract shape.

The architecture initially defines only `review-readonly` and
`workspace-write-offline`. Effective authority is the monotone intersection of
the human envelope, task/worktree ownership, risk policy, provider capability
and local attestation; providers cannot broaden a profile; receipts bind
requested/effective profile, compiler version and exact native settings.

The architecture decision does not authorise either execution slice. W010-A
requires separate human approval of the crucial-scope profile/compiler change
and thin recorder. W010-B requires a separate human grant naming the exact live
tuple, calls, cost, time and host. Those gates govern the grant; this ADR does
not record whether `workspace-write-offline` is currently available.

**Direct cutover, no legacy bridge** (human directive, overriding codex-pair's
proposed `LegacyAuthorityInputV1` quarantine): the repo is pre-release with no
external consumers; migrate all callers, tests and stored state to V2 in the
authority-contract cutover. Pre-existing stored authorities are regenerated or
the local pre-release state is reset — no dual parser is retained.

## Consequences

- Workspace writes and external effects remain strictly separated (staged
  effects via the existing `ExternalEffectService` model).
- The first write pilot provider is chosen by containment evidence, not
  preference; the other stays read-only until it independently passes.
- [Issue #22](https://github.com/mblauberg/provenant/issues/22) is the current
  route from the standalone specifications to this work. The former
  `docs/efforts/EFFORT-capability-profiles.md` route map is historical and is
  reachable at the `docs-archive-2026-09-02` tag.
