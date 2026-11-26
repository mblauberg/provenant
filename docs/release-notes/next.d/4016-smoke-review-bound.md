## Fixes

### Lanes and settlement

- `run smoke` no longer reviews the caller checkout's last commit. The bundled smoke graph's review node now runs after its writer and reads the writer's own scratch commit against the commit the writer branched from, so a smoke check is bounded by its single smoke change on both local and peer launches instead of taking as long as whatever release commit the caller happens to be on. A launch with no writer has no scratch commit to review and skips review with a sourced reason; `--set diff_base=BASE` still overrides the review base and its source is shown in the warnings. (#4016)
