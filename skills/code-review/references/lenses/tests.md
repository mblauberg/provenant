# Lens: tests and verification

Canonical questions: [review-lenses.md](../review-lenses.md), section
"Verification" (the five test questions).

- For each changed test, ask what plausible defect it catches, whether it
  survives a behaviour-preserving refactor, and whether its oracle is independent
  of the code under test.
- Flag loosened assertions, skipped or deleted tests, and happy-path-only cover.
- Map acceptance criteria to tests. Record the exact command and result for any
  check you rely on; a claimed check without evidence is unknown.
