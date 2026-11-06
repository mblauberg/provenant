## Breaking changes
### Pipelines and graphs

- **Automatic retries now belong to the graph.** Drain active lanes before upgrading. Runs no longer launch policy successors or use separate child stores; existing child-store directories are ignored. Lane route failover continues through the graph (#2869).
