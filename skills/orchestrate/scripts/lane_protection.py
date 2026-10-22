#!/usr/bin/env python3
"""Refuse unreleased protected instruction changes before a lane is consumed or landed.

The gate walks every commit in `tip ^base` through the owner's common Git directory. A commit
may change a protected path only to the version the base already has, or be reachable from a
tip the chair released after review, or match that tip's complete protected tree while its
reviewed content has not yet landed. Fabric landing passes the landing lease's remote SHA as
the base; claims and manual consumption default to the primary checkout's HEAD. Writers can
write only their own branch ref and the object store: never the primary branch, its upstream
or the release records here.
"""

from __future__ import annotations

import argparse
from datetime import UTC, datetime
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
from provider_exec import _instruction_path
from trusted_git import owner_location

SHA = re.compile(r"[0-9a-f]{40}(?:[0-9a-f]{24})?")


class ProtectionError(ValueError):
    def __init__(self, message, code="protected_instructions_unverifiable"):
        self.code = code
        super().__init__(f"{code}: {message}")


def git(git_dir, *args):
    # No replace refs, commit graphs, multi-pack index, lazy fetch or caller Git environment.
    environment = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    result = subprocess.run(
        ["git", "--git-dir", str(git_dir), "-c", "core.commitGraph=false", "-c", "core.multiPackIndex=false",
         "-c", "core.fsmonitor=false", "-c", "protocol.allow=never", *args],
        capture_output=True, text=True, timeout=30,
        env={**environment, "GIT_NO_LAZY_FETCH": "1", "GIT_NO_REPLACE_OBJECTS": "1"})
    if result.returncode:
        raise ProtectionError(result.stderr.strip() or f"git {args[0]} failed")
    return result.stdout


def commit(common, rev):
    if rev.startswith("-"):
        raise ProtectionError(f"invalid revision {rev!r}")
    return git(common, "rev-parse", "--verify", rev + "^{commit}").strip()


def head(location):
    return git(location.private, "rev-parse", "--verify", "HEAD^{commit}").strip()


def protected(common, rev):
    entries = {}
    for line in git(common, "ls-tree", "-r", "-z", "--full-tree", rev).split("\0"):
        meta, _, name = line.partition("\t")
        if name and _instruction_path(name):
            entries[name] = meta
    return entries


def releases(common):
    return Path(common) / "provenant-instruction-checks/releases"


def released(common):
    tips = []
    for path in sorted(releases(common).glob("*.json")):
        tip = json.loads(path.read_text()).get("tip")
        if not isinstance(tip, str) or not SHA.fullmatch(tip):
            raise ProtectionError(f"invalid release record {path.name}")
        tips.append(tip)
    return tips


