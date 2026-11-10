## Fixes
### Lanes and settlement

- `status GRAPH/NODE/ATTEMPT` with a node or attempt that does not belong to the graph now fails at once with a `not_found` error naming the part that did not match. `status` and other lane listings no longer flush every settled writer's delivery receipt to disk, which made them take minutes while another lane was building. (#3144)

- `status`, `output` and `steer` now resolve a full `GRAPH/NODE/ATTEMPT` lane ID through the graph's checkpoint and show or act on only that lane, instead of rejecting it or listing every lane. `steer` also accepts `GRAPH/NODE`. (#3144)
