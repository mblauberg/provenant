## Fixes
### CLI and install
- Batched `pr status` readings and their GraphQL fallback exclude declared review gates from CI counts, so a pending review gate does not make passing CI appear pending. (#4042)
