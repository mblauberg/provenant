## Fixes
### Configuration warnings
- Warnings for ignored `[[adapter_capabilities]]` rows carry the configuration layer and key as their structured source. Warnings for unreadable adapter configuration and skipped capability-matrix entries carry a structured `detected` source naming their warning code. (#4106)
