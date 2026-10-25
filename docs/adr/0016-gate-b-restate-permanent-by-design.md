# ADR 0016: Keep GATE-B' skipped by design after the RESTATE decision

## Status

Superseded by [ADR 0029](0029-rust-rewrite-in-one-pass.md); accepted
2026-07-21 (RESTATE decision, issue #354).

## Context

The issue #354 GATE-B' characterisation asks whether certifying recovery should
settle a budget only when custody ownership also matches. The in-repository
behaviour settles the reservation using the authority bound at admission, while
the row-owner fence already protects the provider action. The dual-bound fixture
needed to exercise an alternative refusal is not produced by an in-repository
writer.

## Decision

RESTATE the behaviour as intended. Keep `GATE-B' DECISION-GATE RESOLVED
(restate): certifying settlement is owner-agnostic BY DESIGN` permanently
skipped. Do not add a custody-scoped refusal branch or build a harness for this
unreachable fixture unless the external review-evidence daemon publishes a
settled contract that requires custody-scoped settlement.

## Evidence and consequences

`GATE-B PINS LATENT BEHAVIOUR: certifying recovery settles its dual-bound budget`
pins the intended settlement behaviour. The permanent skip records that the
alternative path is a rejected design question, not forgotten test work; no test
logic or assertion changes are required.
