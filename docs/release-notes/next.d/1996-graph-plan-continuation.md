## Additions

- Fabric graph plan nodes expand validated, bounded fragments with default senior routes, scoped writer scheduling and existing integrate delivery for one final PR.
- Structured stopped-early handoffs create one continuation per writer on the same checkout, within a configurable graph budget. Graph status exposes expansions, handoffs and remaining budgets.
- Refused planner fragments survive resume for limit extensions. Malformed optional handoffs warn without blocking successful delivery; writer admission covers overlapping or unspecified scopes, and inactive recovery writers no longer block later fragments.
