## Fixes

- Explicit reconciliation schedules recovery for terminal attempts with retained temporary data or writer reservations, and one damaged attempt no longer hides healthy attempts' results or blocks their recovery. Reaction-only workers stay out of the custody recovery report, and registry failures allow recoverable attempts to proceed while keeping failed recovery retries bounded. Child lanes can inherit read access to bundled skills inside an installed release; staging directories and other state roots remain excluded (#2383).
