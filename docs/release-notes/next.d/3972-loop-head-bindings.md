## Fixes
### Pipelines and graphs

- Bounded delivery loops can bind `previous.<node>` to an explicit initial publication. Later rounds bind and recheck the publication from the preceding round, keeping conflict and CI gates on the latest delivered head. (#3972)
