## Fixes
### Pipelines and graphs

- Review panels retry a malformed lens verdict once and retain returned findings when coverage falls short. If the correction exceeds a prompt limit, the lens remains unavailable and the panel continues under its coverage rule. Coverage failures remain replayable even when finding export also fails. Normal multi-lens findings use the aggregation budget, avoiding premature verification overflow (#3067).
