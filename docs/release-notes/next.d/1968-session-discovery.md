## Behaviour changes
### Chair, inbox and console

- Saving a checkpoint now registers the chair for session discovery and reports a checkpoint left inconsistent by an interrupted save (#1968).
- Each chair now has its own inbox identity, so Claude and Codex chairs on one host no longer share an inbox. To recover an old binding, run `session save --client HARNESS --rebind` (#1968).
