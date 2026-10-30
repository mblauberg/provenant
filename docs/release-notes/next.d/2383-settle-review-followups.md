## Fixes

- Explicit reconciliation schedules recovery for terminal attempts with retained temporary data or writer reservations, and one attempt's progress errors no longer hide other attempts' results. Registry failures use bounded automatic retries even when a terminal attempt still owes reactions. Child lanes can inherit read access to bundled skills inside an installed release; staging directories and other state roots remain excluded (#2383).
