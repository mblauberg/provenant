## Fixes

### Graphs and templates

- An invalid `--set` parameter now fails with the field named and what is accepted. An unknown key lists the template's parameters and suggests the nearest one, a wrong value names the expected shape, and a failure inside a composite also names the node.
