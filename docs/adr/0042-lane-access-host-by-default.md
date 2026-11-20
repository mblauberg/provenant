# ADR 0042: Lane access defaults to host

**Status:** Accepted

## Context

The access audit found that selecting a sandbox changed the environment,
provider home, Git configuration, credential access and process access at once.
Those coupled restrictions can prevent ordinary same-host work.

## Decision

Resolve one lane access model per attempt: `host` by default, or `isolated` as
an explicit choice. Host access inherits the native environment and credentials
for same-host work. Isolated retains the pre-2.5.3 restrictions. Sandbox mode
and destination credential grants remain separate decisions. The resolved
value and its source are recorded and shown at launch.

## Consequences

All access enforcement shares that one resolved choice instead of introducing
independent switches. Doubtful same-host states warn and proceed, naming the
effective access and its source, rather than refusing. Other lanes' work, data
integrity, merge, release and ruleset authority, and credentials crossing hosts
or ungranted providers remain boundaries.
