# Review lenses

Use the relevant [runtime lens prompts](lenses/) for lens-specific questions.
Follow evidence, not a fixed checklist quota. Apply the shared guidance below.

## Dependency cone

- Read each touched file in full where practical.
- Find live callers, consumers, re-exports, registrations and configuration.
- Identify the canonical owner of the changed behaviour or data.
- Trace persistence, network, queue, cache and generated-artifact boundaries.
- When persisted state changes, check the repository format policy.
- Read tests around the public behaviour, including sibling and integration
  tests that the diff did not touch.

Stop at the affected cone. Nearby pre-existing debt is in scope only when the
change depends on it, worsens it, or makes it newly dangerous.

## Aligning a looser rule with a stricter one

Before approving a change that tightens a permissive validator or boundary to
match its stricter counterpart, search the tests for the permissive values
themselves, not just the function name. A permissive boundary in front of a
strict one is often a designed funnel into a rejection path that no other test
can reach. Ask what becomes unreachable, not only what becomes impossible. When
the intent is recorded nowhere near the site, require an invariant or
property-style check at the public boundary or document the design, rather than
a rigid test pinning implementation details.
For absence or impossibility claims in a review, see `diagnose`'s evidence rules.
