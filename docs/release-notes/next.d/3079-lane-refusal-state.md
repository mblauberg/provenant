## Fixes
### Lanes and settlement

- A lane whose provider reports a refused action and produces no answer now settles as `permission_denied` instead of a generic failure, and its reason names the denied action. A refusal the provider swallows and works around is not observable at settlement and still settles as `succeeded` with a denial note (#3079).

## Additions
### Lanes and settlement

- A retrospective lane can read agent history when the owner declares the directory (`--read-root ~/.claude/projects`, `~/.codex/sessions` or `~/.codex/archived_sessions`). Only that directory opens, for reads only: credential files beside it and every other credential store stay denied (#3079).
