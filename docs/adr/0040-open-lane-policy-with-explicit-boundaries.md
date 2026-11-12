# ADR 0040: Ambient lane access with explicit mandatory boundaries

**Status:** Accepted

Changed: 2026-11-08; 2026-11-12 (sandbox mode, #3246).

## Context

This personal harness should give an agent the access it would have when called
from the CLI, including children launched through the host. Restrictions outside
[ADR 0035's four boundaries](0035-four-hard-boundaries-and-flexible-everything-else.md)
should warn or be host-configurable. [ADR 0039](0039-graph-of-templates.md)
requires the same launch policy for children and direct launches.

Explicit denials protect credentials and host state while allowing ordinary
access. Read-only sources remain protected from writes.

## Decision

Use the existing macOS renderer with `(allow default)` and explicit denials.
There is one renderer and no raw SBPL configuration, compatibility reader or
alternate profile mode. Ordinary filesystem, process and network access is
ambient unless a mandatory boundary, explicit lane request, adapter offline
requirement or optional host policy restricts it.

The policy contract lives in
`tools/front-door/crates/confinement/src/permissions/policy.rs#LanePolicy` and
`tools/front-door/crates/confinement/src/run_config.rs#RunConfig::policy_for_launch`.
The launch edge consumes
this contract at `confinement_request`, derives complete mandatory roots from the
verified launch context, reports once at launch and enforces them with the ambient
default. An empty input list must never mean that mandatory protection is
absent. Resolution is data, not proof of enforcement.

### Optional host policy

The sole optional host policy lives in the selected state configuration; there
is no project, lane or child override layer. The typed owner is
[`RunConfig`](../../tools/front-door/crates/confinement/src/run_config.rs), with
operator guidance in the [configuration runbook](../runbooks/configuration.md#run-host-policy).
The source and warnings are observable once per launch attempt.

Optional restrictions may narrow ambient access but cannot remove mandatory
boundaries. Path denials match subtrees and apply even when the named path does
not yet exist. They accept no executable policy or allow exceptions. Explicit
host/profile denials take precedence over ordinary grants and mandatory
exceptions. The effective network is the narrower of the host ceiling and lane
request; adapter offline requirements can narrow it further.

Invalid optional settings warn without echoing rejected values and preserve
valid siblings. This tolerance never extends to authority controls or malformed
configuration files. A valid host restriction that makes a provider unusable is
an enforcing denial, not malformed configuration.

### Mandatory root inventory

[`policy.rs#MandatoryRoots`](../../tools/front-door/crates/confinement/src/permissions/policy.rs)
and its credential registries own the concrete inventory. Do not duplicate that
path list in policy prose. The host adds registered identities, relocated
credential stores, canonical aliases, selected endpoints, state roots, host Git
and protected checkouts. Optional settings cannot remove these protections.
Absent paths remain protected, and aliases must not bypass the boundary.

Credential and preservation roots deny content reads, mutation and execution
except for typed grants below. The selected state root and its database/lock
siblings are immutable. Source and host Git authority comes from verified
handles and the selected branch, never lane-supplied strings. Extra write roots
cannot overlap protected roots; the typed host-write exception is distinct from
a dispatch's extra write request. Shared validation and launch revalidation
must agree on those boundaries.

### Permitted exceptions

Exceptions are host-derived typed grants, never optional configuration:

1. **Selected authentication.** Only the selected, fixture-backed mechanism
   receives its declared credential reads. Refresh writes are limited to that
   mechanism's declared exact files; they confer no create, unlink or rename
   authority over credential directories. Unselected identities and unrelated
   keychain services stay inaccessible. Provider profiles own the concrete
   endpoints, including OpenCode's selected login stores. Credentials never
   cross hosts. Native keychain services remain outside filesystem confinement,
   so identity verification is still necessary.
2. **GitHub CLI access.** Effective open-network lanes, including readers, may
   read immutable host GitHub CLI stores unless a profile explicitly denies
   credential access. Narrower network requests receive no exception. The token
   can write to GitHub: read-only describes host-source access, not GitHub
   permissions, and the owner accepts that. Launch-token/helper delivery is
   separately controlled by strict `lanes.publish`; malformed authority settings
   refuse configuration. Transient host token lookup failures are retried and
   unresolved failures warn without exposing the credential.
3. **Attempt-private state.** Scratch, mutable provider stores and continuation
   state bind to the exact attempt's host-derived private paths. Cursor chats,
   OpenCode writable state and Claude project transcripts use those bindings;
   they do not reopen shared credential/configuration parents. Selected inputs,
   immutable staged skills, process snapshots, build-slot coordination files and
   the Fabric socket receive only their declared grants. A grant to one attempt
   does not open another attempt or the whole runs store. Target seeds and
   credential authority stay inaccessible. The Fabric state-read exception is
   described under Known limits.
4. **Installation and packages.** Verified immutable installed assets and
   validated standalone provider packages may be read/executed. Their parents,
   receipts and sibling credential/config/session stores remain protected.
5. **Source and Git.** Read-only attempts deny source mutations. Ordinary
   read-only exceptions apply only to declared paths; live source and isolated
   views do not inherit those exceptions into the live checkout. Writers use a
   verified worktree and complete private Git directory. Host Git, shared refs,
   objects and the checkout's `.git` marker remain immutable; the verified host
   object database is only a read-only alternate. Host import and delivery retain
   object-integrity, custody and expected-ref checks.

The renderer excludes only verified exception paths and operations from parent
denials, without reopening siblings. It explicitly protects ancestors against
rename/unlink and denies new hard links across boundaries. Native enforcement
checks, including path swaps and keychain IPC, must prove the result; ordering
or a rendered profile alone is not enforcement evidence.

### Backend and renderer acceptance

macOS uses the enforcing backend. Linux retains its typed backend-unavailable
refusal; it must never fall back to unconfined execution. Fixture checks cover
policy resolution, mandatory roots, exception boundaries and provider private
state. Configured-host native checks cover actual enforcement. A lane returning
`ProbeFailed`/skip supplies no native enforcement evidence.

## Sandbox mode

Writers on trusted routes run without Seatbelt, like the same CLI started by
hand, so they can share host build caches and services. A lane stays sandboxed
when its route is listed in `domains.run.sandboxed_routes` (the OpenCode Zen
tier, whose models may train on prompts), when a sandboxed lane submitted it,
when its network is narrowed, or, under the default `auto`, when it is
read-only, because only the sandbox enforces those. The `lanes.sandbox` knob
sets the rest. The
[configuration runbook](../runbooks/configuration.md#lane-sandbox-mode) owns
the rules and what holds in each mode.

For an unsandboxed lane, ADR 0035's lane Git and credentials boundaries rest
on the private Git directory, host-owned delivery, writer custody and the
stripped environment, not on kernel denials: route trust replaces the file
and shared-Git write denials, and `host_policy` path denials do not apply.
Such a lane can also write the host state that records the floors, and
detached daemons it starts escape cleanup.
Overturn when a trusted route is traced to writing shared Git state or using
an on-disk credential it was not given.

## Known limits

Login shell startup files stay unreadable: `/etc/zprofile`, `/etc/profile` and
the zsh and bash files in the home directory or `ZDOTDIR`. They reorder `PATH`,
which puts a system Git ahead of the lane's Git wrapper, and a writer's ordinary
`git add` then fails on the protected Git directory. They can also source
secrets. A lane keeps the `PATH` of the environment that launched it.

A lane's Fabric MCP server runs inside its sandbox and reads the state directory's
`config.toml`, `host-id`, `projects` and Fabric database directly, so the lane's
other commands can read them too, . Nested Fabric and nested
launches need these reads. Serving them through the Fabric socket would close this
gap and let those files become inaccessible.

Boundaries are enforced by pathname. A hard link to a protected file that already
exists outside granted roots, such as an object in an earlier `git clone --local`,
is not covered. New hard links remain denied. The owner accepts this limit;
launches are not refused merely because a file has multiple links.

Credential directory symlinks are inventoried without following directory
links, up to a depth and entry limit per root. Hitting a limit warns and leaves
the remaining links uninventoried. Links whose targets cover the lane source or
scratch are skipped with a warning. The walk skips OpenCode's `log`, `storage`, `snapshot` and `shell` session trees, so a credential link placed inside them is not inventoried. Links introduced after inventory, or links
that cannot be resolved or represented during the walk, cannot be covered by
this walk. Unresolved declared endpoint links are also skipped with a warning.
Directory targets outside the host home are skipped with a warning; file targets remain inventoried.
Executable targets and targets inside Provenant state are skipped without a warning: provider stores link to their own binaries and installed instructions.
`com.apple.SecurityServer` stays reachable unless a profile denies keychain or credential access, because TLS root lookup on macOS needs it. Keychain files keep their own denials.

## Consequences

Optional host policy is observable and tolerant, while the boundaries remain
host-owned. More ambient capability means the renderer can no longer rely on
omitting allows: every confidentiality and integrity requirement needs an
explicit denial and a tested exception. The shared inventory and native enforcement checks must evolve together so
incomplete roots cannot silently weaken enforcement.

Overturn when: lanes run work for anyone other than the host's owner.
