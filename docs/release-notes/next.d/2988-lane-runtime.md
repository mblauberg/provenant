## Fixes
### Lanes and settlement

- Route aliases skip members with fresh exhaustion evidence; stale evidence ranks members last without refusing them. Aliases retry another member after a quota limit that produced no work. When every member has fresh exhaustion evidence, the refusal names the members and earliest known reset; exact `--model` launches stay on that model (#2988).
- Read-only lanes can use host GitHub authentication without saving tokens in run files. Command denials name the operation, and the captured output holds the path (#2989).
- Claude lanes retain the requested permission mode when launched from a Claude parent, and malformed results report the provider’s error (#2990).
