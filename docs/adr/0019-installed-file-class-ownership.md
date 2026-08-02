# ADR 0019: Installed file-class ownership by product, instance, or seeded template

**Status:** Accepted 2026-07-30 (user, issue #530); amended 2026-07-31 (user,
issue #561); applies [ADR 0001](0001-personal-first-product-compatible.md) and
[ADR 0004](0004-per-domain-truth-owners.md)

## Context

Every installed file currently lives in one repository. `~/.agents` is at once
the product source tree, the user's configuration, and the thing the installer
projects into `~/.claude` and `~/.codex`. Ownership never had to be written
down, because there was only one owner.

The planned split ([issue
#532](https://github.com/mblauberg/provenant/issues/532)) ends that. A product
repository will ship the runtime, scripts, schemas, managed skills, workflows
and shipped policy; an instance repository will hold what one user's machines
share and commit. The moment those are two trees, every installed file needs an
answer to "who writes this, and what happens to my edit on the next update".

Without a per-class answer there are two failure modes, and both are silent.
An instance edit to a file the product also writes is reverted by the next
update, with no diff to notice. A machine-local absolute path committed into
the instance repository breaks the user's other machine, again with no diff to
notice. The file looks fine; it just names a directory that does not exist
there.

The material already sits on both sides of the line. `AGENTS.md` carries global
product doctrine (`AGENTS.md:1-31`) and is the symlink target of
`~/.claude/CLAUDE.md` and `~/.codex/AGENTS.md`, so it is simultaneously
product-authored and the natural home for one user's personal doctrine.
`config/risk-policy.json`, `config/review-profiles/`,
`config/delivery-profiles.json` and `config/adapter-compatibility.yaml` are
product policy the harness gates against. `config/model-preferences.json` and
`config/model-routing.json` encode which providers a particular user pays for.
`workflows/` is installed into `~/.claude/workflows` by
`scripts/install-workflows` and had no ownership story at all.

Two mechanisms already exist and neither had to be invented here. The
installation receipt written by `scripts/managed_installation_manifest.py`
records absolute target roots, per-entry source paths and content digests
beside the installed directory; it is machine-local by construction. And
`loadFabricConfig` (`runtime/agent-fabric/src/config/index.ts:180-213,224-285`)
already merges a global and a local trusted layer under a typed narrowing-only
rule: allow-lists intersect, workspace roots must stay contained, limits take
the minimum, and a widening attempt raises `CONFIG_WIDENING_FORBIDDEN`. What
was missing is that normal daemon startup only ever supplied a global path
(`runtime/agent-fabric/src/cli/default-daemon-options.ts:7-26`), so the local
layer was reachable from `--local-config` on `daemon-run` and nowhere else.

## Decision

Every file the installer touches belongs to exactly one of three owners. A
fourth class, machine-local derived state, is not owned by either repository
and is never committed anywhere.

| Class | Owner |
|---|---|
| runtime, scripts, schemas, skills (managed), workflows, HARNESS.md | product-shipped projection |
| risk-policy, review-profiles, delivery-profiles, adapter-compatibility | product-shipped projection |
| instance `agent-fabric.yaml` local layer | instance-owned (narrowing-only) |
| AGENTS.md, model-preferences.json, model-routing.json | seeded once, then instance-owned |
| third-party skill sources (`custom-skills/`) | instance-owned, committed |
| desired state (product version, mode) | instance-owned, committed |
| installation receipts, generated `skills/` projection, client links | machine-local, ignored |

The table above is the approved decision and does not change. Two further
artifacts are members of classes it already names, recorded here so the
classification is exhaustive against what the installer actually writes.

**Client links, in the last row, cover the generated client instruction files**
as well as symlinks: `~/.claude/CLAUDE.md` and `~/.codex/AGENTS.md`. The
installer may create either as a symlink to the instance `AGENTS.md` or as a
small generated file naming it, and an existing user-authored file is preserved
untouched. Both forms are machine-local: they name absolute paths on this
machine, they are never committed by either repository, and they carry no
content of their own. Because `AGENTS.md` is instance-owned after seeding, that
generated text points at the instance copy, while `HARNESS.md` stays product-
shipped and is addressed in the product. A client that read the product
`AGENTS.md` instead would never see the user's own doctrine, which is the
failure this decision exists to prevent.

Every installer before this decision wrote the product path into that file. That
text is stale, not foreign, so an upgrade migrates it in place rather than
refusing it: only the exact legacy path is rewritten, anything the user wrote
around it survives, and in a fused layout the two paths are the same string so
the file is untouched. Refusing it would have broken every upgrade from an
earlier install, which is a worse failure than the one being fixed. A symlink
the user made to either doctrine file is left alone, because repointing someone's
deliberate link is more than an upgrade should do, and a genuinely user-authored
file is still preserved and still fails closed.

**The product pointer, also in the last row**, is
`<instance root>/.agent-fabric/product-root.json`: a schema-versioned file
carrying the absolute path of this machine's product checkout, written by
`install-harness` on every install. That pointer exists to keep one invariant
absolute. **Committed instance state never contains an absolute machine path.**
The path a consumer needs is real and has to live somewhere, so it lives in the
receipt class, where it is ignored and regenerated. Relocating the product is
therefore always "re-run `install-harness`", never an edit to a committed file
and never an edit to a client configuration.

Its directory carries its own `.gitignore` of `*`, written before the pointer
itself so no window exists in which the absolute path is stageable. Git ignore
rules do not cross repository roots, so the product checkout's `.gitignore` says
nothing about an independent instance repository; the rule has to travel with
the file it protects. The pointer has exactly one writer,
`scripts/lib/product_root_resolver.py`, shared with [issue
#529](https://github.com/mblauberg/provenant/issues/529), so the file has one
format no matter which entry point produced it.

**On the hardening around these writes.** Instance files are staged beside their
destination and renamed into place, and the resolved parent directory is
required to stay inside the resolved instance root. The pointer writer carries
the same containment guard, because it reaches through that parent three times
(the `mkdir`, the `.gitignore`, and the staged pointer) and `mkdir` follows a
symlink like any other path. Together these close a symlinked-parent
redirection and narrow a check-then-write window.

None of it is a privilege boundary. An attacker able to plant a symlink or swap
a directory inside the instance root already holds the user's own privileges on
the user's own machine, which is the threat model [ADR
0001](0001-personal-first-product-compatible.md) accepts. The guards are here
because they are cheap and because they turn a silent misdirected write into a
loud refusal, not because anything downstream relies on them. One residual gap
is accepted on the same grounds and recorded rather than fixed: a destination
checked as absent can still be replaced between the check and the rename, so the
seeder reports `existing` for a path an attacker created in that window. The
rename cannot be redirected, so the outcome is a skipped seed, not a misdirected
write.

**On reconcile's two checks and rename rollback.** A `reconcile` first checks
the plan against the manifest. If accepted, it applies declared renames to the
target tree and manifest, recomputes the plan against the new manifest, and
checks again. The first check prevents a rename that the pre-rename plan says
conflicts. The second check catches a conflict that the rename itself created:
an item can shift from "missing" or "stale" to "conflicting" or
"custom-conflicting" because the renames changed what the installer believes
the tree contains.

If the second check detects a conflict, the installer rolls back: it unlinks
the new symlinks it created, restores the old symlinks it removed, and removes
the manifest entries for the new names and restores the entries for the old
names. The tree is then exactly as it was before the `reconcile` ran. Re-running
the same command will check the first plan against the original tree. The
conflict will be caught by the FIRST check on retry, with no partial tree or
skip-on-retry logic needed.

This does not make reconcile atomic. The residual window after the second check
passes but before the manifest is written is accepted: the installer has no
multi-path transaction, and closing that interval would need a locking or
snapshot design outside issue #561. The checks turn a conflict observed at
either plan boundary into a loud refusal, but they do not establish a privilege
boundary or a concurrent-writer guarantee.

**Note on the two rows that both involve the installer writing a file.** They
are not the same mechanism. The desired state is *created* by the installer as
the instance's own intent: no product template exists for it, and its content
describes the instance rather than the product. The `AGENTS.md` class is
*projected once*: a product template is copied, and from that moment the copy is
the instance's. Both are written exactly once and never rewritten, but the first
is authored and the second is inherited.

That table is the decision. The three owners mean:

**Product-shipped projection.** The product repository is the only writer. The
installer materialises these into the instance and into client homes as
symlinks and never edits them in place. An instance that wants different
behaviour changes the product, or narrows it through a layer the product
provides. It does not edit the projected file, because the next update
replaces it.

**Instance-owned.** The instance repository is the only writer. The installer
may create the file when it is absent; after that it never writes it again. The
user commits it and it travels between their machines.

**Seeded once, then instance-owned.** The product ships a template. The
installer copies it into the instance root only when no file is there, and
never again: not on update, not on repair, not when the template changes.
After the first write the file is instance-owned in full.

No general template hash-drift detection is introduced. Git is the drift
detector. Both repositories are Git repositories, so a user who wants to know
whether the shipped template moved past their seeded copy diffs the two trees;
a mechanism that recomputed the same answer from stored hashes would be a
second, weaker source of truth for something version control already owns.

### Desired state is not the receipt

These are two artifacts with two lifetimes and they are never merged.

*Desired state* is the instance's committed statement of intent: which product
version and which install mode this instance wants. It is path-free by
construction. It lives at `config/installation.json` in the instance root, it
is committed, and it is identical on every machine the instance is checked out
on. Its schema is:

```json
{
  "schema_version": 1,
  "contract": "installation-desired-state",
  "product": { "name": "provenant", "version": "0.1.0", "ref": "v0.1.0" },
  "mode": "fused"
}
```

`product.ref` is optional. `mode` is `fused` when the instance root and the
product root are the same directory (today's layout) and `split` when they
are two trees. No value in the document may be an absolute path or a
home-relative path; that is enforced on write and on read, not merely
documented.

The consequence readers should not miss: a path-free file cannot tell a
consumer where the product is. It carries intent, not location. Location comes
from the machine-local product pointer described above, and the resolution order
a consumer uses is:

1. `AGENT_FABRIC_PRODUCT_ROOT` in the environment, or an explicit
   `--product-root`;
2. `<instance root>/.agent-fabric/product-root.json`;
3. `AGENTS_HOME` in the environment;
4. `~/.agents`.

That is the contract the relocation-safe MCP shim resolves against ([issue
#529](https://github.com/mblauberg/provenant/issues/529)), and it is why a
relocated product needs no client-configuration rewrite: only step 2 moved, and
`install-harness` rewrote it.

*The installation receipt* is machine-local: it records the absolute target
root, per-entry source paths, content digests and install timestamps for what
is actually on this machine right now. It is written beside the installed
directory, it is ignored, and it is never committed by either repository. It is
also versioned by the product that wrote it, which has a consequence recorded
under Consequences below.

### The instance config layer narrows, it does not merge

The product ships `config/agent-fabric.yaml`. It is the global trusted layer
and it is the only place adapters, adapter commands, environment sources, the
listener and the credential selector may be set.

An instance may ship its own `config/agent-fabric.yaml`. Normal daemon startup
passes it as `localPath` into the existing `loadFabricConfig` merge. It gets
the rule that merge already implements: allow-lists intersect so an adapter
absent from the product list cannot be added, `activeAdapters` outside the
resulting allow-list raises `CONFIG_WIDENING_FORBIDDEN`, workspace roots must be
contained within a product root, and the concurrency limit takes the minimum of
the two. An absent instance file resolves to exactly the product-only
configuration.

One rule is added, because the merge as inherited did not enforce narrowing on
the field that matters most. **The instance layer selects from the product's
adapters; it never defines one.** A local `adapters` entry naming an adapter the
product does not define, or altering the command of one it does, raises
`CONFIG_WIDENING_FORBIDDEN`. The inherited merge overlaid the local adapter map
over the product's, so an instance file could keep the adapter id, keep the
allow-list, and substitute the program behind it. Nothing else in the merge
would notice: the id is allowed, the id is active, and the daemon would execute
whatever the instance named. That is widening of the most consequential kind,
because an adapter entry is the one place a trusted layer names an executable,
and it is the reason a local layer cannot be granted this field at all.

Restating the product's exact entry is permitted rather than rejected. An
instance file authored by copying the product's and then narrowing it is the
expected way to write one, and an identical entry substitutes nothing. The check
is on difference, not on presence.

No second merge engine is introduced, for any file class. Where a class needs
layered behaviour it uses this merge; where it does not, it is whole-file owned
by one side. There is deliberately no three-way merge anywhere in the
installer: a three-way merge needs a common ancestor per file, which is
precisely the state the seeded-once rule refuses to keep.

## Consequences

The installer now writes the desired-state file when it is absent and leaves it
alone forever after, seeds `AGENTS.md`, `config/model-preferences.json` and
`config/model-routing.json` into the instance root only when they are absent,
rewrites the machine-local product pointer on every run, and continues to write
its receipt outside the instance repository. `.gitignore` names both receipt
files and the `.agent-fabric/` pointer directory, so nothing in the receipt
class can be committed by accident.

Split-layout startup binds the product root for the global config layer,
`adapter-compatibility.yaml`, the compatibility schema and the `${AGENTS_HOME}`
token, and the instance root for the local layer. The fused layout still has
both roots equal, with both defaulting to `~/.agents`. After this change
`AGENTS_HOME` selects only the product root, so an ambient value naming another
checkout leaves the instance root at `~/.agents` and makes the bindings split
even without an explicit instance-root selection.

Root resolution itself is `resolveFabricRoots`
(`runtime/agent-fabric/src/domain/fabric-roots.ts`, [issue
#528](https://github.com/mblauberg/provenant/issues/528)): an explicit
`--product-root` or `--instance-root` flag, then `--agents-home` for both roots,
then the matching `AGENT_FABRIC_PRODUCT_ROOT` or `AGENT_FABRIC_INSTANCE_ROOT`,
then `AGENTS_HOME` for the product root, then `~/.agents` for any root still
unresolved. This decision adds no second resolver. The instance root has no
`AGENTS_HOME` fallback. One consequence of that precedence is worth stating
plainly: an `--agents-home` flag sets both roots, so a split layout is
expressed by the two explicit root inputs, not by `AGENTS_HOME`, which after
the split names the product because it is the token the shipped adapter
commands expand against.

Layering is uniform across every consumer that reads trusted configuration.
`provenant status`, `provenant doctor` and `agent-fabric adapter executable`
compose the product global layer with the optional instance local layer exactly
as daemon startup does, and bind `adapter-compatibility.yaml` to the product.
A diagnostic that read one layer would answer a different question from the one
an operator is asking. On a split machine it could report a healthy widened view
the daemon would refuse to start on, fail on a valid instance that holds no
product-owned file, or silently omit adapters the product activated. When an
operator pins a single file with `--trusted-config` or `--config`, that file is
the whole configuration and no local layer is added, because naming one file is
a request to inspect exactly that file.

The review-profile catalogue is bound to the product root, matching its row in
the table; `model-routing.json` stays bound to the instance root, matching its
own. Resolving the catalogue under the instance made `status` and `doctor` fail
deterministically on a correct split install, because a split instance does not
carry that product-owned file at all. Separately the MCP server resolves its roots ambiently
(`runtime/agent-fabric/src/mcp/credentials.ts:159`); that path belongs to [issue
#529](https://github.com/mblauberg/provenant/issues/529) and is referenced here
only so the table is complete.

Two consequences fall to [issue
#532](https://github.com/mblauberg/provenant/issues/532) rather than to this
ADR. `custom-skills/` becomes the instance-owned committed source directory for
third-party skills and `skills/` becomes a generated, ignored projection over
the product's managed skills plus that directory; until the trees are actually
separate there is nothing for either to project. And a split instance whose
tree is outside the product root is not a workspace root by default, because
the product config's `workspaceRoots: ["${AGENTS_HOME}"]` expands against the
product; the instance layer cannot add it, since adding is widening. The
product config must list a containing root, or the instance must be passed as
an additional workspace root at startup.

MCP client registration falls in the machine-local, ignored class:
`~/.claude.json`, `~/.codex/config.toml` and the other client configuration
files written by `scripts/configure-agent-fabric-mcp.py`. It is named here for completeness of the table only. The registration
path itself is owned by [issue
#529](https://github.com/mblauberg/provenant/issues/529) and is not changed by
this decision.

**Product downgrade below a managed-class boundary is not supported.** The
receipt is written by the product that installed, and an older product does not
know about classes a newer one introduced. Rolling the product back below the
revision that took ownership of `skills/_shared` ([issue
#531](https://github.com/mblauberg/provenant/issues/531)) strands an
installation that was upgraded past it: the older `manage_installation.py` does
not carry `_shared` in its catalogue, so the receipt entry the newer product
wrote reads as an entry outside the catalogue, and the older integrity check
reports it as an unmanaged extra rather than repairing it. The recovery is to
run `uninstall-managed` with the newer product still in place and then install
with the older one, which is exactly the sequence that reduces the receipt to
what the older product understands. This generalises: any future class the
product takes ownership of moves the floor for supported rollback, and the
floor is not tracked by any automated check.

### Addendum — 2026-08-03

The layering paragraph above names `provenant status`. No such command exists
and none was ever accepted: `scripts/provenant` delegates `route`, `worktree`,
`check`, `fabric`, `doctor` and `project` only, and anything else exits 2, which
matches the command set ADR 0013 accepted. The diagnostic meant is
`provenant fabric status` (`runtime/agent-fabric/src/cli/main.ts`), which is how
every other document writes it. `provenant doctor` and
`agent-fabric adapter executable` are named correctly, and the layering claim
itself is unchanged.

## Rejected

- **A merge for every file class**, so that an instance edit and a product
  change to the same file both survive. Rejected because it requires a stored
  common ancestor per file (a third copy of every seeded file, kept in step
  forever) to answer a question the two Git repositories already answer for
  free, and because the conflict cases it creates would have to be resolved by
  the user anyway, without the tooling Git already gives them for exactly that.
- **General template hash-drift detection**: record the digest of each seeded
  template at install and warn when the shipped template later differs.
  Rejected for the same reason and one more. It reports drift the user cannot
  act on without diffing the trees anyway, and it turns every routine product
  change to a template into a warning on every machine, which trains the user
  to ignore the warning.
- **Committing the installation receipt** so that installed state is visible in
  the instance repository. Rejected because the receipt's content is absolute
  target roots and per-machine digests; committing it makes every machine's
  install dirty the working tree of every other machine's, and the desired-state
  file already carries the part that is genuinely shared.
- **Making `AGENTS.md` a pure product projection.** Rejected because it is the
  file `~/.claude/CLAUDE.md` and `~/.codex/AGENTS.md` point at, and it is where
  a user records doctrine that is theirs and not the product's. A projection
  would silently revert that on update, which is the exact failure this ADR
  exists to prevent. Seeded-once keeps the product's starting text without
  claiming the file.
- **Making `model-routing.json` and `model-preferences.json` product-shipped**,
  on the grounding that routing policy is product behaviour. Rejected because
  their content is which providers and models a particular user is entitled to
  use. That is instance fact, not product policy, and the harness must run for
  a user who subscribes to a different set.
- **Letting the instance `agent-fabric.yaml` widen anything**, for example add
  a locally built adapter. Rejected because the trusted global layer is where
  adapter commands are declared, and a local layer that can add a command is a
  local layer that can execute arbitrary code with daemon authority. An
  instance needing a new adapter changes the product.
- **Rejecting an `adapters` key in the local layer outright**, rather than
  rejecting only entries that differ from the product's. Rejected because the
  natural way to author an instance layer is to copy the product's file and
  delete what you do not want, and a strict rejection would refuse that file for
  restating entries it changes nothing about. Difference is the property that
  matters; presence is not.
- **Recomputing `mode` in the desired state on every install**, so that a
  fused-to-split migration is picked up automatically. Rejected because it
  contradicts the ownership this ADR establishes. Desired state is the
  instance's intent, and an installer that rewrites it is no longer reading the
  instance's intent but its own. Location is not intent: a migration changes
  where the product lives, which the machine-local pointer already records and
  `install-harness` already rewrites. A user who genuinely wants to change the
  declared mode edits one committed line, and Git shows it.
