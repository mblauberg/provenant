## Behaviour changes
### Pipelines and graphs

- `run --name` replays only the same launch: the digest now covers the resolved template and every template it uses, so editing a template launches the next free `NAME-k`. A replay reports the existing graph and starts or advances nothing, and concurrent launches with different input each get their own name instead of a conflict. (#3078)
