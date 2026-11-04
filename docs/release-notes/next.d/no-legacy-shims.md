## Behaviour changes

- Doctrine: Provenant keeps no compatibility shims, before or after 1.0. Breaking changes ship with a release note, plus `provenant config migrate` or a drain before upgrade when persisted state changes. The `refactor` skill now forbids shims and legacy fallbacks in any project, allowing only a short-lived strangler while live traffic or data moves.
- Doctrine: provider and launch checks outside the four hard boundaries (lane Git, credentials, object integrity, authority) warn and continue, or are configurable defaults. They no longer refuse a lane, and reviews treat such a refusal as a defect to loosen.
