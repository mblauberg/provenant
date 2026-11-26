## Fixes
### Pipelines and graphs

- Uncapped delivery queue CI requests can create additional admission slots, while their leases still count against capped requests. (#3972)
