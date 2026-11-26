## Fixes
### Pipelines and graphs

- Retry rounds no longer inherit contradictory success requirements from a prior failed round. (#3972)
- Loop members retain dependencies inherited from their enclosing composites. (#3972)
