## Fixes

- Stop a supervisor from retrying exit publication forever when its attempt directory is removed, including when the sync reports a durability failure rather than a missing path. Removal is judged by the held directory's identity, so a renamed attempt or a replaced pathname is never mistaken for a removed one. A provably removed attempt also skips terminal settlement, which has nowhere left to publish.
