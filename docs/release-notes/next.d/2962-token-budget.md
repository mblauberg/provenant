## Fixes
### Lanes and settlement

- Claude writer lanes stop on their fresh-token budget (default 500000); cached reads do not count. After the provider stops, the host attempts to commit uncommitted work for continuation. In-progress merges are preserved, and commit errors remain visible alongside the budget timeout. Codex reports usage only after the turn, so its budget is not enforced. Current lane providers cannot receive the 90% steer warning; the receipt records that limitation. Build-slot waits stay off the wall deadline for at most `lanes.slot_wait_seconds` in total (#2962).
