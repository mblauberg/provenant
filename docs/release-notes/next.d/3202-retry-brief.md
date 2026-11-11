## Fixes
### Pipelines and graphs
- `provenant retry GRAPH --brief TEXT` now replaces the brief of a graph's only node instead of refusing the field; a graph of several nodes asks for `--node`, and a container built from a template with no `brief` parameter names that field and template. A template-derived acceptance check follows the new brief (#3202)
