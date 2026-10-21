#!/usr/bin/env python3
"""Bind writer instruction checks to commits before reuse or integration."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile
import uuid
from datetime import UTC, datetime

sys.path.insert(0, str(Path(__file__).resolve().parent))
from output_custody import install, open_parent
from provider_exec import _git_output, _instruction_entries, _instruction_path, instruction_snapshot
from trusted_git import GitLocation, owner_location


class ProtectionError(ValueError):
    def __init__(self, message, code="protected_instructions_unverifiable"):
        self.code = code
        super().__init__(f"{code}: {message}")


def directory(cwd):
    return owner_location(cwd).common / "provenant-instruction-checks"


def identity(cwd):
    ref = _git_output(cwd, "rev-parse", "--symbolic-full-name", "HEAD").strip()
    if ref != "HEAD" and not ref.startswith("refs/heads/"):
        raise ProtectionError("writer needs a lane branch or detached HEAD")
    return ref


def state_path(cwd, ref=None):
    # HEAD, gitdir and branch names are writable by a child. Only the owner's worktree
    # registration selects a lane; the record's lane_id additionally binds the attempt scope.
    location = owner_location(cwd)
    return directory(location) / (hashlib.sha256(os.fsencode(location.worktree)).hexdigest() + ".json")


def read(path):
    try:
        parent, leaf = open_parent(str(path))
    except FileNotFoundError:
        return None
    try:
        try:
            fd = os.open(leaf, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent)
        except FileNotFoundError:
            return None
        with os.fdopen(fd, "rb") as handle:
            if not stat.S_ISREG(os.fstat(handle.fileno()).st_mode):
                raise ProtectionError("invalid lane protection evidence")
            value = json.loads(handle.read(65537))
            if (not isinstance(value, dict)
                    or any(not isinstance(value.get(key), str) for key in
                           ("ref", "start_tip", "attempt_dir", "worktree", "common", "private",
                            "run_id", "task_id", "nonce", "lane_id"))
                    or type(value.get("attempt")) is not int
                    or not isinstance(value.get("quarantined"), bool)
                    or value.get("verified_tip") is not None and not isinstance(value["verified_tip"], str)):
                raise ProtectionError("invalid lane protection evidence")
            return value
    finally:
        os.close(parent)


def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile() as staged:
        staged.write((json.dumps(value, sort_keys=True) + "\n").encode())
        staged.flush()
        install(staged.name, str(path))


@contextmanager
def locked(location):
    root = directory(location)
    root.mkdir(parents=True, exist_ok=True)
    parent, leaf = open_parent(str(root / "owner.lock"))
    try:
        fd = os.open(leaf, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600, dir_fd=parent)
        with os.fdopen(fd, "r+") as handle:
            fcntl.flock(handle, fcntl.LOCK_EX)
            yield
    finally:
        os.close(parent)


def bound(location, state, evidence=None):
    if any(state[key] != value for key, value in location.record().items()):
        raise ProtectionError("lane differs from owner-recorded Git locations")
    if evidence is not None and any(evidence.get(key) != state.get(key) for key in
                                    ("run_id", "task_id", "attempt", "nonce", "lane_id", "verified_tip")):
        raise ProtectionError("instruction check does not match dispatch attempt evidence")


def check_dispatch(state):
    # Direct provider calls have no dispatch receipt. Fabric attempts must have matching
    # top-level scope and final nonce; an earlier attempt's successful result cannot stand in.
    if not state["task_id"]:
        return
    dispatch = json.loads((Path(state["attempt_dir"]) / "attempt.json").read_text())
    if any(dispatch.get(key) != state.get(key) for key in ("run_id", "task_id", "attempt")):
        raise ProtectionError("instruction check scope differs from dispatch evidence")
    evidence = dispatch.get("instruction_protection")
    expected = state
    if state.get("release"):
        expected = {**state, "nonce": state["release"]["held_nonce"],
                    "verified_tip": state["release"]["previous_verified_tip"]}
    if not isinstance(evidence, dict) or any(evidence.get(key) != expected.get(key) for key in
                                          ("run_id", "task_id", "attempt", "nonce", "lane_id", "verified_tip")):
        raise ProtectionError("instruction check differs from dispatch attempt evidence")


def current_tip(location, state=None, ref=None):
    location.validate()
    actual = identity(location)
    if state is not None:
        bound(location, state)
        if actual != state["ref"]:
            raise ProtectionError("writer changed its recorded lane branch")
    if ref is not None and ref != actual:
        raise ProtectionError("requested ref differs from owner-recorded lane identity")
    return _git_output(location, "rev-parse", "--verify", actual + "^{commit}").strip()


def descendants(location, anchor, tip):
    if not re.fullmatch(r"[0-9a-f]{40}(?:[0-9a-f]{24})?", anchor):
        raise ProtectionError("invalid verified lane tip")
    return _git_output(location, "rev-list", "--ancestry-path", tip, "^" + anchor).splitlines()


def check_history(location, anchor, tip):
    if not re.fullmatch(r"[0-9a-f]{40}(?:[0-9a-f]{24})?", anchor):
        raise ProtectionError("invalid verified lane tip")
    if anchor != tip and tip not in descendants(location, anchor, tip):
        raise ProtectionError("lane tip rewrote the verified history")
    for commit in _git_output(location, "rev-list", tip, "^" + anchor).splitlines():
        names = _git_output(location, "diff-tree", "--root", "--no-commit-id", "--name-only", "-r", "-m",
                            "-z", commit).split("\0")
        if any(_instruction_path(name) for name in names):
            raise ProtectionError(f"lane changed .agents/ after verified tip {anchor}",
                                  "protected_instructions_changed")


def retain(path, state, exc):
    error = exc if isinstance(exc, ProtectionError) else ProtectionError(str(exc))
    if state is not None:
        write(path, {**state, "quarantined": True, "error": error.code, "reason": str(error)})
        return ProtectionError(f"{error}; branch retained in quarantine for chair review", error.code)
    return error


def verify(cwd, ref=None, *, tip=None, common=None, evidence=None, initial=False):
    """Return a pinned commit, resolved solely through owner-selected Git locations."""
    location = owner_location(cwd, common=common)
    path = state_path(location)
    with locked(location):
        state = read(path)
        try:
            current = current_tip(location, state, ref)
            if tip is not None and current != tip:
                raise ProtectionError("lane branch moved while being consumed")
            if state is None:
                if location.private != location.common and not initial:
                    raise ProtectionError("registered writer has no instruction protection record")
                return current
            bound(location, state, evidence)
            check_dispatch(state)
            if state["quarantined"] or not state.get("verified_tip"):
                raise ProtectionError("lane has no usable completed instruction check",
                                      state.get("error", "protected_instructions_unverifiable"))
            check_history(location, state["verified_tip"], current)
            return current
        except (OSError, ValueError, subprocess.SubprocessError) as exc:
            raise retain(path, state, exc) from exc


def begin(cwd, attempt_dir, *, common=None, run_id="", task_id="", attempt=1, ref=None):
    location = owner_location(cwd, common=common)
    # Serialize both admission and publication. A concurrent begin cannot silently adopt an
    # unfinished attempt, and finish compares the nonce under this same owner-only lock.
    with locked(location):
        path = state_path(location)
        previous = read(path)
        try:
            tip = current_tip(location, previous, ref)
            if previous:
                check_dispatch(previous)
            if previous:
                if previous["quarantined"] or not previous.get("verified_tip"):
                    raise ProtectionError("lane has no usable completed instruction check",
                                          previous.get("error", "protected_instructions_unverifiable"))
                check_history(location, previous["verified_tip"], tip)
            anchor = previous["verified_tip"] if previous else tip
            snapshot = instruction_snapshot(location)
            baseline = snapshot["tree"] if anchor == tip else instruction_tree(location, anchor)
            if (snapshot["disk"] != baseline or snapshot["index"] != baseline
                    or _instruction_entries(location, ("ls-files", "--stage", "-z"), index=True)[1]):
                raise ProtectionError("disk or index .agents/ differs from verified tree",
                                      "protected_instructions_changed")
            scope = {"run_id": run_id, "task_id": task_id, "attempt": attempt}
            lane_id = hashlib.sha256(json.dumps({"worktree": str(location.worktree), **scope},
                                               sort_keys=True).encode()).hexdigest()
            state = {**location.record(), **scope, "lane_id": lane_id, "nonce": uuid.uuid4().hex,
                     "ref": identity(location), "start_tip": tip, "verified_tip": None,
                     "attempt_dir": str(Path(attempt_dir).resolve()), "quarantined": True}
            write(path, state)
            return state
        except (OSError, ValueError, subprocess.SubprocessError) as exc:
            raise retain(path, previous, exc) from exc


def instruction_tree(location, tip):
    return _instruction_entries(location, ("ls-tree", "-r", "-z", "--full-tree", tip))[0]


def finish(cwd, state, verified_tip, *, quarantined):
    location = owner_location(cwd, common=state["common"])
    path = state_path(location)
    with locked(location):
        saved = read(path)
        if saved != state or saved.get("completed"):
            raise ProtectionError("instruction check attempt nonce was superseded or already finished")
        if current_tip(location, saved) != verified_tip:
            raise retain(path, saved, ProtectionError("lane tip moved during the final instruction check"))
        evidence = {**saved, "verified_tip": verified_tip, "quarantined": quarantined, "completed": True}
        write(path, evidence)
        return evidence


def verify_landing(cwd, tip, *, common=None):
    """Inspect contributed history in the landing tip, even after refs reset or disappear."""
    location = owner_location(cwd, common=common)
    verify(location, tip=tip)
    with locked(location):
        for path in directory(location).glob("*.json"):
            state = read(path)
            if state is None:
                raise ProtectionError("missing lane protection evidence")
            anchor = state.get("verified_tip") or state["start_tip"]
            commits = descendants(location, anchor, tip)
            if anchor != tip and tip not in commits:
                continue
            # Only descendants of the recorded anchor contributed. Do not consult the mutable
            # ref (or even the worktree, which may have been pruned) to identify consumed commits.
            direct = state["worktree"] == str(location.worktree)
            if (not commits and not direct and anchor == state["start_tip"]
                    and not (state["quarantined"] and state.get("completed"))):
                continue
            try:
                check_dispatch(state)
                if state["quarantined"] or not state.get("verified_tip"):
                    raise ProtectionError("landing includes a lane without a usable instruction check",
                                          state.get("error", "protected_instructions_unverifiable"))
                for commit in commits:
                    names = _git_output(location, "diff-tree", "--root", "--no-commit-id", "--name-only",
                                        "-r", "-m", "-z", commit).split("\0")
                    if any(_instruction_path(name) for name in names):
                        raise ProtectionError("landing includes protected lane history",
                                              "protected_instructions_changed")
            except (OSError, ValueError, subprocess.SubprocessError) as exc:
                raise retain(path, state, exc) from exc


def release(cwd, *, common=None, reason, actor, evidence):
    """Chair-only acknowledgement. Audit the exact held nonce before trusting a new baseline."""
    location = owner_location(cwd, common=common)
    with locked(location):
        path = state_path(location)
        state = read(path)
        if state is None or not state["quarantined"]:
            raise ProtectionError("lane has no quarantine to release")
        bound(location, state, evidence)
        tip = current_tip(location, state)
        snapshot = instruction_snapshot(location)
        if snapshot["disk"] != snapshot["tree"] or snapshot["index"] != snapshot["tree"]:
            raise ProtectionError("chair must resolve protected disk and index residue before release")
        audit = {"actor": actor, "reason": reason, "released_at": datetime.now(UTC).isoformat(),
                 "held_nonce": state["nonce"], "previous_verified_tip": state["verified_tip"], "verified_tip": tip}
        # Immutable audit entry is installed first. Providers have no write grant here.
        write(directory(location) / "releases" / (uuid.uuid4().hex + ".json"), {**state, "release": audit})
        write(path, {**state, "verified_tip": tip, "quarantined": False, "completed": True,
                     "nonce": uuid.uuid4().hex, "release": audit})
        return tip


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cwd", type=Path, default=Path.cwd())
    parser.add_argument("--common-dir", type=Path)
    parser.add_argument("--ref")
    parser.add_argument("--landing-tip")
    parser.add_argument("--evidence", type=Path, help="dispatch-attempt JSON; match its instruction check and scope")
    parser.add_argument("--release-quarantine", action="store_true")
    parser.add_argument("--actor")
    parser.add_argument("--reason")
    args = parser.parse_args()
    try:
        evidence = None
        if args.evidence:
            dispatch = json.loads(args.evidence.read_text())
            evidence = dispatch["instruction_protection"]
            if any(dispatch.get(key) != evidence.get(key) for key in ("run_id", "task_id", "attempt")):
                raise ProtectionError("instruction check scope differs from dispatch evidence")
        if args.release_quarantine:
            if not evidence or not args.actor or not args.reason or os.environ.get("PROVENANT_ATTEMPT_MARKER"):
                raise ProtectionError("chair release requires dispatch evidence, actor and reason outside a writer")
            print(release(args.cwd, common=args.common_dir, reason=args.reason, actor=args.actor, evidence=evidence))
        elif args.landing_tip:
            verify_landing(args.cwd, args.landing_tip, common=args.common_dir)
        else:
            print(verify(args.cwd, args.ref, common=args.common_dir, evidence=evidence))
        return 0
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as exc:
        print(str(exc if isinstance(exc, ProtectionError) else ProtectionError(str(exc))), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
