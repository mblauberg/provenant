# Lens: concurrency, data and errors

Canonical questions: [review-lenses.md](../review-lenses.md), section
"Correctness and failure" (ordering, atomicity, retries, error propagation).

- Persistence and migrations: atomicity, rollback and mixed-version operation.
- Async, retries and cancellation: idempotency, ordering, duplicate delivery.
- Error handling: context preserved, no silent conversion to success, safe
  partial failure with observable states named.
