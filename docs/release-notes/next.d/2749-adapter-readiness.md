## Fixes
### Providers and routing

- Keep credential watch files, login guidance, compaction controls and allowance policy in the adapter registry (#2749).
- Classify provider sign-in failures as `auth_required`, preserving read-only route fallback and prelaunch authentication refusals (#2744).
- Show host readiness separately from launch permissions in route previews, omit prompt guidance from human output and report non-secret sign-in status in host diagnostics (#2468).
