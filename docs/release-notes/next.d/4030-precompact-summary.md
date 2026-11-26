## Fixes
### Chair, inbox and console

- Compaction summaries no longer relist finished work as next steps or replay saved rulings. The PreCompact instruction now marks work done since the last checkpoint, gives the verbatim tail priority where they disagree, lists waits in flight, cites saved rulings from checkpoint Intent, drops items untouched for 24 h unless they are owner asks, and asks for summaries under 12k characters (#4030).
