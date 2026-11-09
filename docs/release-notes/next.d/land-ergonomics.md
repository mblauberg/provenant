## Fixes
### Pull requests

- `land --head` accepts any object name that resolves to one commit in the repository, such as a short ID or a branch, and uses its full ID. An ambiguous abbreviation is refused with Git's reason and the corrected command (#3079).
- `pr watch` waits up to three minutes for the first check to be reported after a push, then fails with `CI_NOT_SCHEDULED` naming the missing checks, instead of failing at once with `no checks reported` (#3079).

### Pipelines and graphs

- A review ending in one verdict followed by a spaced, pretty-printed handoff block keeps its verdict at any result size; a result too large for the retained window loses its findings, not its verdict (#3079).
