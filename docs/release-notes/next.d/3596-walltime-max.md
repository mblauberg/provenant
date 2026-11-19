## Fixes

### Core and graph state

- `WallTime::MAX` now names the greatest instant the wire accepts (253_402_207_200_000_000 microseconds, `9999-12-30T22:00:00.000000Z`), replacing a documented value that jiff rejects. The graph checkpoint envelope derives its latest wake time from that constant instead of silently falling back to the Unix epoch. (#3596)
