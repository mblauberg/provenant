# Lens: tests

Judge each test by the invariant it proves and the defect it would catch. Tests
must be designed extensibly, flexibly and reliably so future code evolution
does not require updating tests. Prefer invariants and property-style checks at
public boundaries over rigid tests, example snapshots or exact string/count
assertions.
Use the test-design judgement and boundary references for the full standard.
Cite the exact command and result used for anything you ran.

## Findings

A test finding names the production change that would pass unnoticed: a
surviving mutant, or an uncovered boundary invariant on a reachable path. It
keeps its normal severity. Reviewers must never demand rigid tests, exact count
or string pins, or regression tests when an invariant at the public boundary
suffices. Regression tests remain minimal: at most one small test only where no
invariant covers the defect.

These also stay findings, because each leaves a check proving less than it
claims:

- A weak oracle in a test in the diff: an assertion that cannot observe the
  outcome, an expected value copied from the output, a helper that ignores its
  argument, or a mock of the unit under test.
- A vacuous pass: an early return, a fixture listed as a test, a body that
  never reaches its assertion, a runner filter or config name that matches no
  test, or a name promising a bound the body never checks.
- A required test that does not fail closed when its dependency is unavailable.
- A real-time wait or sleep used as proof when a barrier, injected clock,
  timeout or limit can prove the behaviour.
- An assertion that encodes a spec clause being loosened or skipped to get a
  green run.
- A deletion that removes the only check of a live boundary.

## Notes

A finding that names no such production change is a note and not repair input.
That covers criterion mapping, a generic untested branch, and a pin on wording,
a count or a constant.

Also notes, as prompts to prune or rewrite:

- A rigid test, example snapshot, or test pinned to the implementation (private
  call counts, internal structure) that would fail on a behaviour-preserving refactor.
- A behaviour proved again at another level without a distinct defect, and net
  test growth with no new behaviour.
- Near-duplicate cases that a readable table-driven test could merge.
- A new test likely to take more than about one second, a wall-clock performance
  assertion in the default run, or an oversized fixture that exists only to
  cross a hard-coded limit.

Deleting a rigid, duplicate or implementation-pinned test with a stated reason
is good work.
