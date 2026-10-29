## Behaviour changes
### Chair, inbox and console

- `chair log append` now reports the stable error code `IN_LANE` when a lane tries to append, matching `workflow session save`. It was `CHAIR_LOG` (#2273).
- `pr open` and `pr contributions --bind` now report specific error codes (`PR_FILE_UNREADABLE`, `PR_SUMMARY_EMPTY`, `PR_ATTEMPT_STATE`, `PR_PROJECT_UNBOUND`) where they reported `PR` or `PR_CONTRIBUTIONS` (#2273).
- `workflow session` commands now report `SESSION_PROJECT_MISSING` or `SESSION_PROJECT_MISMATCH` when the project cannot be resolved or does not match, and `INVALID` for an unusable checkout or state root, where they reported `ROOT` (#2273).
