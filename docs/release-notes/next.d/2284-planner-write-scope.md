## Behaviour changes
### Pipelines and graphs

- Planner fragments now require a literal `write_scope` only for writer children. Read-only children may omit it, while a writer without a scope is still refused (#2284).
