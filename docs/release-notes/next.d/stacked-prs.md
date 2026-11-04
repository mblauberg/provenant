## Behaviour changes
### Skills and instructions

- Document GitHub stacked pull requests for dependent slices: when to stack, `gh stack` setup, review against each PR's own base, and bottom-first landing. `provenant pr land` cannot land stacked PRs yet (#2849).
- Agents may rebase, rewrite and force-push with lease their own unmerged branches without asking. Rewrites of or pushes to shared branches still need the user.
