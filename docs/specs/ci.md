# Repository CI

## Carrying tested coverage forward

Anchor: `scripts/select-macos-checks` and `scripts/find-green-pr-head`

On a pull request, Rust and macOS coverage may carry forward from a verified
green prior head only when the direct tree delta contains exclusively plain
non-code paths. Missing evidence or any other delta uses the normal selection.

Why: the new tree must retain the code that the earlier CI result covered.

Owner: repository CI.
