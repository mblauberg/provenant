## Fixes

### Lanes and settlement

- Unsandboxed lanes now find the host's gh login whatever the adapter does to
  `XDG_CONFIG_HOME`. OpenCode points that variable at private state, so gh read
  an empty configuration and reported no authenticated hosts. The lane
  environment and the lane's `gh` shim now pin `GH_CONFIG_DIR` to the host's
  directory (the host's `GH_CONFIG_DIR`, else its `XDG_CONFIG_HOME/gh`, else
  `~/.config/gh`), and the launch receipt's `github_credential.detail` names
  it and its source. The lane preamble and runbooks now say a lane opens its
  pull request with `gh pr create --body-file`; `provenant pr open` is for a
  settled attempt. (#3641)
