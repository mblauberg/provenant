# Test boundaries

Expected values come from the approved spec, not the implementation. Test a real public entry point and assert stable results or error codes. If the necessary seam does not exist, use diagnose to find it before repairing a bug.

Mock process, network, clock and randomness edges only. A fake of an owned collaborator can conceal a broken integration; exercise the real collaborator at the boundary. Inject a clock or wait for a state transition with a bounded poll instead of sleeping.

Preserve user-authored work while establishing a failing test. Do not weaken an assertion or delete a test to hide a real behaviour change. Explain every weakened or removed test, including intentional pruning.
