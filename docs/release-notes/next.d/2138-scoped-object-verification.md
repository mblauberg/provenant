## Fixes

- Limit settlement and delivery strict/hash checks to staged objects and the difference from pre-existing shared tips. Stream object names without a history-size cutoff, check connectivity against shared history, verify every staged pack entry and checksum, and refuse detected SHA-1 collisions. Private duplicates cannot hide corrupt shared copies. Remove temporary strict-check packs and indices on every return.
