# Lens: concurrency, data and errors

Shared guidance: [review-lenses.md](../review-lenses.md).

- Persistence: atomic writes and the repository format policy for persisted state changes.
- Async, retries and cancellation: idempotency, ordering, duplicate delivery.
- Error handling: context preserved, no silent conversion to success, safe
  partial failure with observable states named.
