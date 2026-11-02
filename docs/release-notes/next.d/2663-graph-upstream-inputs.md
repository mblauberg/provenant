## Fixes
### Fabric graphs

- A graph node can read the full result of the upstream nodes it selects, not only the bounded prompt excerpt. Each result on this host is written, redacted exactly as `provenant output --full` shows it, to a read-only file in the node's own inputs directory, and the prompt lists the path beside the excerpt. The node is granted read access to that directory alone, up to 2 MiB of results in total; a result cut at the limit ends with a marker and the prompt names it. A result on another host is named unavailable and the node still runs; remote materialisation is tracked in #2665 (#2663).
