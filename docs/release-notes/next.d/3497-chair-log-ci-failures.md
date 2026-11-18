## Fixes

### CLI and chair

- `chair log append` accepts markdown text directly on standard input or `--file`, and `chair log list` defaults `--session` to the active host chair session when known. Config refusals on keys with dedicated commands name their replacement command. Unknown CLI subcommands and flags receive suggestions, and `ci failures pr N M ...` accepts multiple pull requests with a single batched forge read. (#3497, #3504)
