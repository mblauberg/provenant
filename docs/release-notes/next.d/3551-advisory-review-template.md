## Behaviour changes

### Pipelines and graphs

- `graph --pr N` against a template that does not accept a review range no longer fails with "--pr requires a review template". It warns with that source and proceeds with the host's default review shape (#3551)
