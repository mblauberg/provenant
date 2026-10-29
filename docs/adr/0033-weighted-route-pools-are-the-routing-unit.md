# ADR 0033: Weighted route pools are the routing unit

**Status:** Accepted

Changed: 2026-10-30, #2272.

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
Families are established from verified provider evidence, not from names. A
confidential request admits only models that do not train on prompts; free-tier
models need explicit opt-in and are never admitted to confidential work.

Review independence uses the resolved pool tier and model family, recorded per
attempt, so a review leg is judged by what actually ran
([review ladder](../../HARNESS.md#review-ladder)).

## Consequences

Retuning capacity or adding a model is a data change. Tests assert resolver
outcomes, not catalogue contents. The [routing spec](../specs/routing.md) owns
the rules and the [routing runbook](../runbooks/model-routing.md) owns use.
Changing how free models are treated in confidential work amends this ADR.
