## Additions
### Pipelines and graphs

- The bundled `deliver` preset takes an approved slice from a writer's pull request to a merge. A CI gate waits on the pushed head with no model running. Fresh repair agents fix failing checks on the same branch, briefed with the check names, log excerpts and the previous agent's report but never its transcript, for up to `max_iterations` rounds. A review council then judges the newest head, and a land node merges it under the land tier policy. Exhaustion asks the `decision_session` or pauses the graph. `land = 0` stops after review (#2222; see [deliver through CI to land](../../runbooks/presets.md#deliver-through-ci-to-land)).
- A graph `source` of `{ type = "head", node = "WRITER" }` pins the newest commit delivered by a writer or by the writers that continue it, so a review after a variable number of repairs judges the head that land merges (#2222; see [sources](../../specs/fabric.md#sources-and-read-roots)).
- Presets can set `continues`, `land` nodes and a top-level `decision_session`. An empty `decision_session` leaves it unset (#2222).
