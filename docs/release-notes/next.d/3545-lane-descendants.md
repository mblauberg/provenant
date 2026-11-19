## Fixes

### Lanes and settlement

- A lane's own background processes no longer outlive it. When the provider
  exits, for any reason, the supervisor ends its recorded descendants and any
  process carrying the attempt's sandbox mark, re-checking each identity before
  every signal. Recovery after a lost supervisor does the same from the
  recorded identities. The exit records one "killed N surviving descendants"
  warning. Processes found only by working directory are never signalled.
  Background services an unsandboxed lane starts (for example a compiler-cache
  server) now end with the lane. (#3545)
