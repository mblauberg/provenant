# ADR 0033: Weighted route pools are the routing unit

**Status:** Accepted

Changed: 2026-10-30, #2272 and #2349.

## Context

Work needs a model of a suitable capability without every caller naming a
vendor model, and capacity is spread across several accounts and families that
each hit quota limits. Fixed task classes coupled callers to model names.

## Decision

A task selects either one exact model or one named pool. The bundled catalogue
defines `utility`, `junior`, `senior`, `principal`, `writing` and `design`
pools. Junior, senior and principal form the capability pyramid that presets,
roles and review tiers refer to. A pool makes a deterministic weighted draw
among ready positive-weight members, scaled by provider pacing; zero-weight
members are ordered fallbacks. Pool membership and weights are data, editable
under `[domains.routing]`, not code.

Routing owns eligibility, weights and pacing; accounts only filter eligibility.
Families are established from verified provider evidence, not from names. Free
models are ordinary pool members with no opt-in. Confidential routing depends
only on whether a model trains on or retains prompts: a confidential request
admits only models whose catalogue row says neither, and an unknown row is
ineligible. Price and free status do not enter that check.

Review independence uses the resolved pool tier and model family, recorded per
attempt, so a review leg is judged by what actually ran
([review ladder](../../HARNESS.md#review-ladder)).

## Consequences

Retuning capacity or adding a model is a data change. Bundled pool contents
are pinned by catalogue tests and the policy golden, so a data change updates
those in the same commit. The [routing spec](../specs/routing.md) owns
the rules and the [routing runbook](../runbooks/model-routing.md) owns use.
Changing the confidentiality rule, or treating free models differently from
paid ones in eligibility, amends this ADR.
