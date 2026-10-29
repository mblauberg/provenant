## Behaviour changes
### Lanes and settlement
A review verifier is now routed away from the model families of every upstream lens attempt (`distinct_from = "dependencies"`), with a recorded `separation` on the node. It prefers by default and substitutes with a note when no other family is ready; set `family = "cross_required"` on the role to fail instead. Nodes that declare `dispositions` now settle as failed unless every finding carries an allowed `disposition` and a `reason`.
