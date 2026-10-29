## Breaking changes
### Lanes and settlement

- **Drain writer lanes before upgrading.** Running lanes from earlier releases keep their old shared-object write grants. An interrupted older attempt with private objects can resume; without them the host names the recovery path and requires a fresh run (#2131, #2158).
- **Stored lane Git grants from earlier releases no longer match.** Lane writers no longer hold grants on the shared repository's refs, so the lane Git grant digest changes (#2130, #2139).
