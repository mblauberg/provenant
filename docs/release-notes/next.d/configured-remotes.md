## Fixes

- A stray remote-tracking ref, such as an old `refs/remotes/review/N` fetch, no longer counts as a Git remote. Before, a repository with one configured remote and such a ref refused every fresh `--branch` writer launch with "no Git remote is declared and none can be inferred". Remotes are the configured ones; tracking-ref directories name remotes only in a store with none configured.
