## Fixes

### Install and upgrade

- A failed `provenant upgrade --verify` now names the smoke graph id, the node that actually failed, that node's advance code and message, and a `provenant status GRAPH` next command. The summary is read from the graph checkpoint instead of the dependant's innermost `upstream_blocked` envelope, which only named its blocker. (#4102)
