## Fixes
### Pipelines and graphs
- Review panels preserve an authored success minimum, status reads share their invocation deadline, and quota errors retain their retry time. Shared lanes with an empty write scope commit their work, and policy reports identify the source of dispatch settings. (#4126)
- A launch whose guardian never received the whole launch message, or refused it, settles as never spawned instead of a fenced, unconfirmed handoff. A lane whose deadline is warn-only or unset launches with no enforced wall bound instead of being refused. (#4126)
