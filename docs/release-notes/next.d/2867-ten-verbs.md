## Breaking changes
### CLI and install

- Start graph work with `run [BRIEF] --use TEMPLATE` or `run --file DOCUMENT`; inspect layered templates with `templates` and apply live patches with `edit`. The default help shows the ten graph verbs. Removed command routes and `fabric_graph` have no aliases (#2867).
- CLI and MCP share document submission, patch admission, template inspection and grouped attempt statistics. `output --trace` reads the retained graph event timeline (#2867).
