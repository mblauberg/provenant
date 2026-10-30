## Fixes

- Submitting a writer whose `--worktree` belongs to another project now refuses with `WRITER_PROJECT_MISMATCH` and names both project roots, instead of the generic `WRITER_SCOPE` refusal that suggested a write-scope problem ([#2366](https://github.com/mblauberg/provenant/issues/2366)).
