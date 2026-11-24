## Fixes

### Pipelines and graphs

- `provenant run review --pr N --head SHA` now launches in an attempt-private, read-only detached view of that head and its prompt says the subject is already checked out there, so a review lane no longer checks the PR head out in the project's primary checkout and displaces concurrent writers. (#3943)
