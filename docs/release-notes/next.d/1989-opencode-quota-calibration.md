## Fixes

- Identify missing or stale Go calibration windows and the required account key in routing diagnostics.
- Label OpenCode Go usage as a partial local-ledger lower bound in quota reports and routing diagnostics.
- Apply expiring per-account dashboard floors to quota and pacing, taking the maximum of the ledger and each configured percentage. Go pool members remain eligible without calibration, using the local ledger as a lower bound.
- Add the paid Go endpoints of DeepSeek V4.1 Flash (junior weight 2, senior 1) and GLM-5.3 Flash (junior 1, senior 1) to the pools provisionally, as fallbacks when their free variants run out. Catalogue MiMo V2.6 Flash for exact selection only.
