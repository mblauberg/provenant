## Fixes

### Telemetry and providers

- Executor-captured stdout and stderr are now masked on the stream by local-disclosure redaction when local redaction is enabled (redaction=full), ahead of any capture bound, so a secret that straddles a limit cannot leave a fragment in an error, a retained tail or a stored result. Sites that need raw bytes (byte-exact Git reads, credential retrieval, parsed port responses) opt out with a named reason rather than a silent flag; the redundant per-site redactors at delivery and seed-build capture sites are gone, credentials passed to the child are registered with the default mask, and seed-build argv is masked before it is echoed into an error (#3747)
