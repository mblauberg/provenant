# Test judgement

- A typed refusal code survives wording changes; an assertion on an error sentence fails on harmless edits. Assert the code.
- An enum can exclude an invalid state; a test constructing that state through an unsafe escape adds no evidence about ordinary callers.
- One table-driven contract test can cover input variations; copied tests for each value add maintenance without a distinct defect.
- A property over parse and serialise round trips covers an invariant; a golden file is useful when the actual wire representation is itself a contract.
- A public boundary test survives internal reorganisation; call-count tests of owned collaborators constrain the implementation.
- An independent acceptance test gets expected outcomes from the spec; copying output from the implementation can make both share the same mistake.
