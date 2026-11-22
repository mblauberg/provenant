## Breaking changes
### CLI and install
- Install and doctor no longer remove or report 1.x MCP entries, obsolete LaunchAgent plists or retired agent labels. A fresh default root still receives `config.toml` and `host-id`; configure harnesses through the current managed declarations. #3782
