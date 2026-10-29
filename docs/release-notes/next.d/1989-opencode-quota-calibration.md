## Fixes
### Accounts, quota and routing

- Quota reports and routing diagnostics label OpenCode Go usage as a partial lower bound from the local ledger, and name a missing or stale Go calibration window and the account key it needs (#1989).
- Expiring per-account dashboard floors apply to quota and pacing, using the larger of the ledger and each configured percentage. Go pool members stay eligible without calibration (#1989).
- The paid Go endpoints of DeepSeek V4.1 Flash (junior weight 2, senior 1) and GLM-5.3 Flash (junior 1, senior 1) join the pools provisionally, as fallbacks when their free variants run out. MiMo V2.6 Flash is available by exact selection only (#1989).
