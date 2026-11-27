## Fixes

### Lanes and settlement

- `run smoke` and `upgrade --verify` now run one hermetic lifecycle. A deterministic nonce fixture repository is built under `domains.smoke.fixture_root` (default `.local/state/provenant-smoke`, home-relative) in the home of every target host, locally and on peers through the `SmokeFixture` peer protocol (protocol 16). The graph's `review-write` node reviews the writer's own commit against `diff_base`, which defaults to the fixture head and must resolve inside the fixture. `review-fixture` reviews the fixture's own commit when `--set write=false` skips the writer, and `fixture_base` names the fixture commit the write node starts from. The `smoke-write` and `smoke-review` node names and the staged fixture are gone, and the verdict counts only when the nonce appears in both the read and the review result. `--set diff_base=BASE` still overrides the review base and its source is shown. (#4016)
