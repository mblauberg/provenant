## Fixes

- Report `upgrade_required` for newer stored record, graph document, graph
  checkpoint and database schemas, with the reader release and an unknown
  writer release where it was not recorded.
- Accept additional stored and peer record fields. Preserve fields included in
  canonical digests through reader round trips. MCP arguments and user-authored
  configuration remain strict.
