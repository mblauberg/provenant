## Fixes
### Lanes and settlement
- A lost lane token now retries on the same route and keeps its `lane_token_lost` cause; a pinned read-only reviewer is no longer reported as `auth_required` or `fallback_route_pinned`. The advance code comes from the sealed stop through one table. (architecture review F5, 26 Nov)
