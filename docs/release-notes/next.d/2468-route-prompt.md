## Fixes
### Accounts, quota and routing

- Human route previews no longer display instructions intended for a launched model (#2468).
- Route previews describe readiness and distinguish missing or expired observations from provider refusals (#2468).
- Quota and hosts doctor show the same account-scoped readiness evidence and observation time (#2468).
- Route lists and resolution show recorded model-scoped readiness for matching accounts (#2468).
- Hosts doctor identifies installed adapters without account observations as unknown (#2468).
- Hosts doctor checks local sign-in status for installed Codex and Cursor adapters without recorded account observations (#2468).
- Quota ticks refresh missing readiness records without waiting for a fresh quota snapshot to expire (#2468).
