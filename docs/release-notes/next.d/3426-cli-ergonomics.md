## Breaking changes

### CLI and install

- Replace `gc --idle-days N` with `gc --idle Nd`, and use `templates show NAME` to inspect a template. CLI reads are compact by default; use `--full` or `--json` for detail. `result NAME` summarises a lane in one call, and launch refusals and settled steering name the cause and next action. (#3426)
