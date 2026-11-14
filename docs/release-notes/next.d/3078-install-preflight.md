## Fixes

### CLI and install

- The first v2 install transfers v1 harness ownership and validates the staged owner configuration before stopping legacy agents. Invalid handoff input leaves agents running and the v1 root untouched. (#3078)
