## Fixed
### Lanes and settlement

- A lane's supervisor no longer re-parses the whole configuration ten times a second while no child lane is queued, which cost each running lane about a fifth of a core.
