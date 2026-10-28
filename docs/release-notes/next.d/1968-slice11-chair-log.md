## Additions

- Chair sessions can append replay-safe updates and corrections, and read bounded
  pages. Configurable per-session retention prunes old payloads while preserving
  operation receipts for a configurable replay horizon and aggregate counts.
  Appends carry an issued time, so stale retries are refused after receipts
  expire without imposing a lifetime operation limit. Lists respect age
  retention, and full GC removes expired payloads from idle sessions.
