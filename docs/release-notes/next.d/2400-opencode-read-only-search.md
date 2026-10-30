## Fixes
### Providers

- Read-only OpenCode lanes can use the built-in grep, glob and list tools again. OpenCode runs those tools through ripgrep in its lane-private cache, and the read-only Seatbelt profile now grants process-exec for exactly that binary. Shell commands and edits stay denied ([#2400](https://github.com/mblauberg/provenant/issues/2400)).
