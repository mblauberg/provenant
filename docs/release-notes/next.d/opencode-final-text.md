## Fixes

OpenCode runs that emit their final text and exit cleanly now succeed even
when OpenCode omits the closing `step_finish` event. Error events, signals,
non-zero exits and timeouts still prevent success.
