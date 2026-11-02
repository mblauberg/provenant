# Test judgement

- A typed refusal code survives wording changes; an assertion on an error sentence fails on harmless edits. Assert the code.
- An enum can exclude an invalid state; a test constructing that state through an unsafe escape adds no evidence about ordinary callers.
- One table-driven contract test can cover input variations; copied tests for each value add maintenance without a distinct defect.
- A property over parse and serialise round trips covers an invariant; a golden file is useful when the actual wire representation is itself a contract.
- A public boundary test survives internal reorganisation; call-count tests of owned collaborators constrain the implementation.
- An independent acceptance test gets expected outcomes from the spec; copying output from the implementation can make both share the same mistake.

## Test safeguards

- Never edit, delete, skip or loosen a test or assertion to get green. If a test contradicts the spec, stop and report the conflict as `blocked` (see #2589). (ImpossibleBench; EvilGenie)
- Every test asserts observable behaviour; printed values alone are not an oracle. (All Smoke, No Alarm; Chen et al.)
- Derive expected values from the spec, never from the implementation's current output.
- An assertion helper must fail on a known-bad input; prove that once when writing it.
- Declare each test's size: small is hermetic and about 1 s or less; medium uses a local filesystem or DB; large is opt-in. Never wait out a real timeout or sleep as proof; inject clocks, busy timeouts and limits. (Google test sizes)
- Compare unordered collections as sets and seed every RNG. (Hora and Robbes)
- Make every skip visible with a reason and an owner or issue; never use a silent early return.
- Put timing and performance claims in an opt-in benchmark, not the default run.
- Record the command and its output as evidence of done.
- For risky logic, use `cargo mutants --in-diff` or the language equivalent, then investigate survivors. (MutGen)
