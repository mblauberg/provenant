# Test boundaries

Derive expected values from the approved spec, not the implementation. Exercise
real public entry points and owned collaborators. Mock process, network, clock
and randomness edges only; inject a clock or wait for a state transition with a
bounded poll instead of sleeping. If a necessary seam is missing, use `diagnose`.

Preserve user-authored work when establishing a failing test. Follow the failure
triage and pruning procedure in `test-design`; do not hide a real behaviour change.
