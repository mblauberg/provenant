## Fixes
### MCP

- An expired `fabric_run` wait now replies with the same typed wait shape as every other wait, including `attempts: []`, so clients that decode `RunResult` accept it. A `Timeout` from outside an exhausted wait budget, and a timeout on a non-wait operation, still report an error (#2302).
