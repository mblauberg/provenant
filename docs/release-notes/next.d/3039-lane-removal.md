## Additions

### Lanes and settlement

- Finished lanes clean up after themselves. Once a lane's work has landed, settlement or the next `gc` pass removes its worktree, local branch, remote branch and remote-tracking ref, but only refs Provenant created, each deletion leased on the tip it checked. Unlanded refs, refs whose landing state is unknown and refs whose tips differ from the ledger are retained regardless of age. Every artefact reports one JSON outcome, and a failed removal warns without failing settlement (#3039).

- Completion cleanup drains final retentions from its queue. Transient failures retry with durable backoff for at most five attempts; exhausted requests stay as diagnostic evidence.

- A graph waiting for a writer lock can resume from its checkpoint before its first attempt has launched, while malformed or unbounded launch waits remain refused.
