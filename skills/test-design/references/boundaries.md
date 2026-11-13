# Test boundaries

Expected values come from the approved spec, not the implementation. Test a real public entry point and assert invariants, stable properties or error codes rather than rigid snapshots or exact strings. If the necessary seam does not exist, use diagnose to find it before repairing a bug.

Mock process, network, clock and randomness edges only. A fake of an owned collaborator can conceal a broken integration; exercise the real collaborator at the boundary. Inject a clock or wait for a state transition with a bounded poll instead of sleeping.

Preserve user-authored work while establishing a failing test. When an existing test fails, decide whether it pins implementation detail; rewrite it as an invariant or delete it rather than bending code. Do not weaken an assertion or delete a test to hide a real behaviour change. Pruning redundant tests is welcome; explain every weakened or removed test, including intentional pruning.
