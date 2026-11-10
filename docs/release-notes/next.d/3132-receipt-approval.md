## Behaviour changes
### Commands

- `provenant land` takes `land.approval = "receipt"` to let an accepted delivery receipt stand as a pull request's approval. A merge-in from the target or a rebase with an unchanged delta keeps the review; any other change, including a stacked pull request retargeted past the branch below it, needs a new receipt. The graph land node accepts the same carried-over heads. (#3132)

## Breaking changes

- Delivery receipts move to schema version 2. A pull request subject now records `base`, the head's merge base with the branch the pull request merges into (for a stack member, the head of the branch below), and its repository; `workflow receipt save` reads both from the forge and ignores any the request carries. Changed paths are read from that base. A receipt saved before this release is reported as `receipt shape retired: save it again`; saving the delivery at its revision replaces it, with the decision reset to pending. (#3132)
