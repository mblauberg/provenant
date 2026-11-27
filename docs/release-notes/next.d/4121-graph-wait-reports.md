## Fixes

### Pipelines and graphs

- Graph waits avoid a stack overflow during advancement. Failed writer bindings and launch warnings retain their correct status and codes. (#4121)
- Oversized results point to the private report named in the warning, and route refusals retain permission diagnostics. (#4121)
