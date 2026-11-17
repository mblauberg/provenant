## Fixes

### Accounts, quota and routing

- Classify credential stores once for the actual launch source and cwd. Shared stores deny the whole subtree outside lane roots, including undeclared files; declared credential files inside a lane root refuse with their origin and path. Dedicated stores and relocated Codex homes keep whole-store protection. #3392
