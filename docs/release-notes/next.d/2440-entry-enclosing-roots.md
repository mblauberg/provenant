## Fixes
### Access

- Entry checks now refuse a `--read-root` or `--write-root` that encloses a credential root, such as `~/.config`, and name the flag and path in the CLI message. Refused child roots settle as refusals. Lane entry and the confinement compiler share one rule, so a root is refused when submitted rather than later at compile or child launch ([#2440](https://github.com/mblauberg/provenant/issues/2440)). The same rule now uses the host's canonical home, so a nonconventional home such as `/srv/team/alice` and any root enclosing it are refused at entry too ([#2443](https://github.com/mblauberg/provenant/issues/2443)).
