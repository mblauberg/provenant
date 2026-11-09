## Fixes
### Pipelines and graphs

- Review panels retry a malformed lens verdict once and retain returned findings when coverage falls short. Normal multi-lens findings use the aggregation budget, avoiding premature verification overflow (#3067).
