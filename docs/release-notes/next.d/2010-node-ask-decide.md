## Additions
### Pipelines and graphs

- Graphs can ask a person to decide. A `decide` node posts its question and options to the inbox, and the chosen option becomes its verdict, so a verdict gate can route on it. With `timeout_seconds` and a `default`, the default is taken at the deadline, and the deadline is saved as `wake_at` in the checkpoint. With a graph `decision_session`, a budget or other pause also raises an inbox decision that can resume, cancel or raise budgets. Every ask is keyed by ID, several can be open at once, and the checkpoint records who answered or that the default was taken (#2010; see [node asks and decide nodes](../../specs/fabric.md#node-asks-and-decide-nodes)).
