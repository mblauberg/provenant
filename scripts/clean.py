#!/usr/bin/env python3
"""Classify Provenant run artifacts and remove only an approved, unchanged plan."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
from typing import Any


RUN_NAME = re.compile(r"^\d{8}-\d{4}-(dispatch|batch|orch|delivery|mission|review|wf)-[a-z0-9-]+-[a-z0-9]{6}$")
LEGACY_ORCH = re.compile(r"^\d{8}(?:[-T]\d{4,6})?(?:[-_].*)?$")
MCP_NAME = re.compile(r"^mcp-[A-Za-z0-9_-]+$")
LEGACY_SIBLING = re.compile(r"^(mcp-[A-Za-z0-9_-]+)-(?:owner\.(?:stdout\.jsonl|stderr\.log)|task-manifest\.json)$")
DEFAULT_INCLUDE = frozenset({"runs", "scratch", "worktrees"})
KINDS = frozenset({"runs", "scratch", "worktrees", "sessions"})
DAY = 86400


class CleanError(ValueError):
    """The cleanup request cannot be proved safe."""


def _command(*argv: str, cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(argv, cwd=cwd, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          stdin=subprocess.DEVNULL, timeout=10, check=False)


def primary_root(path: Path) -> Path:
    requested = path.expanduser().resolve()
    if not requested.is_dir():
        raise CleanError(f"repository path is not a directory: {requested}")
    result = _command("git", "rev-parse", "--path-format=absolute", "--git-common-dir", cwd=requested)
    if result.returncode == 0:
        common = Path(result.stdout.strip()).resolve()
        if common.name == ".git":
            return common.parent
    return requested


def _json(path: Path) -> dict[str, Any] | None:
    try:
        if path.is_symlink() or not path.is_file():
            return None
        value = json.loads(path.read_text())
        return value if isinstance(value, dict) else None
    except (OSError, ValueError):
        return None


def _size(path: Path) -> int:
    if path.is_symlink():
        return 0
    if path.is_file():
        return path.stat().st_size
    size = 0
    for base, directories, files in os.walk(path, followlinks=False):
        directories[:] = [name for name in directories if not (Path(base) / name).is_symlink()]
        for name in files:
            child = Path(base) / name
            if not child.is_symlink():
                try:
                    size += child.stat().st_size
                except OSError:
                    pass
    return size


def _age(path: Path, now: datetime) -> float:
    return max(0.0, (now.timestamp() - path.lstat().st_mtime) / DAY)


def _pid_alive(pid: Any, started_at: Any) -> bool:
    """Match run-registry's PID/start check; uncertain identity stays protected."""
    if type(pid) is not int or pid <= 1:
        return False
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    if not isinstance(started_at, str) or not started_at:
        return True
    try:
        observed = _command("/bin/ps", "-o", "lstart=", "-p", str(pid)).stdout.strip()
    except (OSError, subprocess.TimeoutExpired):
        return True
    return not observed or observed == started_at


