## Fixes

- Recovery decodes of child-store captures pass the holder's home into the provider registry, so OpenCode's session identity resolves from `<home>/.local/share/opencode` when `XDG_DATA_HOME` is unset instead of reporting no model (#2416).
