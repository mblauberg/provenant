## Fixes
### Lanes and settlement

- `status GRAPH/NODE/ATTEMPT` with a node or attempt that does not belong to the graph now fails at once with a `not_found` error naming the part that did not match. `status` and other lane listings no longer flush every settled writer's delivery receipt to disk, which made them take minutes while another lane was building. (#3144)
