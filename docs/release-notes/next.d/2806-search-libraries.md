## Fixes
### Telemetry and providers

- Fixed read-only OpenCode search to grant access only to dynamic libraries outside lane state and credential stores, regardless of their install location (#2806).
- Fixed partial quota percentages in the console to round down, preserving their lower-bound meaning.
