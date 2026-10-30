## Fixes
### Access

- Entry checks now refuse a `--read-root` or `--write-root` that encloses a credential root, such as `~/.config`, and name the flag and path in the CLI message. Refused child roots settle as refusals. Lane entry and the confinement compiler share one rule, so a root is refused when submitted rather than later at compile or child launch ([#2440](https://github.com/mblauberg/provenant/issues/2440)).
