## Fixes

- `provenant upgrade` now refreshes the bundled macOS app once, verifies its downloaded archive without downloading it again, and swaps it while retaining the previous app until the new bundle is in place. A failed app refresh no longer rolls back committed instruction files (#2417).
