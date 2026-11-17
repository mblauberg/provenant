## Fixes

### Accounts, quota and routing

- Classify credential stores once for the actual launch source and cwd. Shared stores deny the whole subtree outside lane roots, including undeclared files; declared credential files inside a lane root refuse with their origin and path, checking both directions of symlinks. HOME-rooted stores render as denials without relaxing broad-grant checks and retain independent protection for default state. Selected authentication uses exact endpoints; sibling credentials and their external symlink targets stay denied. Dedicated stores and relocated Codex homes keep whole-store protection. #3392

HOME-rooted stores admit validated provider executables and runtime packages without exposing declared credentials. External directory links keep their target subtrees denied and refuse conflicting lane or runtime authority.
