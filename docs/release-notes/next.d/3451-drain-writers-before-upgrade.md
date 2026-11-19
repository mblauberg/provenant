## Breaking changes

### Git and safety

- Drain writers before upgrading: finish or stop every writer lane on the previous version first. Attempt evidence written before shared worktree leases (guardian lock record, object binding schema, import pins) is not upgraded in place. `provenant reconcile` reports each such attempt as a reservation issue and fences it until it is reconciled; leftover `refs/provenant/import/<commit>` pins are removed when the matching attempt is cleaned up. (#3451)
