## Fixes
### Providers and routing

- Keep credential watch files, login guidance, compaction controls and allowance policy in the adapter registry (#2749).
- Classify provider sign-in failures as `auth_required`, preserving read-only route fallback and prelaunch authentication refusals (#2744).
