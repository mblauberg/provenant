## Fixes
### Telemetry and providers

- Claude cloud submissions from a graph now run under the pseudo-terminal they need. A submission whose outcome is unknown keeps a bounded diagnostic, and `provenant reconcile --abandon` settles it with a warning that a remote session may still exist (#2889).
