## Fixes

### Lanes and settlement

- Reusing a graph name with changed input now leads the launch output with `started NAME-2 (NAME exists with a different input)` and reports the prior handle as `named.superseded` in `--json`. `wait NAME` and `status NAME` warn that the graph was superseded and print the newer handle as the next step without redirecting the read. Name-grammar refusals suggest the normalised name, so `--name Foo-` suggests `foo`. (#3496)