def _lock_held(run_dir: Path) -> bool:
    """Same nonblocking MANIFEST lock observation as run_dir_finalize."""
    path = run_dir / "MANIFEST.md"
    if not path.is_file() or path.is_symlink():
        return False
    try:
        with path.open("r+") as stream:
            try:
                fcntl.flock(stream.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                return True
            finally:
                fcntl.flock(stream.fileno(), fcntl.LOCK_UN)
    except OSError:
        return True
    return False


def _live(run_dir: Path) -> bool:
    if _lock_held(run_dir):
        return True
    for name in ("dispatch-owner.json", "dispatch-provider.json", "_owner/dispatch-owner.json",
                 "_owner/dispatch-provider.json"):
        record = _json(run_dir / name)
        if record and any(_pid_alive(record.get(prefix + "_pid"), record.get(prefix + "_started_at"))
                          for prefix in ("owner", "host", "provider")):
            return True
    for attempt in run_dir.glob("tasks/*/attempt-*/attempt.json"):
        record = _json(attempt)
        if record and _pid_alive(record.get("pgid"), None):
            return True
    return False


def _accepted(delivery: dict[str, Any]) -> bool:
    gate = delivery.get("human_gates", {}).get("acceptance") if isinstance(delivery.get("human_gates"), dict) else None
    return isinstance(gate, dict) and (gate.get("status") in {"approved", "accepted"} or gate.get("approved") is True)


def _stopped(mission: Path) -> bool:
    try:
        goal = (mission / "GOAL.md").read_text(errors="replace")
        state = (mission / "STATE.md").read_text(errors="replace")
        handoff = (mission / "HANDOFF.md").read_text(errors="replace")
    except OSError:
        return False
    return bool(
        re.search(r"(?im)^\s*STATUS:\s*STOP\s*$", goal)
        and re.search(r"(?im)^.*Run status:.*STOPPED", state)
        and re.search(r"(?im)^.*Run status:.*FINISHED.*STOP", handoff)
    )


def _refs(root: Path, pr_bodies: list[str] | None) -> tuple[str | None, bool]:
    state = root / ".agent-run" / "sessions"
    bodies = "\n".join(pr_bodies or [])
    for path in state.glob("*/STATE.md") if state.is_dir() else []:
        if path.is_file() and not path.is_symlink():
            bodies += "\n" + path.read_text(errors="replace")
    return bodies, pr_bodies is None


def _open_prs(root: Path) -> tuple[list[str] | None, set[str]]:
    if not (root / ".git").exists():
        return [], set()
    remotes = _command("git", "remote", cwd=root)
    if remotes.returncode == 0 and not remotes.stdout.strip():
        return [], set()
    try:
        result = _command("gh", "pr", "list", "--state", "open", "--limit", "1000",
                          "--json", "headRefName,body", cwd=root)
        rows = json.loads(result.stdout) if result.returncode == 0 else None
    except (OSError, subprocess.TimeoutExpired, ValueError):
        rows = None
    if not isinstance(rows, list):
        return None, set()
    return [str(item.get("body") or "") for item in rows if isinstance(item, dict)], {
        str(item.get("headRefName")) for item in rows if isinstance(item, dict) and item.get("headRefName")
    }


def _run_kind(path: Path, canonical: bool) -> str | None:
    if canonical:
        match = RUN_NAME.fullmatch(path.name)
        return match.group(1) if match else None
    if MCP_NAME.fullmatch(path.name) and (path / "RUN_RECEIPT.json").is_file():
        return "dispatch"
    if (path / "RUN.json").is_file():
        return "delivery"
    if (path / "GOAL.md").is_file():
        return "mission"
    if LEGACY_ORCH.fullmatch(path.name) and (path / "RUN_RECEIPT.json").is_file():
        return "orch"
    return None


def _run_verdict(path: Path, kind: str, age: float, refs: str | None, pr_unknown: bool,
                 older_than: float | None, indexed_ids: set[str]) -> str:
    if (path / "KEEP").exists():
        return "keep:KEEP"
    if _live(path):
        return "keep:live"
    if pr_unknown:
        return "keep:pr-unknown"
    receipt = _json(path / "RUN_RECEIPT.json")
    referenced_ids = {path.name, str(path), *indexed_ids}
    if receipt and isinstance(receipt.get("run_id"), str):
        referenced_ids.add(receipt["run_id"])
    owner = _json(path / "dispatch-owner.json") or _json(path / "_owner/dispatch-owner.json")
    if owner and isinstance(owner.get("run_id"), str):
        referenced_ids.add(owner["run_id"])
    if refs is not None and any(identity in refs for identity in referenced_ids):
        return "keep:referenced"
    if kind == "delivery":
        receipt = _json(path / "RUN.json")
        if receipt is None:
            return "triage:invalid-delivery"
        if not _accepted(receipt):
            return "keep:unaccepted-delivery"
    if kind == "mission" and not _stopped(path):
        return "keep:mission-active"
    if kind not in {"delivery", "mission"} and receipt is None:
        return "triage:missing-receipt"
    status = str((receipt or {}).get("status") or (receipt or {}).get("state") or "").lower()
    if kind == "delivery":
        retention = 30
    elif kind == "mission":
        retention = 7
    elif status in {"failed", "partial", "stalled", "timed_out", "interrupted", "rejected", "tool_missing"}:
        retention = 14
    elif status in {"active", "running", "queued", ""}:
        if age > 2:
            return "abandon"
        return "keep:active"
    elif status in {"succeeded", "ok", "cancelled", "canceled", "complete", "completed"}:
        retention = 7
    else:
        return "triage:unknown-status"
    if older_than is not None:
        retention = max(retention, older_than)
    return "delete" if age >= retention else f"keep:retention-{retention:g}d"


def _row(root: Path, path: Path, kind: str, verdict: str, now: datetime) -> dict[str, Any]:
    stat = path.lstat()
    return {"path": path.relative_to(root).as_posix(), "kind": kind, "age_days": round(_age(path, now), 1),
            "size_bytes": _size(path), "verdict": verdict, "_identity": [stat.st_dev, stat.st_ino, stat.st_mtime_ns, stat.st_size]}


def _registered_worktrees(root: Path) -> set[Path]:
    result = _command("git", "worktree", "list", "--porcelain", "-z", cwd=root)
    if result.returncode != 0:
        return set()
    return {Path(line[len("worktree "):]).resolve() for line in result.stdout.split("\0")
            if line.startswith("worktree ")}


def _indexed_run_ids(index: Path) -> dict[str, set[str]]:
    ids: dict[str, set[str]] = {}
    if index.is_symlink():
        return ids
    try:
        with index.open() as stream:
            for line in stream:
                try:
                    row = json.loads(line)
                except ValueError:
                    continue
                if isinstance(row, dict) and isinstance(row.get("dir"), str) and isinstance(row.get("run_id"), str):
                    ids.setdefault(Path(row["dir"]).name, set()).add(row["run_id"])
    except OSError:
        pass
    return ids


def _worktree_verdict(root: Path, path: Path, pr_unknown: bool, open_heads: set[str],
                      registered: set[Path]) -> str:
    if path.resolve() not in registered:
        return "triage:unregistered"
    for artifact_root in (path / ".agent-run", path / ".work" / "wf"):
        if artifact_root.is_symlink() or (artifact_root.is_dir() and any(artifact_root.iterdir())):
            return "keep:worktree-runs"
    if pr_unknown:
        return "keep:pr-unknown"
    branch_result = _command("git", "symbolic-ref", "--quiet", "--short", "HEAD", cwd=path)
    if branch_result.returncode != 0:
        return "triage:detached"
    branch = branch_result.stdout.strip()
    if path.name != branch.replace("/", "-"):
        return "triage:name-mismatch"
    if branch in open_heads:
        return "keep:open-pr"
    dirty = _command("git", "status", "--porcelain=v1", "--untracked-files=all", cwd=path)
    if dirty.returncode != 0:
        return "triage:git-error"
    if dirty.stdout:
        return "triage:merged-dirty" if _merged(root, branch) else "keep:dirty"
    if _merged(root, branch):
        base = "refs/heads/main" if _command("git", "show-ref", "--verify", "--quiet", "refs/heads/main", cwd=root).returncode == 0 else "HEAD"
        if _command("git", "rev-parse", branch, cwd=root).stdout.strip() == _command("git", "rev-parse", base, cwd=root).stdout.strip():
            return "keep:branch-at-base"
        # lsof checks cwd, including descendants; a missing or failing probe
        # cannot prove the worktree is free of live processes.
        try:
            liveness = _command("lsof", "-n", "-a", "-d", "cwd", "+D", str(path), cwd=root)
        except (OSError, subprocess.TimeoutExpired):
            return "keep:liveness-unknown"
        if len(liveness.stdout.splitlines()) > 1:
            return "keep:live"
        if liveness.returncode != 1 or liveness.stderr.strip():
            return "keep:liveness-unknown"
        return "delete"
    return "keep:unmerged"


def _merged(root: Path, branch: str) -> bool:
    base = "refs/heads/main" if _command("git", "show-ref", "--verify", "--quiet", "refs/heads/main", cwd=root).returncode == 0 else "HEAD"
    if _command("git", "merge-base", "--is-ancestor", branch, base, cwd=root).returncode == 0:
        return True
    # Squash proof needs both a merged PR and an empty scoped tree diff.
    try:
        result = _command("gh", "pr", "list", "--state", "merged", "--head", branch,
                          "--limit", "1000", "--json", "state,number", cwd=root)
        merged_prs = [row["number"] for row in json.loads(result.stdout)
                      if row.get("state") == "MERGED" and isinstance(row.get("number"), int)] if result.returncode == 0 else []
    except (OSError, subprocess.TimeoutExpired, ValueError, AttributeError):
        return False
    for number in merged_prs:
        try:
            result = _command("gh", "pr", "view", str(number), "--json", "files", cwd=root)
            files = json.loads(result.stdout).get("files", []) if result.returncode == 0 else []
            changed = [file["path"] for file in files if isinstance(file, dict) and isinstance(file.get("path"), str)]
        except (OSError, subprocess.TimeoutExpired, ValueError, AttributeError, KeyError):
            continue
        if changed and len(changed) == len(files) and _command("git", "diff", "--quiet", base, branch, "--", *changed, cwd=root).returncode == 0:
            return True
    return False


def plan(repo: Path, *, include: frozenset[str] = DEFAULT_INCLUDE, older_than: float | None = None,
         pr_bodies: list[str] | None = None, now: datetime | None = None) -> dict[str, Any]:
    root = primary_root(repo)
    now = now or datetime.now(timezone.utc)
    if not include <= KINDS:
        raise CleanError("unknown include class")
    if pr_bodies is None:
        pr_bodies, open_heads = _open_prs(root)
    else:
        open_heads = set()
    refs, pr_unknown = _refs(root, pr_bodies)
    rows: list[dict[str, Any]] = []
    agent = root / ".agent-run"
    indexed_ids = _indexed_run_ids(agent / "runs" / "index.jsonl")
    run_verdicts: dict[str, str] = {}
    for parent, canonical in ((agent / "runs", True), (agent, False)):
        if not parent.is_dir() or parent.is_symlink():
            continue
        for path in sorted(parent.iterdir()):
            if parent == agent and path.name in {"runs", "scratch", "sessions", "README.md"}:
                continue
            if parent.name == "runs" and path.name == "index.jsonl":
                rows.append(_row(root, path, "index", "keep:provenance-index", now))
                continue
            if path.is_symlink():
                rows.append(_row(root, path, "unknown", "triage:symlink", now))
                continue
            if path.is_file():
                if parent == agent:
                    sibling = LEGACY_SIBLING.fullmatch(path.name)
                    if sibling:
                        # Linked to the run below after every run has been classified.
                        rows.append(_row(root, path, "owner-log", "triage:orphan-sibling", now))
                    else:
                        rows.append(_row(root, path, "unknown", "triage:hand-named", now))
                else:
                    rows.append(_row(root, path, "unknown", "triage:unexpected-file", now))
                continue
            if not path.is_dir():
                continue
            kind = _run_kind(path, canonical)
            if kind is None:
                rows.append(_row(root, path, "unknown", "triage:hand-named", now))
                continue
            verdict = _run_verdict(path, kind, _age(path, now), refs, pr_unknown, older_than, indexed_ids.get(path.name, set()))
            if "runs" not in include and verdict in {"delete", "abandon"}:
                verdict = "keep:excluded"
            rows.append(_row(root, path, kind, verdict, now))
            if not canonical:
                run_verdicts[path.name] = verdict
            owner = path / "_owner"
            if owner.is_dir() and not owner.is_symlink() and verdict != "delete":
                for child in sorted(owner.iterdir()):
                    is_log = child.is_file() and not child.is_symlink() and child.suffix in {".log", ".jsonl"}
                    log_verdict = "delete" if is_log and verdict.startswith("keep:retention") and _age(child, now) >= max(7.0, older_than or 0) and "runs" in include else "keep:run-record"
                    rows.append(_row(root, child, "owner-log" if is_log else "owner-record", log_verdict, now))
    for row in rows:
        if row["kind"] == "owner-log" and row["verdict"] == "triage:orphan-sibling":
            match = LEGACY_SIBLING.fullmatch(Path(row["path"]).name)
            if match and match.group(1) in run_verdicts:
                verdict = run_verdicts[match.group(1)]
                is_log = Path(row["path"]).suffix in {".log", ".jsonl"}
                row["verdict"] = "delete" if verdict == "delete" or (is_log and verdict.startswith("keep:retention") and row["age_days"] >= max(7.0, older_than or 0) and "runs" in include) else "keep:run-record"
    scratch = agent / "scratch"
    if scratch.is_dir() and not scratch.is_symlink():
        for path in sorted(scratch.iterdir()):
            retention = max(1.0, older_than or 0)
            verdict = "triage:symlink" if path.is_symlink() else "delete" if "scratch" in include and _age(path, now) >= retention else f"keep:retention-{retention:g}d"
            rows.append(_row(root, path, "scratch", verdict, now))
    sessions = agent / "sessions"
    if sessions.is_dir() and not sessions.is_symlink():
        for path in sorted(sessions.iterdir()):
            if path.is_symlink():
                verdict = "triage:symlink"
            elif _age(path, now) >= max(14.0, older_than or 0):
                verdict = "delete" if "sessions" in include else "triage:session-idle"
            else:
                verdict = "keep:session"
            rows.append(_row(root, path, "session", verdict, now))
    worktrees = root / ".worktrees"
    if worktrees.is_dir() and not worktrees.is_symlink():
        registered = _registered_worktrees(root)
        for path in sorted(worktrees.iterdir()):
            if path.is_symlink() or not path.is_dir():
                verdict = "triage:unregistered"
            else:
                verdict = _worktree_verdict(root, path, pr_unknown, open_heads, registered)
            if "worktrees" not in include and verdict == "delete":
                verdict = "keep:excluded"
            elif verdict == "delete" and older_than is not None and _age(path, now) < older_than:
                verdict = f"keep:retention-{older_than:g}d"
            rows.append(_row(root, path, "worktree", verdict, now))
    legacy_workflows = root / ".work" / "wf"
    if legacy_workflows.is_dir() and not legacy_workflows.is_symlink():
        for workflow in sorted(legacy_workflows.iterdir()):
            if workflow.is_dir() and not workflow.is_symlink():
                for capsule in sorted(workflow.iterdir()):
                    rows.append(_row(root, capsule, "legacy-wf", "triage:legacy-workflow", now))
    rows.sort(key=lambda item: item["path"])
    digest_data = {"root": str(root), "include": sorted(include), "older_than": older_than,
                   "rows": [{key: value for key, value in row.items() if key not in {"age_days", "size_bytes"}}
                            for row in rows if row["verdict"] in {"delete", "abandon"}]}
    digest = "sha256:" + hashlib.sha256(json.dumps(digest_data, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    total = sum(row["size_bytes"] for row in rows if row["verdict"] == "delete")
    all_size = sum(row["size_bytes"] for row in rows if row["kind"] not in {"owner-log", "index"})
    result: dict[str, Any] = {"root": str(root), "rows": rows, "reclaimable_bytes": total, "plan_sha256": digest}
    if all_size > 500 * 1024 * 1024:
        result["warning"] = {"message": "run artifacts exceed 500 MB", "largest": sorted(
            [{"path": row["path"], "size_bytes": row["size_bytes"]} for row in rows if row["kind"] not in {"owner-log", "index"}],
            key=lambda row: row["size_bytes"], reverse=True)[:5]}
    return result


def apply(repo: Path, approved_plan: str, *, include: frozenset[str] = DEFAULT_INCLUDE,
          older_than: float | None = None, pr_bodies: list[str] | None = None,
          human_authorised: bool = False) -> list[str]:
    current = plan(repo, include=include, older_than=older_than, pr_bodies=pr_bodies)
    if approved_plan != current["plan_sha256"]:
        raise CleanError("approved plan digest does not match the current cleanup plan")
    if not human_authorised and any(row["kind"] == "worktree" and row["verdict"] == "delete" for row in current["rows"]):
        raise CleanError("worktree removal requires --human-authorised; or exclude worktrees from this plan")
    root = Path(current["root"])
    removed: list[str] = []
    for row in current["rows"]:
        path = root / row["path"]
        verdict = row["verdict"]
        if verdict not in {"delete", "abandon"}:
            continue
        if path.is_symlink() or not path.exists() or not path.resolve().is_relative_to(root):
            raise CleanError(f"path changed during cleanup: {path}")
        if verdict == "abandon":
            receipt_path = path / "RUN_RECEIPT.json"
            receipt = _json(receipt_path)
            if receipt is None or _live(path):
                raise CleanError(f"abandoned run changed during cleanup: {path}")
            receipt.update({"status": "interrupted", "terminal_reason": "abandoned: owner gone",
                            "closed_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")})
            temporary = receipt_path.with_suffix(".json.tmp")
            temporary.write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n")
            temporary.replace(receipt_path)
            continue
        if row["kind"] == "worktree":
            command = [sys.executable, str(Path(__file__).with_name("worktree.py")), "remove", path.name,
                       "--repo", str(root), "--human-authorised"]
            result = subprocess.run(command, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
            if result.returncode != 0:
                raise CleanError(f"worktree removal failed for {path}: {result.stderr.strip()}")
        elif path.is_dir():
            shutil.rmtree(path)
        else:
            path.unlink()
        removed.append(row["path"])
    return removed


def _duration(value: str) -> float:
    match = re.fullmatch(r"(\d+)([dh])", value)
    if not match:
        raise argparse.ArgumentTypeError("duration must be like 7d or 24h")
    return int(match.group(1)) * (1 if match.group(2) == "d" else 1 / 24)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    parser.add_argument("--include", default="runs,scratch,worktrees")
    parser.add_argument("--older-than", type=_duration)
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--plan")
    parser.add_argument("--human-authorised", action="store_true", help="attest authority to remove merged worktrees")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    include = frozenset(part.strip() for part in args.include.split(",") if part.strip())
    if args.apply != bool(args.plan):
        parser.error("--apply and --plan sha256:<digest> are required together")
    try:
        if args.apply:
            removed = apply(args.repo, args.plan, include=include, older_than=args.older_than,
                            human_authorised=args.human_authorised)
            report = {"removed": removed, "count": len(removed)}
        else:
            report = plan(args.repo, include=include, older_than=args.older_than)
    except (CleanError, OSError, subprocess.TimeoutExpired) as exc:
        print(f"provenant clean: {exc}", file=sys.stderr)
        return 2
    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    elif args.apply:
        print(f"removed {report['count']} paths")
        for path in report["removed"]:
            print(path)
    else:
        print("path | kind | age | size | verdict")
        for row in report["rows"]:
            print(f"{row['path']} | {row['kind']} | {row['age_days']}d | {row['size_bytes']} B | {row['verdict']}")
        print(f"reclaimable: {report['reclaimable_bytes']} B")
        print(f"plan_sha256: {report['plan_sha256']}")
        if "warning" in report:
            print(report["warning"]["message"], file=sys.stderr)
            for row in report["warning"]["largest"]:
                print(f"  {row['path']}: {row['size_bytes']} B", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
