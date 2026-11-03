## Fixes
### Pipelines and graphs

- Planned graphs retain expanded composite membership and completion edges across checkpoint reloads, allowing settled members to advance or report their typed failure. Existing pattern and fallback checkpoints recover the missing topology from the submitted specification. Invalid checkpoints name the node and the violated invariant (#2774).
