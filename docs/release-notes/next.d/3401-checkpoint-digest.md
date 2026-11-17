## Fixes

### Pipelines and graphs

- Credential-shaped graph text is normalised before planning, so saved graphs verify on reload and advance with matching plan digests. Template parameters are normalised even when their names match identity fields, and strict checkpoint reads reject incorrect plan digests before publication or landing. (#3401)
