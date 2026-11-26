## Fixes

### Graphs and routing

- `review-repair` now resolves an authored `with.writer` id to its expanded node id, so naming the writer as `fix` in a composite works where the compiled id is `task--fix`. When a continuation still cannot bind, the refusal names the expected expanded id and lists the upstream writers that could satisfy it. (#4066)
- `run --explain` reports a `node_routes` row for every launchable node: the effective adapter, model and effort with the source of each (`node field`, `policy`, `pool` or `default`). An exact node field or a configured named route resolves to one spelling; a pool lists its candidate models without drawing a pick, since the weighted selection, account and credential decision remain launch-only. Pool routing can now be checked before submitting a graph. (#4066)
