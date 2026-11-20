## Behaviour changes

### Telemetry and providers

- OpenCode lanes now use the host XDG profile and service by default, with an isolated `providers.opencode.profile = "lane"` option. Host mode grants access to the OpenCode credential stores and attached tools run in the host service process. (#3703)
