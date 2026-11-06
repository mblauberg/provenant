## Fixes
### Pipelines and graphs

- Lane edits stay within their own subtree; planners and graph owners can still edit across the graph. Landing requires an approving review gate after a gate replacement.
- Graph calls retain refusal codes and details, and CLI child replies use the same JSON as MCP. Status shows when a parent is waiting to reclaim admission after its children finish.
