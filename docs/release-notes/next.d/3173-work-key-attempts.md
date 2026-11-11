## Breaking changes
### Lanes and settlement

- **The run store keys attempts by work key.** Each attempt row names its project, graph, node and attempt number, which the graph assigns and the store keeps unique. The run, operation, continuation, cancellation and admission-window tables and the separate work index are removed; a refused admission is a terminal attempt row under its key. An earlier run store is refused with `STATE_RESET_REQUIRED`. (#3173)
