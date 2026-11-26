## Fixes

### Lanes and settlement

- `run smoke` no longer reviews the caller checkout's last commit. The bundled smoke graph's review node now runs after its writer and reads the writer's own smoke change against the commit the writer branched from, or the existing linked checkout's HEAD immediately before the write. This keeps review bounded by the smoke change on local and peer launches even when a reused branch has accumulated history. A launch with no writer has no scratch commit to review and skips review with a sourced reason; `--set diff_base=BASE` still overrides the review base and its source is shown in the warnings. (#4016)
