# Lens: correctness against the spec

Canonical questions: [review-lenses.md](../review-lenses.md), sections
"Correctness and failure" and "Verification".

Check the change against the spec (the issue or file you were given), not
against the author's account of it.

- Map each acceptance criterion to code and to evidence. Name any criterion with
  no implementation or no test.
- State the invariants that must hold before and after each operation, then try
  to break them with empty, duplicate, stale, malformed, partial and retried input.
- Follow failure paths: ordering, cancellation, timeouts, re-entry, partial
  writes and errors that are converted to success.
- Check public API, schema and protocol compatibility against the base.
