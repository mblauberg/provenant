## Features

### Fabric and templates

- Enhanced the bundled `batch` template to support structured items, ordering,
  and concurrency caps:
  - `items` accepts structured tables `{id, brief | brief_file, route | model, write_scope, after}`
    alongside bare strings. Mutual exclusion between brief/brief_file and route/model is enforced.
  - Slices can specify `after` dependencies; cycles and unknown IDs are rejected at compile time.
  - `max_parallel` caps concurrent slice execution, defaulting to `domains.fabric.batch.max_parallel`
    (with source shown in `--explain`) and overridable per run via `--set max_parallel=N`.
    Queued slices show as waiting in `status` with reason `cap` or `after`.
  - Slice settlement integrates into `integration_branch` incrementally; a merge conflict pauses
    only that slice, reporting conflicting files, while other slices continue running. (#3532)
