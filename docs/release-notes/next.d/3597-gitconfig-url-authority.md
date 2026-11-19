## Fixes
### Git and safety
- A URL with extra slashes after its scheme, such as `https:///user:password@host/`, no longer gets its credentials past a sandboxed lane's Git configuration or output redaction. Git and curl read the user after any number of slashes, and the lane's `insteadOf` screen and the redactor now share one URL-authority parser that does the same. A `domains.lanes.git_config_allow` pattern naming a non-SSH URL user of any shape is now refused, so the launch receipt cannot record one. (#3597)
