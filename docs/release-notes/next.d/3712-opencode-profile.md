## Behaviour changes

### Lanes and settlement

- OpenCode's profile now follows `lanes.access`: host-access lanes use the host XDG profile and service, isolated lanes use a private per-attempt profile. `providers.opencode.profile` is removed; a stored value is ignored with a warning that names `lanes.access`. (#3712)
