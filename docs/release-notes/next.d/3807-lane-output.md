## Fixes

### CLI and install

- `output` now leads with settlement, verdict, findings and verify and keeps lane prose behind `--full`, so a compact read no longer embeds a head-and-tail of the result text. `--tail N` still bounds bytes and `--tail-lines N` bounds lines, both clamped consistently for local, peer, composite and event reads; fields that do not apply are omitted instead of printed as unavailable. Human output no longer carries ANSI escapes when stdout is not a TTY, and `--full` uses the parsed global flag rather than a raw argv scan. (#3807)
