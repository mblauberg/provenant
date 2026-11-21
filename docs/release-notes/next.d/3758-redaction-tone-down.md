## Behaviour changes

### CLI and install

- Redaction now has only `off` (the default) and `secrets`: local output, diagnostics and captured command tails stay raw, while `secrets` masks known credential values and token shapes in text published off the host, such as forge bodies and comments. A config value of `full` no longer masks locally or scans assignments; it warns with its config source and is treated as `secrets`, and no command is refused for it. (#3758)
