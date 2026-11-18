# Test judgement

- Invariants and properties at public boundaries survive code growth and reorganisation; example snapshots, exact strings and counts go stale.
- Prefer regression coverage at a public boundary when it can catch the defect.
- A typed refusal code survives wording changes; an assertion on an error sentence fails on harmless edits. Assert the code.
- A test that builds a state the types exclude, through an unsafe escape, says nothing about ordinary callers.
- One table-driven contract test covers input variations; a copy per value adds upkeep, not coverage.
- Call-count tests of owned collaborators constrain the implementation; test the public boundary.
- Take expected outcomes from the spec; copying implementation output shares its mistakes.

## One declaration

- Avoid hand-kept mirrors of declarations; keep independent expected values when they provide an oracle.
- Use snapshots when representation matters, and property or state-machine tests when they cover variations better than examples.
- Pin clocks from the same epoch the fixtures and seed use, never a hand-picked date that can fall before seeded data.
- Parallel worktrees conflict on shared hand-edited lists and recorded files every change must touch; discover by convention instead.

## Test safeguards

- Choose the boundary that gives useful confidence without redundant coverage.
- Every test asserts observable behaviour; printed values alone are not an oracle.
- An assertion helper must fail on a known-bad input; prove that once when writing it.
- Avoid waiting out real timeouts as proof; inject clocks, busy timeouts and limits.
- Compare unordered collections as sets and seed every RNG.
- Make every skip visible with a reason and an owner or issue; never use a silent early return.
- Put timing and performance claims in an opt-in benchmark, not the default run.
- For risky logic, `cargo mutants --in-diff` or the language equivalent can expose weak tests; investigate survivors. Review councils run it only at crucial or terminal risk.
