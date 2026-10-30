## Fixes

- `provenant upgrade` now delegates bundled macOS app refresh to the packaged installer, which also owns the direct install path. Swaps keep the previous app through installation and menu-bar restart, and interrupted or failed refreshes restore it without rolling back committed instruction files (#2417).
