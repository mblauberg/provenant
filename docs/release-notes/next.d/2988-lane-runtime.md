## Fixes
### Lanes and settlement

- Route aliases skip exhausted members and retry another member after a quota limit that produced no work. When every member is exhausted, the refusal names the members and earliest known reset; exact `--model` launches stay on that model (#2988).
- Read-only lanes can use authenticated `gh` and shell here-documents while repository files remain protected. Command denials identify the path and operation (#2989).
- Claude lanes retain the requested permission mode when launched from a Claude parent, and malformed results report the provider’s error (#2990).
