# ADR 0021 — Proportionate mechanism, and named quality attributes

**Status:** Accepted 2026-08-10 (user; the attributes were named by the user)

## Context

[ADR 0001](0001-personal-first-product-compatible.md) fixed the posture as
personal-first, and [ADR 0020](0020-retire-the-daemon-fabric.md) applied it
once, at scale, by deleting a 350,000-line coordination runtime that agents
could not actually use. Neither left behind a reusable test. The reasoning in
ADR 0020 was recorded as the justification for one deletion rather than as a
rule, so the same class of accretion could recur, and did.

A review of the seven salvage changes merged on 2026-08-10 found three
instances. A subprocess-drain rule was enforced by a 310-line
abstract-interpretation visitor guarding four call sites. A local worktree
helper acquired credential redaction, capture digests and a hardcoded trusted
`PATH`, all defending against the operator's own machine, and the `PATH`
restriction fails closed on any git installed outside the list. A hard
per-module line cap was satisfied by passing `globals()` into a helper module
rather than importing it, which made the module harder to review, not easier.

Each was individually defensible under a generic "good engineering" standard.
The standard was the problem: it is calibrated for shared, multi-tenant,
staffed systems, and this is one person's harness on one laptop.

The quality attributes the harness is actually optimised for were also
unwritten. They were inferable from ADRs 0001 and 0020 and from scattered
clauses in `MAINTAINING.md`, but no document stated them, ranked them, or said
what to do when they conflict. An agent reading the repository cold would
default to the generic standard, because nothing told it not to.

## Decision

Adopt the quality attributes in [`docs/ASRS.md`](../ASRS.md) as the harness's
architecturally significant requirements, in the stated priority order:
reliability in use, low maintenance, simplicity proportionate to the risk
actually carried, flexibility and extensibility through seams, parity across
both primaries, and personal-first product-compatible posture.

Gate new mechanism on the four-question proportionality test in that document.
A mechanism that cannot name a failure that has actually occurred here, and
cannot name an adversary other than the operator's own environment, does not go
in. Audits apply the matching removal test, whose default outcome is deletion
with the tip preserved on an archive branch.

Record the exception list explicitly in the same document, so that "simplify"
is never read as licence to weaken the checks that exist for observed agent
failure modes: authors may not certify their own surface, the user gates hold,
evidence outranks confidence, one writer owns each source surface, and public
history stays clean.

`HARNESS.md` carries the binding one-line form, because it is the only document
loaded every session. `MAINTAINING.md` applies the test to repository change.
`docs/ASRS.md` holds the reasoning and the worked calibration examples.

## Consequences

Mechanism now carries a burden of proof that documentation and tests do not.
This is deliberately asymmetric: a wrong comment is cheap and a wrong gate
blocks work.

Some existing machinery fails the test as merged, and is recorded as such
rather than quietly kept. Remediation is separately scoped, because removing a
correct-but-disproportionate check is a judgement the operator makes, not a
tidy-up an agent performs unasked.

A hard per-module review cap that a module can satisfy by degrading its own
structure is itself subject to the test. Where a rule is satisfied by a
construction nobody would choose on its merits, the rule is what changes. The
specific instance is recorded here so it is not rediscovered:
`tests/test_delivery_validator_structure.py` globs `*.py` across the whole of
`skills/deliver/scripts/`, so a test named for the validator's structure caps
the producer as well, and the producer met the cap by passing `globals()` into
its helper. Two independent reviews reached that conclusion separately.

The removal test needs a diagnosis step before it is acted on, because dead
code and under-wired code present identically. That correction came out of
review and is written into `docs/ASRS.md` rather than left implicit.

The cost is that a genuine defence-in-depth measure can now be argued down by
citing this ADR. The exception list is the guard against that, and the
priority order means a proposal to weaken a listed guarantee is answered on
reliability grounds rather than on taste.

## Rejected

- **Leaving the attributes implicit in ADRs 0001 and 0020.** Tried already.
  Both are decision records about one topic each, and neither is read before an
  unrelated change.
- **Putting the whole statement in `HARNESS.md`.** The constitution stays
  compact because it is always loaded; worked examples and calibration belong
  in a document read on demand.
- **A hard machine-checked complexity budget.** It would be a rule of exactly
  the kind this ADR warns about: the line cap that produced `globals()`
  injection is the demonstration.
