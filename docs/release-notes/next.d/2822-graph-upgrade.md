## Breaking changes
### Pipelines and graphs

- **Drain in-flight graphs before upgrading.** Graph launch retry checkpoints now retain the policy bound to each attempt (#2822, #2823).
