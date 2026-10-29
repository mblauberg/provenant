## Behaviour changes
### Fabric and MCP

- `--set key+=value` appends to a list preset parameter and is refused on scalars. Expansion refuses a composite `inputs` or `exports` selection that its source node cannot supply, naming both nodes (#2280).
