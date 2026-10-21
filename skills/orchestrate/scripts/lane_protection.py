#!/usr/bin/env python3
"""Bind writer instruction checks to commits before reuse or integration."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parent))
from output_custody import install, open_parent
from provider_exec import _git_output, _instruction_path, _is_ancestor


class ProtectionError(ValueError):
    def __init__(self, message, code="protected_instructions_unverifiable"):
        self.code = code
        super().__init__(f"{code}: {message}; branch retained in quarantine for chair review")


def directory(cwd):
    # The owner writes here, outside every provider's Git write grants. Keep these
    # records when a worktree disappears; a branch is not a worktree directory.
    common = _git_output(cwd, "rev-parse", "--path-format=absolute", "--git-common-dir").strip()
    return Path(common) / "provenant-instruction-checks"


def identity(cwd):
    ref = _git_output(cwd, "rev-parse", "--symbolic-full-name", "HEAD").strip()
    if ref != "HEAD" and not ref.startswith("refs/heads/"):
        raise ProtectionError("writer needs a lane branch or detached HEAD")
    return ref


def state_path(cwd, ref):
    key = ref if ref != "HEAD" else _git_output(cwd, "rev-parse", "--absolute-git-dir").strip()
    return directory(cwd) / (hashlib.sha256(key.encode()).hexdigest() + ".json")


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
                    or any(not isinstance(value.get(key), str)
                           for key in ("ref", "start_tip", "attempt_dir", "worktree"))
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


def verify(cwd, ref=None, *, tip=None):
    """Return a pinned commit; reject protected late history and retain a sticky quarantine."""
    ref = identity(cwd) if ref is None or ref == "HEAD" else ref
    if ref != "HEAD" and not ref.startswith("refs/heads/"):
        raise ProtectionError("pass a full refs/heads/ lane ref")
    path = state_path(cwd, ref)
    try:
        if ref != "HEAD":
            _git_output(cwd, "check-ref-format", ref)
        state = read(path)
        current = _git_output(cwd, "rev-parse", "--verify", ref + "^{commit}").strip()
        if tip is not None and current != tip:
            raise ProtectionError("lane branch moved while being consumed")
        if state is None:
            # Changing the branch name must not erase this worktree's last check.
            for previous in path.parent.glob("*.json"):
                prior = read(previous)
                if (prior and prior.get("worktree") == str(Path(cwd).resolve())
                        and ref == identity(cwd)):
                    raise ProtectionError("writer worktree changed its recorded lane branch")
            return current  # A branch never dispatched here has no attempt check to bind.
        if state.get("ref") != ref or state.get("quarantined") or not state.get("verified_tip"):
            raise ProtectionError("lane has no usable completed instruction check",
                                  state.get("error", "protected_instructions_unverifiable"))
        checked = state["verified_tip"]
        if not isinstance(checked, str) or not re.fullmatch(r"[0-9a-f]{40}(?:[0-9a-f]{24})?", checked):
            raise ProtectionError("invalid verified lane tip")
        if current != checked:
            if not _is_ancestor(cwd, checked, current):
                raise ProtectionError("lane tip rewrote the verified history")
            # Inspect every commit, not only the net tree diff: edit-then-revert and merge
            # commits must not launder instructions into the next attempt's baseline.
            commits = _git_output(cwd, "rev-list", checked + ".." + current).splitlines()
            for commit in commits:
                names = _git_output(cwd, "diff-tree", "--root", "--no-commit-id", "--name-only", "-r", "-m",
                                    "-z", commit).split("\0")
                if any(_instruction_path(name) for name in names):
                    raise ProtectionError(f"lane changed .agents/ after verified tip {checked}",
                                          "protected_instructions_changed")
        return current
    except (OSError, ValueError, subprocess.SubprocessError) as exc:
        error = exc if isinstance(exc, ProtectionError) else ProtectionError(str(exc))
        # Preserve the offending ref and its commits. Never restore or delete late state.
        if "state" in locals() and state is not None:
            write(path, {**state, "quarantined": True, "error": error.code, "reason": str(error)})
        raise error from exc


def begin(cwd, attempt_dir):
    ref = identity(cwd)
    tip = verify(cwd, ref)
    state = {"ref": ref, "start_tip": tip, "verified_tip": None,
             "attempt_dir": str(attempt_dir), "worktree": str(Path(cwd).resolve()), "quarantined": True}
    write(state_path(cwd, ref), state)
    return state


def finish(cwd, state, verified_tip, *, quarantined):
    if identity(cwd) != state["ref"]:
        raise ProtectionError("writer changed branch identity")
    if _git_output(cwd, "rev-parse", "--verify", state["ref"] + "^{commit}").strip() != verified_tip:
        raise ProtectionError("lane tip moved during the final instruction check")
    evidence = {**state, "verified_tip": verified_tip, "quarantined": quarantined}
    write(state_path(cwd, state["ref"]), evidence)
    return evidence


def verify_landing(cwd, tip):
    """Check recorded lane refs whose checked or current tips enter a landing commit."""
    verify(cwd, tip=tip)
    root = directory(cwd)
    if not root.exists():
        return
    refs = dict(line.split()[::-1] for line in _git_output(cwd, "show-ref", "--heads").splitlines())
    for path in root.glob("*.json"):
        state = read(path)
        if state is None or not isinstance(state.get("ref"), str):
            raise ProtectionError("invalid lane protection evidence")
        ref = state["ref"]
        lane_cwd = state.get("worktree") if ref == "HEAD" else cwd
        current = (_git_output(lane_cwd, "rev-parse", "--verify", "HEAD^{commit}").strip()
                   if ref == "HEAD" and lane_cwd and Path(lane_cwd).is_dir() else refs.get(ref))
        checked = state.get("verified_tip")
        if current is None and not state.get("quarantined"):
            continue  # A normally merged/pruned lane no longer supplies a branch to consume.
        # A common starting commit alone does not mean this landing consumed a
        # sibling lane. Only its contribution, or the directly pushed lane, does.
        anchors = {current, checked} - {None, state.get("start_tip")}
        if current:
            try:
                common = _git_output(cwd, "merge-base", "--all", current, tip).splitlines()
            except ValueError:
                common = []  # Unrelated history cannot contribute to this landing.
            anchors.update(anchor for anchor in common if anchor != state["start_tip"]
                           and _is_ancestor(cwd, state["start_tip"], anchor))
        if any(_is_ancestor(cwd, anchor, tip) for anchor in anchors):
            if current is None:
                raise ProtectionError(f"recorded lane ref is missing: {ref}")
            verify(lane_cwd, ref)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cwd", type=Path, default=Path.cwd())
    parser.add_argument("--ref")
    parser.add_argument("--landing-tip")
    args = parser.parse_args()
    try:
        if args.landing_tip:
            verify_landing(args.cwd, args.landing_tip)
        else:
            print(verify(args.cwd, args.ref))
        return 0
    except (OSError, ValueError, subprocess.SubprocessError) as exc:
        print(str(exc if isinstance(exc, ProtectionError) else ProtectionError(str(exc))), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
