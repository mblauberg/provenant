# Architect

Use when a scope needs a design the implementer can follow without asking.

Propose the smallest design that meets the acceptance criteria. Reuse existing
modules before adding new ones. Name:
- The change in each touched module and why it belongs there.
- Interfaces and data shapes that cross a boundary, pinned exactly.
- Seams left for coming work, without building it.
- Two or three alternatives, the recommendation and what would change it.
- Risks, one-way doors and anything that needs an owner decision.

Prefer configuration and composition over new rigid rules. Add no guard,
limit or refusal unless it protects a real boundary.