def check(common, tip, base="HEAD"):
    """Return tip's SHA when no commit in tip ^base makes an unreleased protected change.

    A merge may also take a protected path's version from a parent in the range that is not held,
    but only where a released commit in the range introduced that version, as a landing train's
    --no-ff merge of a released tip does. Older versions, such as an outside parent's or a stale
    branch's, never count, so a merge cannot revert a protected path.

    A commit may also match one released tip's complete protected tree. That tip must not be
    an ancestor of the base, and its protected tree must differ from the base's, so an old
    release cannot vouch for restoring content the base has already moved past.
    """
    tip, base = commit(common, tip), commit(common, base)
    tips = released(common)
    covered = set(git(common, "rev-list", "--ignore-missing", *tips, "^" + base).split()) if tips else set()
    trees, held = {}, []
    reviewed_trees = None
    # Each judged, unheld range commit maps to the protected paths whose version a released commit introduced.
    vouched = {}

    def entries(rev):
        if rev not in trees:
            trees[rev] = protected(common, rev)
        return trees[rev]

    # Parents first, so each merge parent in the range is judged before the merge.
    for line in git(common, "rev-list", "--topo-order", "--reverse", "--parents", tip, "^" + base).splitlines():
        sha, *parents = line.split()
        inside = [parent for parent in parents if parent in vouched]
        # -m compares a merge with each parent, so its resolution counts as well.
        names = git(common, "diff-tree", "--root", "-m", "-r", "--no-commit-id", "--name-only", "-z", sha)
        changed = {name for name in names.split("\0") if name and _instruction_path(name)}
        inherited = set().union(*(vouched[parent] for parent in inside)) - changed
        if sha in covered or not changed:
            vouched[sha] = inherited | (changed if sha in covered else set())
            continue
        if reviewed_trees is None:
            # Membership in covered excludes missing tips and tips already in the base's history.
            reviewed_trees = [entries(tip) for tip in tips
                              if tip in covered and entries(tip) != entries(base)]
        if entries(sha) in reviewed_trees:
            vouched[sha] = inherited | changed
            continue
        # The base's version is trusted, as a merge or rebase from the base takes it.
        taken = {name for name in changed if len(parents) > 1 and any(
            name in vouched[parent] and entries(parent).get(name) == entries(sha).get(name) for parent in inside)}
        if all(name in taken or entries(sha).get(name) == entries(base).get(name) for name in changed):
            vouched[sha] = inherited | taken
        else:
            held.append(sha)
    if held:
        raise ProtectionError(
            f"unreleased commits change .agents/ relative to {base}: {', '.join(held)}; review them, "
            "then release the reviewed tip or restore .agents/", "protected_instructions_changed")
    return tip


def release(cwd, tip, *, actor, reason, common=None):
    """Record the chair's review of tip and everything reachable from it.

    This is an audit record, not authorisation: anyone who can write the owner's common
    directory could write it. No writer boundary grants that directory, which keeps it chair-side.
    """
    if os.environ.get("PROVENANT_ATTEMPT_MARKER"):
        raise ProtectionError("release runs only outside a writer attempt")
    if not SHA.fullmatch(tip):
        raise ProtectionError("release requires the full reviewed commit SHA")
    location = owner_location(cwd, common=common)
    if commit(location.common, tip) != tip or head(location) != tip:
        raise ProtectionError(f"{location.worktree} HEAD is not the reviewed tip {tip}; review it again")
    record = {"tip": tip, "actor": actor, "reason": reason, "worktree": str(location.worktree),
              "released_at": datetime.now(UTC).isoformat()}
    root = releases(location.common)
    root.mkdir(parents=True, exist_ok=True)
    fd = os.open(root / f"{uuid.uuid4().hex}.json", os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o644)
    with os.fdopen(fd, "w") as handle:
        handle.write(json.dumps(record, sort_keys=True) + "\n")
    return tip


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--cwd", type=Path, default=Path.cwd(), help="checkout whose HEAD is the default tip")
    parser.add_argument("--common-dir", type=Path, help="owner common Git directory (default: from the checkout)")
    parser.add_argument("--tip", help="commit to check or release (default: the checkout's HEAD)")
    parser.add_argument("--base", default="HEAD", help="owner-held trusted commit (default: the primary HEAD)")
    parser.add_argument("--release", action="store_true", help="chair only: record review of --tip")
    parser.add_argument("--actor")
    parser.add_argument("--reason")
    args = parser.parse_args()
    try:
        if args.release:
            if not (args.tip and args.actor and args.reason):
                raise ProtectionError("release requires --tip <reviewed SHA>, --actor and --reason")
            print(release(args.cwd, args.tip, actor=args.actor, reason=args.reason, common=args.common_dir))
        else:
            # An explicit tip and common directory need no checkout, as for Fabric landing.
            location = None if args.tip and args.common_dir else owner_location(args.cwd, common=args.common_dir)
            print(check(args.common_dir or location.common, args.tip or head(location), args.base))
        return 0
    except (OSError, ValueError, subprocess.SubprocessError) as exc:
        print(exc if isinstance(exc, ProtectionError) else ProtectionError(str(exc)), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
