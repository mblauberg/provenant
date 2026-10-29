## Behaviour changes
- Remove free routes from bundled pools and restrict Gemini Flash and OpenCode free routes to read-only work with configurable result checks. Failed checks can automatically fall back once in host-owned runs while retaining the failed attempt; nested dispatch selects the reliable fallback before launch.
- Add `route list --valid` to list catalogued model/effort pairs accepted by resolution and the configured allowance floor, without probing live providers.
