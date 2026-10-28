## Fixes

- Identify missing or stale Go calibration windows and the required account key in pool exclusion diagnostics.
- Label OpenCode Go usage as a partial local-ledger lower bound in quota reports and routing diagnostics.
- Apply expiring per-account dashboard floors to quota and pacing, taking the maximum of the ledger and each configured percentage. Go pool members require fresh quota and calibration covering all three windows.
- Add MiMo V2.6 Flash provisionally to junior and senior pools at weight 1, and the paid Go endpoints of DeepSeek V4.1 Flash and GLM-5.3 Flash to the junior pool at weight 1 as fallbacks when their free variants run out. Catalogue MiMo V2.6 Pro and GLM 5.3 for exact selection pending trials.
