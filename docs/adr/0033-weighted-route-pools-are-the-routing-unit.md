# ADR 0033: Weighted route pools are the routing unit

**Status:** Accepted

## Context

Tasks need capabilities without coupling every caller to a vendor model. Capacity varies across accounts and families.

## Decision

Select an exact model or a named, weighted pool. Pool membership, weights and policy are configurable data; routing owns eligibility and pacing. Family attribution depends on verified evidence, and confidentiality depends on the model's treatment of prompts, not its price.

Admission should bind a fresh capability snapshot to the route. A final pre-dispatch compare-and-swap should recheck the admitted capability and permissions; resolve pre-effect drift again and reconcile an ambiguous effect against its original action. Admission and observed identity are distinct evidence. This is a design direction, not a claim that all capability checks are implemented.

The [routing spec](../specs/routing.md#policy-is-overridable-data) owns current behaviour and the [routing runbook](../runbooks/routing-and-templates.md) owns use.

## Consequences

Adding or retuning a route is a data change. Evidence from a provider or benchmark is a prior, not proof of a local outcome.
