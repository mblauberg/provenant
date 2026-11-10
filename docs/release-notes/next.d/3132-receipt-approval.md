## Behaviour changes
### Commands

- `provenant land` takes `land.approval = "receipt"` to let an accepted delivery receipt stand as a pull request's approval. A merge-in from the target or a rebase with an unchanged delta keeps the review; any other change needs a new receipt. The graph land node accepts the same carried-over heads. (#3132)

## Breaking changes

- A pull request delivery receipt's subject now requires `base`, the commit the review read the head against. Receipts saved without it no longer load; save them again. (#3132)
