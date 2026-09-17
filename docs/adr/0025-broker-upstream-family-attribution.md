# ADR 0025: Broker upstream family attribution

Status: Accepted  
Date: 2026-09-17  
Issue: #824

## Context

Multi-family brokers (OpenRouter endpoints, OpenCode, Agy, Kiro, Cursor) hide
upstream lineage behind the pipe. Distinct-family and certification gates compare
`lead_family` to `model_family`. Collapsing gateways to `generic-open` blocked
honest distinct legs and could still set mechanical `cross_family` against
Claude/Codex without proving a vendor.

## Decision

1. **`model_family` is upstream lineage** when the model slug can be inferred
   (after stripping broker prefixes `opencode/` and `openrouter/`, and a leading
   `~`). Pipe identity stays in `endpoint_provider` / adapter.
2. Record **`family_source`**: `slug-inferred`, `broker-default`,
   `endpoint-profile-fallback`, `runtime-capability`, `catalog-family`, or
   `unresolved`.
3. Endpoint profile `model_family` is a **fallback only** when inference fails
   (not an override of a known vendor slug).
4. **`generic-open` and `open-weight` are not assurance-eligible.**
   `--require-distinct` fails closed with `family_not_assurance_eligible`;
   dispatch `cross_family` / `certification_eligible` require an assurance-eligible
   family.
5. Keep inference **pattern-based and prefix-stripping** — no large per-model
   tables. Unknown Zen free ids may still dispatch as `generic-open` workers.

## Consequences

- Kimi/DeepSeek/Claude via OpenRouter or OpenCode can satisfy distinct-family
  when the slug attributes clearly and the lead family differs.
- Stealth / `auto` / unparseable broker ids remain ordinary workers only.
- Compatibility allowlists for OpenCode and Kiro admit the vendor families
  inference can produce; Kiro execution may stay disabled separately.

## Alternatives considered

- Secondary `inferred_lineage` field only — leaves assurance gap open.
- Per-broker mapping tables — high maintenance.
- Soft docs-only policy — already proven insufficient.
