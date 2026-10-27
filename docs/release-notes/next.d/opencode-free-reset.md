## Behaviour changes

OpenCode free-tier limit events reporting `retryAfterMs` (available from
OpenCode 2.1.2) now cool the `opencode-free` pool until the stated reset time
instead of falling back to an uncalibrated default period. After the reset
passes, admission restores the free pool across models. Stated retry timings
are clamped to at most 24 hours, and missing, negative, zero or non-numeric
values retain the existing fallback cooldown.
