## Fixes

### Lane settlement

- Settlement reuses held writer custody during cleanup, caps failures before
  object import, and honours the durable retry deadline before recovery passes.
  Exhausted recovery stops with its cause visible as blocked in status; retries
  pending for three minutes are flagged as stalled. (#3537)
