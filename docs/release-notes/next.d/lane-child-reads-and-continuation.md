## Fixes
### Pipelines and graphs

- Child output reads stay within the calling lane's graph when graph and attempt IDs collide.
- Writer children in another project can continue after quota fallback or retry. Existing same-project writers retain their saved request identity on replay after upgrading.
