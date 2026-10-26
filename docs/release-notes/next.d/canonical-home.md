## Behaviour changes

- Install and check canonicalise `HOME` before resolving their targets. A
  symlinked home now resolves to its target, and a relative `HOME` is rejected.
