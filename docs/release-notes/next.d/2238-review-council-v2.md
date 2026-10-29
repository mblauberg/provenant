## Behaviour changes
### Pipelines and graphs

- `review-council` now runs a verifier between the lenses and the synthesis. The verifier tries to falsify each finding on a route named `verifier` (falling back to `senior`, recorded), and the synthesis gives every finding a disposition of accepted, rejected or deferred. `generator-evaluator` gains an evaluator stage with the same dispositions, and `run --explain` shows the family constraint of each node. Set `verifiers = 0` to skip verification (#2238).
