## Fixes
### Chair, inbox and console

- The answer hook reports corrupt local answer records and continues past them, instead of silently omitting them (#2076).
- Answers too large for the hook's envelope are skipped by later hook calls while Fabric mail stays available. Small answers behind an oversized one are still delivered, and the diagnostic names the answer by ID (#2077, #2078, #2081).
