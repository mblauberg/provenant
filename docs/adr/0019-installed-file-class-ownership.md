# ADR 0019: Installed file-class ownership by product, instance, or seeded template

**Status:** Accepted 2026-07-30 (user, issue #530); applies [ADR
0001](0001-personal-first-product-compatible.md) and [ADR
0004](0004-per-domain-truth-owners.md)

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
consumer where the product is. It tells them the mode, and in `fused` mode the
product root is the instance root. In `split` mode the product root is not
derivable from this file and must come from `--product-root` or
`AGENT_FABRIC_PRODUCT_ROOT`. Any consumer resolving a product root reads the
mode and then the environment, in that order.

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
the rule that merge already implements and no other: allow-lists intersect so
an adapter absent from the product list cannot be added, `activeAdapters`
outside the resulting allow-list raises `CONFIG_WIDENING_FORBIDDEN`, workspace
roots must be contained within a product root, and the concurrency limit takes
the minimum of the two. An absent instance file resolves to exactly the
product-only configuration.

No second merge engine is introduced, for any file class. Where a class needs
layered behaviour it uses this merge; where it does not, it is whole-file owned
by one side. There is deliberately no three-way merge anywhere in the
installer: a three-way merge needs a common ancestor per file, which is
precisely the state the seeded-once rule refuses to keep.

## Consequences

The installer now writes the desired-state file when it is absent and leaves it
alone forever after, seeds `AGENTS.md`, `config/model-preferences.json` and
`config/model-routing.json` into the instance root only when they are absent,
and continues to write its receipt outside the instance repository.
`.gitignore` names both receipt files so that a receipt landing inside an
instance tree cannot be committed by accident.

Split-layout startup binds the product root for the global config layer,
`adapter-compatibility.yaml`, the compatibility schema and the `${AGENTS_HOME}`
token, and the instance root for the local layer. In today's fused layout the
two roots are the same directory, so every one of those bindings resolves where
it resolved before and the change is a no-op until an instance root actually
differs.

An explicit `AGENT_FABRIC_INSTANCE_ROOT` outranks a generic agents-home input
when resolving the instance root. After the split `AGENTS_HOME` names the
product, because it is the token the shipped adapter commands expand against, so
without that ordering a split layout would collapse back to fused whenever
`AGENTS_HOME` was set, which is always.

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
