# Test judgement

- Invariants and properties at public boundaries survive code growth and internal reorganisation; example snapshots or exact strings and counts go stale.
- Regression tests stay minimal: an invariant at the public boundary covers whole classes of defects; write a regression test only where no invariant can catch it.
- A typed refusal code survives wording changes; an assertion on an error sentence fails on harmless edits. Assert the code.
- An enum can exclude an invalid state; a test constructing that state through an unsafe escape adds no evidence about ordinary callers.
- One table-driven contract test can cover input variations; copied tests for each value add maintenance without a distinct defect.
- A property over parse and serialise round trips covers an invariant; a golden file is useful when the actual wire representation is itself a contract.
- A public boundary test survives internal reorganisation; call-count tests of owned collaborators constrain the implementation.
- An independent acceptance test gets expected outcomes from the spec; copying output from the implementation can make both share the same mistake.

## Test safeguards

- Prove each behaviour once, at the lowest boundary that can; do not repeat it at unit, integration and end-to-end level.
- Every test asserts observable behaviour; printed values alone are not an oracle.
- An assertion helper must fail on a known-bad input; prove that once when writing it.
- Declare each test's size: small is hermetic; medium uses a local filesystem or DB; large is opt-in. Never wait out a real timeout or sleep as proof; inject clocks, busy timeouts and limits.
- Compare unordered collections as sets and seed every RNG.
- Make every skip visible with a reason and an owner or issue; never use a silent early return.
- Put timing and performance claims in an opt-in benchmark, not the default run.
- For risky logic, `cargo mutants --in-diff` or the language equivalent can expose weak tests; investigate survivors. Review councils run it only at crucial or terminal risk.
