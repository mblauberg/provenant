## Fixes

- Keep writer lane Git objects private until the host hash-verifies them at settlement. Lanes cannot alter existing shared loose objects, packs or indices; integration and delivery refuse corrupt object content.
