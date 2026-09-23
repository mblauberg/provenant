#!/usr/bin/env python3
"""Repository-wide run layout, also used by linked worktrees and non-Git callers."""

import argparse
from datetime import UTC, datetime
import os
from pathlib import Path
import re
import secrets
import subprocess


def run_root(cwd=None):
    cwd = Path(cwd or Path.cwd()).resolve()
    env = {
        key: value for key, value in os.environ.items() if not key.startswith("GIT_")
    }
    try:
        result = subprocess.run(
            [
                "git",
                "-C",
                str(cwd),
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
            ],
            capture_output=True,
            text=True,
            timeout=5,
            env=env,
        )
        if result.returncode == 0 and result.stdout.strip():
            return Path(result.stdout.strip()).resolve().parent
    except (OSError, subprocess.SubprocessError):
        pass
    return cwd


def run_workspace(path, cwd=None):
    """Locate the workspace of an explicitly supplied run, without ambient discovery."""
    cwd = Path(cwd or Path.cwd()).resolve()
    path = Path(path).resolve()
    root = run_root(cwd)
    if path.is_relative_to(root):
        return root
    # A non-Git owner may execute in a nested task cwd. Its supplied run path
    # identifies the workspace; unrelated ancestor .agent-run directories do not.
    if root == cwd and not (root / ".git").exists():
        for ancestor in path.parents:
            if ancestor.name == ".agent-run" and cwd.is_relative_to(ancestor.parent):
                return ancestor.parent
    return root


def contains_run(path, cwd=None):
    return Path(path).resolve().is_relative_to(run_workspace(path, cwd))


def new_run_dir(cwd=None, kind="dispatch", slug="task", owner=False):
    if kind not in {"dispatch", "batch", "orch", "delivery", "mission", "review", "wf"}:
        raise ValueError("invalid run kind")
    slug = re.sub(r"[^a-zA-Z0-9._-]+", "-", slug).strip("-.")[:64] or "task"
    root = run_root(cwd) / ".agent-run"
    name = (
        datetime.now(UTC).strftime("%Y%m%d-%H%M")
        + f"-{kind}-{slug}-"
        + secrets.token_hex(3)
    )
    result = root / "runs" / name
    result.mkdir(parents=True, exist_ok=False)
    if owner:
        (result / "_owner").mkdir()
    return result


def reap_orphans(cwd=None, at=None):
    """Close stale abandoned execution receipts; never signal an unverified reused PID."""
    import json
    import signal
    import time

    try:
        from .fabric_records import render_digest, append_index
    except ImportError:
        from fabric_records import render_digest, append_index
    sys_path = Path(__file__).resolve().parents[2]
    import sys

    if str(sys_path) not in sys.path:
        sys.path.insert(0, str(sys_path))
    from _shared.custody import read_bound_bytes, atomic_write_contained

    root = run_root(cwd) / ".agent-run"
    at = time.time() if at is None else at
    closed = []
    paths = list((root / "runs").glob("*/RUN_RECEIPT.json")) + list(
        root.glob("mcp-*/RUN_RECEIPT.json")
    )
    for path in paths:
        directory = path.parent
        try:
            receipt = json.loads(
                read_bound_bytes(directory, "RUN_RECEIPT.json", label="run receipt")
            )
            if (
                receipt.get("status") != "active"
                or at - path.stat().st_mtime <= 48 * 3600
            ):
                continue
            owner = receipt.get("owner_pid")
            owner_file = directory / "dispatch-owner.json"
            if owner_file.is_file():
                owner_record = json.loads(
                    read_bound_bytes(
                        directory, "dispatch-owner.json", label="owner receipt"
                    )
                )
                owner = owner_record.get("owner_pid", owner_record.get("pid", owner))
            if isinstance(owner, int) and owner > 0:
                try:
                    os.kill(owner, 0)
                except ProcessLookupError:
                    pass
                except PermissionError:
                    continue
                else:
                    continue
            # A provider may outlive its owner. Check the retained start identity first.
            provider_file = directory / "dispatch-provider.json"
            if provider_file.is_file():
                provider = json.loads(
                    read_bound_bytes(
                        directory, "dispatch-provider.json", label="provider receipt"
                    )
                )
                pgid = provider.get("provider_pgid")
                started = provider.get("provider_started_at")
                if isinstance(pgid, int) and pgid > 0:
                    try:
                        os.kill(pgid, 0)
                    except ProcessLookupError:
                        pass
                    except PermissionError:
                        continue
                    else:
                        observed = subprocess.run(
                            ["/bin/ps", "-o", "lstart=", "-p", str(pgid)],
                            capture_output=True,
                            text=True,
                            timeout=2,
                            env={**os.environ, "LC_ALL": "C", "LANG": "C"},
                        )
                        if not started or observed.stdout.strip() != started:
                            inherited = subprocess.run(
                                ["/bin/ps", "-o", "lstart=", "-p", str(pgid)],
                                capture_output=True, text=True, timeout=2,
                            )
                            if not started or inherited.stdout.strip() != started:
                                continue
                        os.killpg(pgid, signal.SIGTERM)
                        time.sleep(0.1)
                        try:
                            os.killpg(pgid, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
            live_provider = False
            for attempt in (directory / "tasks").glob("*/attempt-*/attempt.json"):
                row = json.loads(
                    read_bound_bytes(
                        directory,
                        attempt.relative_to(directory),
                        label="attempt receipt",
                    )
                )
                pgid = row.get("pgid")
                if (
                    row.get("state") != "terminal"
                    and isinstance(pgid, int)
                    and pgid > 0
                ):
                    try:
                        os.kill(pgid, 0)
                    except ProcessLookupError:
                        continue
                    except PermissionError:
                        pass
                    live_provider = True
            if live_provider:
                continue
            ended = datetime.fromtimestamp(at, UTC).isoformat().replace("+00:00", "Z")
            rows = []
            for attempt in (directory / "tasks").glob("*/attempt-*/attempt.json"):
                row = json.loads(
                    read_bound_bytes(
                        directory,
                        attempt.relative_to(directory),
                        label="attempt receipt",
                    )
                )
                if row.get("state") != "terminal":
                    row.update(
                        state="terminal",
                        status="interrupted",
                        ended_at=ended,
                        retryable=True,
                    )
                    row["evidence"].update(
                        signature="owner_died",
                        excerpt="Owner is no longer live; stale execution closed",
                    )
                    row["digest"] = render_digest(row)
                    atomic_write_contained(
                        directory,
                        attempt.relative_to(directory),
                        (json.dumps(row, indent=2) + "\n").encode(),
                        label="attempt receipt",
                    )
                    append_index(row, directory, root=root)
                rows.append(row)
            receipt.update(
                status="interrupted",
                closed_at=ended,
                attempts=rows,
                terminal_reason="owner_died",
            )
            atomic_write_contained(
                directory,
                "RUN_RECEIPT.json",
                (json.dumps(receipt, indent=2) + "\n").encode(),
                label="run receipt",
            )
            closed.append(str(directory))
        except (OSError, ValueError, TypeError, subprocess.SubprocessError):
            continue
    return closed


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--reap-orphans", action="store_true")
    p.add_argument("--kind", default="orch")
    p.add_argument("--slug", default="run")
    p.add_argument("--owner-logs", action="store_true")
    p.add_argument("--root", action="store_true")
    args = p.parse_args()
    print(
        reap_orphans()
        if args.reap_orphans
        else run_root()
        if args.root
        else new_run_dir(kind=args.kind, slug=args.slug, owner=args.owner_logs)
    )
