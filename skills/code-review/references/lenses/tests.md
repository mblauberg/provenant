# Lens: tests

Judge each test by the behaviour it proves and the defect it would catch.
Use the test-design judgement and boundary references for the full standard.

- Check that assertions observe outcomes and use an oracle independent of the
  implementation. Flag helpers that ignore their argument.
- Catch vacuous passes: early returns, fixtures listed as tests, or a test body
  that never reaches its assertion.
- Make skips visible. A required test must fail closed when its dependency is
  unavailable.
- Do not use real-time waits or sleeps as proof when a barrier, injected clock,
  timeout or limit can prove the behaviour.
- Keep wall-clock performance assertions out of the default test run.
- Question oversized fixtures that exist only to cross a hard-coded limit.
- Favour pruning. Deleting a rigid, duplicate or implementation-pinned test with
  a stated reason is good work; flag a deletion only when it removes the sole
  check of a spec clause.
- Flag tests pinned to the implementation (private call counts, internal
  structure) that would fail on a behaviour-preserving refactor.
- Flag a behaviour proved again at another level without a distinct defect, and
  net test growth with no new behaviour.
- Merge near-duplicate cases into table-driven tests when that stays readable.
- Verify test-runner filters and config names against tests that exist.
- Flag each new test likely to take more than about one second.
- Reject an assertion that encodes a spec clause being loosened or skipped to
  get a green run.
- Do not mock the unit under test or copy expected values from its output.
- Ensure a test name does not promise a bound or behaviour its body never checks.
- Map acceptance criteria to tests and cite the exact command and result used.
