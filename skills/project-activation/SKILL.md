---
name: project-activation
description: "Use when activating or inspecting Provenant project trust with `provenant project activate` or `provenant project status`. Not for low-level workspace trust maintenance, seat bootstrapping, provider login, or release."
---

# Project activation

Use the thin project front door from either installed client:

```text
provenant project activate [PATH]
provenant project status [PATH]
```

The path defaults to the current directory. Activation resolves the canonical
Git repository root, or the exact current directory for a non-Git project, and
then delegates trust to Fabric's existing workspace-trust owner. It does not
create a second registry, infer trust, prompt implicitly, or bootstrap a seat.

Read the report fields as follows:

- `trustedRoot` is the exact root recorded in `trusted-workspaces.json`.
- `configRoot` is independent: the nearest ancestor containing `.provenant/`.
- `seatExists` reports whether an active Fabric seat generation is present.
- `fabricReady` is true only when both the trust gate and an active seat are
  present. `missingDependencies` names the remaining setup.

An already trusted root is a successful no-op. A refusal is non-zero and should
be followed literally. Filesystem-root and home-wide refusals are security
properties of the existing trust owner.

If `missingDependencies` names a seat, use the existing Fabric bootstrap or
peer-provisioning path explicitly. Provider authentication and installation
are separate setup concerns. For low-level trust inspection or revocation,
use the existing `provenant fabric workspace ...` command and its owner.
