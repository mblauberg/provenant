## Breaking changes
### CLI and install

- **Update work targets to the graph, node or attempt syntax** `NAME[/NODE[#N]]`; older identifiers and prefixed targets are removed. Use `provenant wait` to wait for work and `retry` to start a fresh attempt, supplying a new brief or extension when needed. (#3078, #3175, #3177, #3179)
