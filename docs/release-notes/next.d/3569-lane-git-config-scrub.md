## Fixes
### Git and safety
- Lanes see only the token credential helper. The lane Git wrapper exports `GIT_CONFIG_NOSYSTEM` even when it supplies an identity config, launch no longer strips it, and the lane environment drops `GIT_CONFIG_COUNT`, `GIT_CONFIG_KEY_*`, `GIT_CONFIG_VALUE_*` and `GIT_CONFIG_PARAMETERS`, so a host helper such as `osxkeychain` can no longer be reached from a lane. (#3569)
