## Fixes

### Configuration and workflow

- A repository whose Git remote is not `origin` and whose default branch is not `main` or `master` now follows its declared names through handoff push and branch recreation, cloud fetch and `ls-remote`, GC upstream probing, PR base resolution and delivery. The last compiled-in fallback in PR base resolution now uses the one resolved forge remote; a branch with its own tracking configuration keeps that remote. With no remote HEAD, a declared `git.protected` set (and a declared default branch) controls which branches delivery protects. `provenant config ports` prints the effective remote, default branch and protected set with the layer and key that supplied each, so a non-conventional name can be confirmed before a workflow runs. Conventional literals may still appear as shown defaults; they no longer decide behaviour. (#3288)
