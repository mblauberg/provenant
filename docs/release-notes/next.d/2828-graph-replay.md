## Breaking changes
### Fabric graphs

- Prepared graph operations no longer retry submission using the older input
  without task context. Finish or cancel active graphs with the old binary and
  wait for their attempts to settle before upgrading (#2828).
