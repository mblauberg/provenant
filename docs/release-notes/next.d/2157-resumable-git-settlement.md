## Behaviour changes
### Lanes and settlement

- Writer lanes now have a private Git directory with read-only shared refs, and the host verifies lane objects before they reach the shared repository. Stored lane Git grants from earlier releases no longer match (#2130, #2131, #2138, #2139, #2158).
- Settlement resumes after a crash and `cancel` and `reconcile` stay responsive while Git runs. A terminal result no longer needs a second `reconcile` (#2141, #2151, #2154, #2157).
