## Breaking changes
### Fabric graphs

- Prepared graph operations no longer retry submission using the older input
  without task context. Finish or cancel active graphs with the old binary and
  wait for their attempts to settle before upgrading (#2828).
- Checkpoints containing pre-node-ask loop state are now unreadable. Finish or
  cancel their graphs with the previous release before upgrading; cancelling an
  unreadable record only retires it and does not settle attempts or inbox
  requests (#2828).
- Checkpoints missing expansion topology are no longer reconstructed. Finish
  those graphs before upgrading; cancel an unreadable record and start a new
  graph after its attempts and custody have been settled (#2828).
