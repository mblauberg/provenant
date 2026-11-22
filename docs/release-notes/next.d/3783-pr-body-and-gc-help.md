## Fixes

### CLI and install

- `pr open` no longer asks to approve the merge in every pull request body: a `Decision requested` section appears only when the caller passes an outstanding decision, and the findings row is now `Unresolved blocking findings`. `provenant gc --help` states the real `gc.free_floor` default of 1% of the state volume, capped at 5 GiB (#3783)
