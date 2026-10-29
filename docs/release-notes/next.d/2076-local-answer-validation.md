## Fixes

- Local answer readers now report corrupt records instead of silently omitting them. The hook continues past invalid records and prints a diagnostic (#2076).
- The hook marks answers that cannot fit its envelope, so later hook calls skip them while Fabric mail stays available. Small answers behind an oversized one are still delivered, and SQLite errors are reported with bounded detail (#2077, #2081).
- A local answer that exceeds the page limit is identified by answer ID in a page-limit diagnostic (#2078).
