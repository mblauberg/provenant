## Fixes
### Configuration warnings
- Warnings for ignored `[[adapter_capabilities]]` rows, unreadable adapter configuration and skipped capability-matrix entries now carry a structured `detected` source naming their warning code, as required by `docs/specs/cli.md#warnings-are-typed-values`. (#4106)
