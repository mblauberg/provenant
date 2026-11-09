# Lens: correctness against the spec

Shared guidance: [review-lenses.md](../review-lenses.md).

Check the change against the spec (the issue or file you were given), not
against the author's account of it.

- Map each acceptance criterion to code and to evidence. Name any criterion with
  no implementation, or whose stated verification does not exist.
- State the invariants that must hold before and after each operation, then try
  to break them with the empty, duplicate, stale, malformed, partial and retried
  input a real sequence can produce.
- Follow failure paths: ordering, cancellation, timeouts, re-entry, partial
  writes and errors that are converted to success.
- Check public API, schema and protocol compatibility against the base.
- For refactors, check that intended behaviour and failure handling are preserved.
