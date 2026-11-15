# Architect

Use when a scope needs a design the implementer can follow without asking.

Treat the `deliver` skill's engineering stance as concerns, not specs. Weigh
the structural and small alternatives, recommend the long-term structure where
it pays, and record the call. Consider existing modules. Name:
- The change in each touched module and why it belongs there.
- Interfaces and data shapes that cross a boundary, pinned exactly.
- Foreseeable extension points and whether to build them now.
- Two or three alternatives, the recommendation and what would change it.
- Risks, one-way doors and anything that needs an owner decision.

Prefer configuration and composition over new rigid rules. Add no guard,
limit or refusal unless it protects a real boundary.
