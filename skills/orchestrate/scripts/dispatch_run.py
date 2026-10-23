#!/usr/bin/env python3
"""Own a durable run of provider attempts, including fallback and session resume.

Routing stays with cf_dispatch.py --plan-only; provider_exec runs the provider
in this process. Legacy receipts remain additive while Fabric v1 rows provide
live progress, typed terminal states, provenance, and controls.
"""

from __future__ import annotations

import argparse
import errno
from contextlib import contextmanager, redirect_stdout
import fcntl
import hashlib
import importlib.util
import io
import json
import math
import os
import random
import re
import shutil
import signal
import stat
import subprocess
import sys
import time
import tempfile
import uuid
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any, NamedTuple, NoReturn

# The shared library sits one level above this skill. Resolving it from the
# skill rather than from a repository root two levels further up keeps the
# installed per-entry layout working even when the instance directory that
# holds the linked skills is not itself named "skills" (#755).
SKILLS_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(SKILLS_ROOT))
sys.path.insert(0, str(Path(__file__).resolve().parent))
import process_info
CF_DISPATCH = Path(__file__).with_name("cf_dispatch.py")
ATTEMPT_ID_RE = re.compile(r"^attempt-(?P<number>\d{3}|[1-9]\d{3,})$")
ESCALATION_REASONS = ("gate_failed", "review_blocked", "capability", "quota_or_limit")
BATCH_ID_RE = re.compile(r"^batch-(?:\d{3}|[1-9]\d{3,})$")
DEFAULT_TIMEOUT_SECONDS = 3600.0
# Match the existing abandoned-receipt grace period for pre-identity attempts.
LEGACY_ATTEMPT_STALE_SECONDS = 48 * 3600
MAX_GIT_EVIDENCE_HEADER_BYTES = 64 * 1024

from _shared.bounded_process import BudgetArgumentParser, stop_process_group
from _shared import harness_profile
from layout import run_workspace, run_root, contains_run
import run_records
import writer_lease
import dispatch_preflight
import git_evidence_packet
import provider_exec
import provider_output
import memory_admission
import secret_scan
import exec_routing
import context_usage
import attempt_storage
import run_lineage
import stall_watchdog
from fabric_records import pacing_policy, identity_resolver, render_digest, write_cooldown, write_route_health, append_index, TERMINAL_STATUSES
from _shared.custody import (
    OwnedFileError, OwnedLinkError, atomic_write_contained, contained_regular_path,
    ensure_contained_directory, create_contained_directory, open_contained_regular, read_bound_bytes,
    read_contained_regular, unlink_contained_regular,
)
from attempt_evidence import AttemptEvidenceError as SharedAttemptEvidenceError, canonical_success_status, is_success_status, successful_adapter_error, validate_successful_attempt

def now() -> str:
    return datetime.now(UTC).isoformat(timespec="milliseconds").replace("+00:00", "Z")

def active_receipt_error(receipt: Any) -> str | None:
    if not isinstance(receipt, dict):
        return "RUN_RECEIPT.json root must be an object"
    if receipt.get("status") != "active" or receipt.get("closed_at") is not None:
        return "dispatch requires an active orchestration run"
    if receipt.get("schema_version") != 1:
        return "RUN_RECEIPT.json schema_version must be 1"
    try:
        created_at = receipt["created_at"]
        parsed = datetime.fromisoformat(created_at.replace("Z", "+00:00"))
        if (
            not isinstance(created_at, str)
            or not created_at.endswith("Z")
            or parsed.tzinfo is None
            or parsed.utcoffset() != UTC.utcoffset(parsed)
        ):
            raise ValueError
    except (KeyError, AttributeError, TypeError, ValueError):
        return "RUN_RECEIPT.json created_at must be a UTC timestamp"
    if not isinstance(receipt.get("owner"), str) or not receipt["owner"]:
        return "RUN_RECEIPT.json owner is required"
    if not isinstance(receipt.get("retention_policy"), str) or not receipt["retention_policy"]:
        return "RUN_RECEIPT.json retention_policy is required"
    for field in (
        "owned_panes", "closed_panes", "handed_off_panes", "unclassified_paths", "pruned_paths"
    ):
        if not isinstance(receipt.get(field), list):
            return f"RUN_RECEIPT.json {field} must be a list"
    pair = receipt.get("pair")
    if not isinstance(pair, dict) or pair.get("mode") not in {"solo", "paired-primary"}:
        return "RUN_RECEIPT.json pair must declare solo or paired-primary mode"
    if not isinstance(pair.get("status"), str) or not pair["status"]:
        return "RUN_RECEIPT.json pair status is required"
    return None

def success_receipt_error(
    adapter: dict[str, Any], args: argparse.Namespace, result_path: Path, result_digest: str
) -> str | None:
    return successful_adapter_error(adapter, args.tool, args.intent, result_path, result_digest)

def fail(run_dir: Path | None, status: str, message: str, error: str | None = None) -> int:
    record = {"schema_version": 1, "status": status, "message": message,
              "error": error or status}
    print(json.dumps(record, sort_keys=True))
    return 2

def timeout_value(value: str) -> float:
    try:
        timeout = float(value)
    except ValueError as exc:
        raise argparse.ArgumentTypeError("timeout must be a finite positive number") from exc
    if not math.isfinite(timeout) or timeout <= 0:
        raise argparse.ArgumentTypeError("timeout must be a finite positive number")
    return timeout

def acquire_run_custody(run_dir: Path):
    """Acquire the shared manifest lock without creating a new run artifact."""
    return run_lineage.acquire_owner_custody(run_dir)

@contextmanager
def attempt_custody(run_dir: Path, path: Path, *, blocking: bool = True):
    """Serialise short publications, including children without run custody."""
    fd, _relative, _target = open_contained_regular(
        run_dir, path.with_suffix(".lock").relative_to(run_dir),
        os.O_RDWR | os.O_CREAT, label="attempt custody"
    )
    with os.fdopen(fd, "a+") as stream:
        fcntl.flock(stream.fileno(), fcntl.LOCK_EX | (0 if blocking else fcntl.LOCK_NB))
        yield

def reap_run_evidence(run_dir: Path):
    """Read current closure evidence, refusing a live or unobservable owner."""
    receipt = json.loads(read_bound_bytes(run_dir, "RUN_RECEIPT.json", label="run receipt"))
    if not isinstance(receipt, dict) or receipt.get("status") != "active":
        return None
    # Older writers may not hold custody. A live or unobservable owner
    # still owns its attempts, even if its provider has already exited.
    owners = [receipt]
    for name in ("dispatch-owner.json", "dispatch-status.json"):
        try:
            (run_dir / name).lstat()
        except FileNotFoundError:
            continue
        owners.append(json.loads(read_bound_bytes(run_dir, name, label="run owner")))
    terminal_status = "lost"
    run_token = None
    for owner in owners:
        if not isinstance(owner, dict):
            return None
        if owner.get("run_token"):
            run_token = owner["run_token"]
        if owner.get("finished_at") and owner.get("status") in {"interrupted", "cancelled"}:
            terminal_status = owner["status"]
        pid = owner.get("owner_pid", owner.get("pid"))
        if not isinstance(pid, int) or pid <= 0:
            continue
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            continue
        started = owner.get("owner_started_at")
        observed = process_info.start_time(pid) if started else None
        if not started or observed is None or observed == started:
            return None
    return terminal_status, run_token

def reap_stop_status(run_dir: Path, path: Path, row: dict[str, Any]):
    evidence = reap_run_evidence(run_dir)
    if evidence is None:
        return None
    status, run_token = evidence
    # A recorded closure takes precedence over leftover cancellation markers.
    if status != "lost":
        return status
    try:
        (run_dir / "termination-request.json").lstat()
        request = json.loads(read_bound_bytes(run_dir, "termination-request.json", label="termination request"))
    except FileNotFoundError:
        request = {}
    # Controller intent is run-scoped; batch children have distinct provider PIDs.
    if provider_exec.termination_request_matches(
        request, run_token=run_token, provider_pid=None,
        started_at=row.get("started_at"), signals={"SIGTERM", "SIGKILL"},
    ):
        return "cancelled" if request["source"] == "cancel_requested" else "interrupted"
    cancel_dir = run_dir / "dispatch/tasks" / path.parent.parent.name / path.parent.name
    if run_records.cancellation_marker_present(run_dir, cancel_dir):
        return "cancelled"
    batch_id = row.get("batch_id")
    if isinstance(batch_id, str) and BATCH_ID_RE.fullmatch(batch_id):
        batch_dir = run_dir / "dispatch/batches" / batch_id
        if run_records.cancellation_marker_present(run_dir, batch_dir):
            try:
                (batch_dir / "summary.json").lstat()
                summary = json.loads(read_bound_bytes(run_dir, batch_dir.relative_to(run_dir) / "summary.json", label="batch summary"))
            except FileNotFoundError:
                summary = {}
            if not isinstance(summary, dict) or summary.get("status") not in {"completed", "failed", "cancelled"}:
                return "cancelled"
    return "lost"

def process_evidence(run_dir: Path, row: dict[str, Any]) -> dict[str, Any]:
    evidence = {"pgid_probe": "unknown", "last_event_at": None,
                "events_bytes": None, "stderr_bytes": None}
    pgid = row.get("pgid")
    if isinstance(pgid, int) and pgid > 0:
        try:
            os.killpg(pgid, 0)
        except ProcessLookupError:
            evidence["pgid_probe"] = "gone"
        except OSError:
            pass
        else:
            started = row.get("pgid_started_at")
            observed = process_info.start_time(pgid) if started else None
            if started and observed:
                evidence["pgid_probe"] = "reused" if observed != started else "live"
    for name in ("events", "stderr"):
        try:
            value = (row.get("paths") or {}).get(name)
            if not value:
                continue
            fd, _, _ = open_contained_regular(run_dir, value, os.O_RDONLY, label=name)
            try:
                metadata = os.fstat(fd)
            finally:
                os.close(fd)
            evidence[name + "_bytes"] = metadata.st_size
            if name == "events":
                evidence["last_event_at"] = datetime.fromtimestamp(metadata.st_mtime, UTC).isoformat(timespec="milliseconds").replace("+00:00", "Z")
        except (OSError, OwnedFileError):
            pass
    return evidence

def reap_provider_identity(run_dir: Path, path: Path, row: dict[str, Any]) -> dict[str, Any]:
    if type(row.get("pgid")) is int and row["pgid"] > 0:
        return {"pgid": row["pgid"], "pgid_started_at": row.get("pgid_started_at")}
    attempt_dir = run_dir / "dispatch/tasks" / path.parent.parent.name / path.parent.name
    for directory in dict.fromkeys((path.parent, attempt_dir)):
        try:
            pgid = int(read_bound_bytes(run_dir, (directory / "pgid").relative_to(run_dir), label="provider pgid"))
            if pgid > 0:
                return {"pgid": pgid, "pgid_started_at": row.get("pgid_started_at")}
        except (OSError, ValueError, OwnedFileError):
            pass
    try:
        provider = json.loads(read_bound_bytes(run_dir, "dispatch-provider.json", label="provider identity"))
        evidence = reap_run_evidence(run_dir)
        token = evidence[1] if evidence else None
        pgid = provider.get("provider_pgid")
        if "task_id" in provider and "attempt" in provider:
            same_attempt = provider["task_id"] == row.get("task_id") and provider["attempt"] == row.get("attempt")
        else:  # An older owner did not record the attempt; trust it only for a single-task run.
            same_attempt = len({p.parent.parent.name for tree in ("tasks", "dispatch/tasks")
                                for p in (run_dir / tree).glob("*/attempt-*")}) <= 1
        if (provider.get("run_token") == (token or run_records.run_identity(run_dir)) and same_attempt
                and type(pgid) is int and pgid > 0):
            return {"pgid": pgid, "pgid_started_at": provider.get("provider_started_at")}
    except (OSError, ValueError, TypeError, AttributeError, OwnedFileError):
        pass
    return {}

def reap_disk_full(run_dir: Path, row: dict[str, Any]) -> tuple[str | None, str | None]:
    """Return (ENOSPC stderr evidence, low-free-space hint); only the first proves disk exhaustion."""
    try:
        value = (row.get("paths") or {}).get("stderr")
        if value:
            fd, _, _ = open_contained_regular(run_dir, value, os.O_RDONLY, label="stderr")
            try:
                os.lseek(fd, max(0, os.fstat(fd).st_size - 8192), os.SEEK_SET)
                tail = os.read(fd, 8192).decode("utf-8", errors="replace")
            finally:
                os.close(fd)
            if re.search(r"ENOSPC|no space left on device|disk (?:is )?full", tail, re.I):
                return tail[-400:], None
    except (OSError, OwnedFileError):
        pass
    try:
        workspace = Path(row.get("workspace_root") or row.get("cwd") or run_dir)
        if shutil.disk_usage(run_dir).free < disk_floor_bytes(workspace, row.get("mode", "read_only")):
            return None, "Free space below disk_floor_bytes at reap time"
    except (OSError, ValueError, KeyError):
        pass
    return None, None

def reap_attempts(run_dir: Path) -> list[str]:
    """Finalise dead attempts under custody, preserving deliberate termination."""
    run_dir = Path(run_dir)
    reaped = []
    git_deadline = time.monotonic() + 1  # Shared by every dead writer in this pass.
    try:
        custody = acquire_run_custody(run_dir)
    except (OSError, OwnedFileError):
        return reaped
    with custody:
        try:
            if reap_run_evidence(run_dir) is None:
                return reaped
        except (OSError, ValueError, TypeError, OwnedFileError):
            return reaped
        for tree in ("tasks", "dispatch/tasks"):
            for path in (run_dir / tree).glob("*/attempt-*/attempt.json"):
                try:
                    row = json.loads(read_bound_bytes(run_dir, path.relative_to(run_dir), label="attempt receipt"))
                    if not isinstance(row, dict) or row.get("schema") != "fabric.attempt.v1" or row.get("state") != "running":
                        continue
                    identity = reap_provider_identity(run_dir, path, row)
                    events = (row.get("paths") or {}).get("events")
                    try:
                        evidence_dir = contained_regular_path(run_dir, events, "events")[1].parent if events else path.parent
                    except (OSError, ValueError):
                        evidence_dir = path.parent
                    pgid = identity.get("pgid")
                    if not isinstance(pgid, int) or pgid <= 0:
                        continue
                    started = identity.get("pgid_started_at")
                    try:
                        os.killpg(pgid, 0)
                    except ProcessLookupError:
                        if not started:
                            if not isinstance(row.get("last_progress_at"), str):
                                continue
                            progress = datetime.fromisoformat(row["last_progress_at"].replace("Z", "+00:00"))
                            if (datetime.now(UTC) - progress).total_seconds() <= LEGACY_ATTEMPT_STALE_SECONDS:
                                continue
                        reason = "pgid_gone"
                    else:
                        observed = process_info.start_time(pgid) if started else None
                        if not started or observed is None or observed == started:
                            resolution, _ = harness_profile.lenient(cwd=Path(row.get("cwd") or run_workspace(run_dir)))
                            seconds = (row.get("profile") or {}).get("knobs", {}).get("lanes.silence_seconds",
                                resolution.knobs["lanes.silence_seconds"] if resolution else 300)
                            stall_watchdog.observe_if_silent(evidence_dir, row.get("worktree") or row.get("cwd") or run_workspace(run_dir),
                                pgid, True, row.get("started_at"), seconds)
                            continue
                        reason = "pgid_reused"
                    # A wedged publisher must not hold run custody through us.
                    with attempt_custody(run_dir, path, blocking=False):
                        current = json.loads(read_bound_bytes(run_dir, path.relative_to(run_dir), label="attempt receipt"))
                        # A child may have published after its provider exited.
                        if current != row or current.get("state") != "running":
                            continue
                        status = reap_stop_status(run_dir, path, row)
                        if status is None:
                            continue
                        try:
                            stall_watchdog.checkpoint(evidence_dir, row.get("worktree") or row.get("cwd") or run_workspace(run_dir), pgid,
                                include_git=row.get("mode") == "worktree_write", git_deadline=git_deadline)
                        except (OSError, ValueError) as exc:
                            row.setdefault("warnings", []).append("checkpoint unavailable: " + str(exc))
                        else:
                            row.setdefault("paths", {})["checkpoint"] = run_records.relative_path(run_dir, evidence_dir / "wip-checkpoint.json")
                        row.update(identity)
                        disk_error, space_hint = reap_disk_full(run_dir, row)
                        if disk_error and status == "lost":
                            status = "failed"
                            reason = "host_disk_full"
                            row["error"] = "host_disk_full"
                            row["fix"] = "host_disk_full: free disk space or lower the mode's disk_floor_bytes in .agents/fabric-policy.json."
                        row.update(state="terminal", status=status, ended_at=now(), retryable=status == "lost",
                                   error_class=provider_output.error_class(status, reason))
                        row["evidence"] = {**(row.get("evidence") or {}), "signature": reason,
                                           "excerpt": disk_error if reason == "host_disk_full" else
                                           "Attempt process group is gone" if reason == "pgid_gone"
                                           else "Attempt process group start time no longer matches"}
                        if disk_error:
                            row["evidence"]["host_disk_full"] = True
                        elif space_hint:
                            row["evidence"]["low_free_space_hint"] = space_hint
                        row["evidence"].update(process_evidence(run_dir, row))
                        row["digest"] = render_digest(row)
                        run_records.write_owned(run_dir, path, json.dumps(row, indent=2) + "\n")
                        reaped.append(str(path))
                except (OSError, ValueError, TypeError, KeyError, AttributeError, OwnedFileError):
                    continue
        if reaped:
            close_mcp_run(run_dir)
    return reaped

def existing_attempt_number(task_dir: Path) -> int:
    numbers = []
    for candidate in task_dir.glob("attempt-*"):
        match = ATTEMPT_ID_RE.fullmatch(candidate.name)
        if match and candidate.is_dir():
            numbers.append(int(match.group("number")))
    return max(numbers, default=0) + 1


def build_command(
    args: argparse.Namespace, prompt_path: Path, result_path: Path,
    evidence_dir: Path | None = None,
) -> list[str]:
    command = [
        str(CF_DISPATCH),
        "--intent",
        args.intent,
        "--tool",
        args.tool,
        "--prompt-file",
        str(prompt_path),
        "--out",
        str(result_path),
        "--run-dir",
        str(result_path.parent),
        "--role",
        args.role,
    ]
    for flag, value in (
        ("--orchestrator-family", args.orchestrator_family),
        ("--alias", args.alias),
        ("--task-class", args.task_class),
        ("--risk-tier", args.risk_tier),
        ("--model-override-tier", args.model_override_tier),
        ("--reviewer-id", args.reviewer_id),
        ("--model", args.model),
        ("--effort", args.effort),
        ("--account", getattr(args, "account", None) if args.tool == "claude" else None),
        ("--access-mode", args.access_mode),
        ("--worktree", str(args.worktree) if args.worktree else ""),
        ("--timeout-seconds", str(dispatch_preflight.provider_timeout_seconds(args.timeout_seconds))),
        ("--original-prompt-file", getattr(args, "original_prompt_file", None)),
    ):
        if value:
            command.extend((flag, value))
    if evidence_dir is not None:
        command.extend(("--add-dir", str(evidence_dir)))
    capabilities = getattr(args, "capabilities", None)
    if capabilities:
        command.extend(("--capabilities", json.dumps(capabilities)))
    for flag, value in (("--cwd",getattr(args,"provider_cwd",None)),("--sandbox", getattr(args,"sandbox",None)),("--network",getattr(args,"network",None)),("--resume-session",getattr(args,"resume_session",None))):
        if value is not None: command.extend((flag,str(value)))
    for directory in getattr(args,"add_dirs",[]): command.extend(("--add-dir",str(directory)))
    for root in getattr(args,"read_roots",None) or []: command.extend(("--read-root",str(root)))
    if not getattr(args,"preface",True): command.append("--no-preface")
    policy = exec_routing.validate_policy(getattr(args, "fallback", None))
    if policy is not None:
        command.extend(("--fallback", "true" if isinstance(policy, list) else json.dumps(policy) if type(policy) is bool else policy))
    return command

# Statuses whose provider turn ended before a result, so its session may be missing.
INCOMPLETE_TURN_STATUSES = {"timed_out", "cancelled", "stalled", "startup_timeout", "interrupted", "lost"}

def incomplete_provider_turn(attempt: dict) -> bool:
    """Include provider deaths whose failure status does not imply a finished turn."""
    return attempt.get("status") in INCOMPLETE_TURN_STATUSES or (
        attempt.get("status") == "failed"
        and (attempt.get("evidence") or {}).get("signature") in {"provider_signal", "provider_incomplete", "host_disk_full", "provider_lost"}
    )


def _copy_git_evidence(
    workspace: Path, run_dir: Path, source: Path, destination: Path
) -> tuple[dict[str, Any], str]:
    """Validate and copy a packet in bounded memory while hashing its bytes."""
    try:
        source.relative_to(run_dir)
        source_relative = source.relative_to(run_dir)
        destination_relative = destination.relative_to(run_dir)
    except ValueError as exc:
        raise run_records.AttemptEvidenceError("Git evidence must be materialised inside the run directory") from exc
    source_fd = destination_fd = -1
    try:
        source_fd, _source_rel, _source_target = open_contained_regular(
            run_dir, source_relative, os.O_RDONLY, label="Git evidence source"
        )
        destination_fd, _destination_rel, _destination_target = open_contained_regular(
            run_dir,
            destination_relative,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL,
            label="Git evidence packet",
        )
    except (OwnedLinkError, OwnedFileError) as exc:
        if source_fd >= 0:
            os.close(source_fd)
        raise run_records.AttemptEvidenceError(str(exc)) from exc
    hasher = hashlib.sha256()
    scanner = git_evidence_packet.PacketScanner(
        run_records.AttemptEvidenceError, MAX_GIT_EVIDENCE_HEADER_BYTES, validate_on_feed=True
    )

    def write_all(fd: int, value: bytes) -> None:
        offset = 0
        while offset < len(value):
            offset += os.write(fd, value[offset:])

    try:
        while chunk := os.read(source_fd, 1024 * 1024):
            hasher.update(chunk)
            write_all(destination_fd, chunk)
            scanner.feed(chunk)
        metadata = scanner.finish()
        if metadata is None:
            raise run_records.AttemptEvidenceError("Git evidence packet has no JSON header")
        if not scanner.has_status:
            raise run_records.AttemptEvidenceError("Git evidence packet is missing its status section")
        if not scanner.has_diff:
            raise run_records.AttemptEvidenceError("Git evidence packet is missing its diff section")
        os.fsync(destination_fd)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        try:
            unlink_contained_regular(run_dir, destination_relative, label="Git evidence packet")
        except (OSError, ValueError, OwnedFileError):
            pass
        raise run_records.AttemptEvidenceError("Git evidence packet has an invalid JSON header") from exc
    except BaseException:
        try:
            unlink_contained_regular(run_dir, destination_relative, label="Git evidence packet")
        except (OSError, OwnedFileError):
            pass
        raise
    finally:
        if source_fd >= 0:
            os.close(source_fd)
        if destination_fd >= 0:
            os.close(destination_fd)
    return metadata, f"sha256:{hasher.hexdigest()}"

def _record_provider_process(run_dir: Path, process: subprocess.Popen[Any], *, task_id=None, attempt=None) -> None:
    """Record the provider's identity so a detached run stays reachable.

    The MCP front door spawns this owner detached and then lets go of it. If
    that host dies, the only handle left on the provider is what is written
    here: its pid, the group it leads, and a start timestamp that tells it
    apart from a recycled pid. The run token proves the record belongs to this
    run rather than to an earlier one in a reused directory. A failure to write
    it costs later reaping, never the dispatch itself.
    """
    token = os.environ.get("PROVENANT_RUN_TOKEN") or run_records.run_identity(run_dir)
    try:
        started_at = process_info.start_time(process.pid)
        record = {
            "schema_version": 1,
            "run_token": token,
            "provider_pid": process.pid,
            "provider_pgid": os.getpgid(process.pid),
            "provider_started_at": started_at or None,
            "task_id": task_id,
            "attempt": attempt,
        }
        (run_dir / "dispatch-provider.json").write_text(
            json.dumps(record, indent=2) + "\n", encoding="utf-8",
        )
    except (OSError, ValueError, subprocess.SubprocessError):
        return

def _retire_provider_process(run_dir: Path, process: subprocess.Popen[Any] | None, *, task_id=None, attempt=None) -> None:
    """Drop this run's provider record once this owner has reaped the provider.

    A reaped provider's pid can be recycled; a record left behind, especially one
    without a start time, would let an unrelated process pass for it.
    """
    if process is None or process.returncode is None:
        return
    path = run_dir / "dispatch-provider.json"
    try:
        record = json.loads(path.read_text(encoding="utf-8"))
        token = os.environ.get("PROVENANT_RUN_TOKEN") or run_records.run_identity(run_dir)
        if (record.get("run_token") == token and record.get("provider_pid") == process.pid
                and record.get("task_id") == task_id and record.get("attempt") == attempt):
            path.unlink()
    except (OSError, ValueError):
        return

# Fabric grants a read root only for a directory inside a registered project.


def with_pick_reason(line, reason):
    """Name why a route pool picked this model inside the Route line's parentheses."""
    if not reason or not line.endswith(")") or line.endswith("; " + reason + ")"):
        return line
    return line[:-1] + "; " + reason + ")"

def attempt_scope(args, number):
    # Terminal construction reuses the scope captured before provider startup.
    scopes = getattr(args, '_attempt_scopes', None)
    if scopes is None:
        scopes = args._attempt_scopes = {}
    key = (number, args.tool)
    if key not in scopes:
        scopes[key] = identity_resolver().scope(args.tool)
    return scopes[key]


def disk_floor_bytes(workspace_root: Path, mode: str) -> int:
    defaults = {"worktree_write": 500 * 1024**2, "read_only": 500 * 1024**2}
    path = workspace_root / ".agents/fabric-policy.json"
    fix = "Set .agents/fabric-policy.json disk_floor_bytes to non-negative integers."
    try:
        policy = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return defaults[mode]
    except (OSError, UnicodeError, ValueError) as exc:
        raise ValueError(fix) from exc
    floors = policy.get("disk_floor_bytes", {}) if isinstance(policy, dict) else None
    if not isinstance(floors, dict) or any(
        key not in defaults or type(value) is not int or value < 0 for key, value in floors.items()
    ):
        raise ValueError(fix)
    return floors.get(mode, defaults[mode])

def inherit_delivery_run_id(args, previous):
    value = previous.get('delivery_run_id')
    if value is not None:
        if getattr(args, 'delivery_run_id', None) not in (None, value):
            raise ValueError('retry must keep delivery_run_id')
        args.delivery_run_id = run_records.delivery_run_id(value)

class RetryReceiptInvalid(ValueError):
    code = 'retry_of_invalid'

def retry_receipt(run_dir, retry_dir):
    try:
        previous = json.loads(read_bound_bytes(
            run_dir, (retry_dir / 'attempt.json').relative_to(run_dir), label='retry receipt'))
    except (OSError, ValueError, OwnedFileError) as exc:
        raise RetryReceiptInvalid(str(exc)) from exc
    if not isinstance(previous, dict):
        raise RetryReceiptInvalid('retry attempt receipt must be an object')
    return previous

def contract_row(args,run_dir,number,attempt_dir,plan,started_at):
    route=plan.get("route",{})
    model=plan.get("model") or route.get("resolved_model") or args.model or ""
    effort=plan.get("effort") or ""
    family=route.get("model_family") or "unknown"
    label=args.tool+"/"+model+("@"+effort if effort else "")
    identity="resolved" if model else "unknown"
    task_class = getattr(args, "task_class", None) or getattr(args, "_rate_limit_task_class", "")
    provenance={"requested":{"adapter":args.tool,"alias":"" if args.model and not getattr(args,"alias_supplied",True) else args.alias or "","model":args.model,"effort":args.effort,"task_class":task_class},
        "resolved_model":model,"observed_model":None,"observed_source":None,"identity":identity,
        "provider":route.get("endpoint_provider") or args.tool,"transport":args.tool,"family":family,
        "effort_requested":args.effort,"effort_applied":effort,"cli_version":route.get("cli_version"),
        "fallback_from":getattr(args,"fallback_from",None),"notes":[],
        "line":with_pick_reason(f"Route: {label} ({family}; {identity})",getattr(args,"pick_reason",None))}
    if getattr(args,"pick_reason",None): provenance["pick_reason"]=args.pick_reason
    # Recorded so a resume or handoff keeps the task off free and prompt-training models.
    if getattr(args,"confidential",False): provenance["requested"]["confidential"]=True
    placement = json.loads(os.environ.get('PROVENANT_PLACEMENT_FALLBACK', 'null'))
    if placement:
        note = f'local fallback from {placement["from_host"]}: {placement["reason"]}' + (f': {placement["detail"]}' if placement.get('detail') else '')
        if note not in plan.setdefault('warnings', []):
            plan['warnings'].append(note)
    if plan.get('account') is not None:
        attempt_scope(args, number)['account'] = plan['account']
        provenance['account'] = plan['account']
    lineage = run_lineage.fields(run_dir) if (run_dir / "RUN_RECEIPT.json").exists() else run_lineage.fields()
    children = run_lineage.children(run_dir, args.task_id, number)
    return {"schema":"fabric.attempt.v1", **attempt_scope(args,number), **lineage, **({"children": children} if children else {}), **({'placement_fallback': placement} if placement else {}),"run_id":plan.get("run_id") or run_records.run_identity(run_dir),"task_id":args.task_id,"task_class":task_class,
        **({"batch_id":args.batch_id} if getattr(args,"batch_id",None) else {}),"profile":dispatch_preflight.profile_resolution(args),
        "attempt":number,"state":"running","status":None,"usage_source":"missing","mode":args.access_mode,"cwd":plan.get("cwd") or str(Path.cwd().resolve()),
        "retry_of":getattr(args,"retry_of",None),"escalation_reason":getattr(args,"escalation_reason",None),
        **({'delivery_run_id': args.delivery_run_id} if getattr(args, 'delivery_run_id', None) is not None else {}),
        "workspace_root":plan.get("workspace_root") or str(Path(getattr(args,"workspace_root",None) or Path.cwd()).resolve()),
        **({"read_roots":[str(root) for root in args.read_roots]} if getattr(args,"read_roots",None) else {}),
        "worktree":str(args.worktree) if args.worktree else None,"started_at":started_at,"ended_at":None,"last_progress_at":started_at,
        "pgid":None,"pgid_started_at":None,"session_id":plan.get("session_id"),"retryable":False,"reset_at":None,"retry_after":None,"fix":None,
        "evidence":{"exit":None,"signal":None,"signature":None,"excerpt":""},"question":None,
        "applied":plan.get("applied",{"sandbox":None,"network":None,"add_dirs":[],"guarantee":"prompt_only"}),
        "warnings":list(plan.get("warnings",[])) + list(getattr(getattr(args, "_secret_scan", None), "warnings", [])) + list(getattr(args, "_branch_warnings", [])) + list(getattr(args, "_resume_warnings", [])),"provenance":provenance,
        "secret_scan":{"allow_secrets":getattr(args,"allow_secrets",False),
                       "finding_names":getattr(getattr(args,"_secret_scan",None),"names",lambda:[])()},
        "timing":{"phases":getattr(args,"_phase_timings",{}).copy()},
        "paths":{"result":run_records.relative_path(run_dir,attempt_dir/"result.md"),"stderr":run_records.relative_path(run_dir,attempt_dir/"stderr.log"),
                 "events":run_records.relative_path(run_dir,attempt_dir/"events.jsonl"),"watchdog":run_records.relative_path(run_dir,attempt_dir/"watchdog.json"),
                 "receipt":f"tasks/{args.task_id}/attempt-{number:03d}/attempt.json"},"digest":""}

def publish_contract(run_dir,row):
    row["run_dir"] = str(run_dir)
    path=run_dir/row["paths"]["receipt"]
    run_records.ensure_owned_directory(run_dir,path.parent)
    with attempt_custody(run_dir, path):
        with run_lineage.journal_custody(run_dir, row["task_id"], row["attempt"]):
            children = run_lineage.children(run_dir, row["task_id"], row["attempt"])
            if row["state"] == "terminal" and row["status"] in {"cancelled", "interrupted"}:
                # Registration waits on this same journal lock and will see terminal state.
                transient_skips = {"process census unavailable", "process signal unavailable",
                                   "owner verification or signal unavailable", "owner identity unavailable",
                                   "environment unreadable"}
                evidence = {(child.get("run_dir"), child.get("run_token")): child["cancellation"]
                            for child in row.get("children", []) if "cancellation" in child
                            and not (child["cancellation"].get("status") == "skipped"
                                     and child["cancellation"].get("reason") in transient_skips)}
                pending = [child for child in children
                           if (child.get("run_dir"), child.get("run_token")) not in evidence]
                run_lineage.cancel_children(run_dir, row["task_id"], row["attempt"], child_rows=pending)
                evidence.update({(child.get("run_dir"), child.get("run_token")): child["cancellation"]
                                 for child in pending if "cancellation" in child})
                children = run_lineage.children(run_dir, row["task_id"], row["attempt"])
                for child in children:
                    cancellation = evidence.get((child.get("run_dir"), child.get("run_token")))
                    if cancellation is not None:
                        child["cancellation"] = cancellation
            if children:
                row["children"] = children
            if row["state"] == "terminal":
                run_lineage.annotate_child_errors(row)
            row["digest"]=render_digest(row)
            run_records.write_owned(run_dir,path,json.dumps(row,indent=2)+"\n")

def terminal_contract(args,run_dir,legacy,adapter,number,attempt_dir,active=None):
    run_lineage.refresh_external(run_dir, args.task_id, number, lambda ids:
        dispatch_preflight._fabric_hosts_owner().lane_owner().client_call("status", str(Path.cwd()), {"ids": ids}, federated_read=True))
    row=contract_row(args,run_dir,number,attempt_dir,getattr(args,"_last_plan",{}),legacy["started_at"])
    for field in ("pgid", "pgid_started_at", "last_progress_at", "process_error"):
        if field in (active or {}): row[field]=active[field]
    if legacy.get("process_error"): row["process_error"]=legacy["process_error"]
    status=adapter.get("status")
    if legacy["outcome"] in {"result_invalid_path","result_integrity_error","adapter_receipt_invalid","manifest_write_error","terminal_envelope_invalid"}: status="failed"
    elif legacy["status"] in {"cancelled","timed_out"}: status=legacy["status"]
    elif legacy["status"]=="blocked": status="input_required"
    elif status == "ok" and legacy["status"] == "failed": status="failed"
    elif status=="quarantined" and legacy["status"]=="failed": status="failed"
    refusal=dispatch_preflight.route_refusal(adapter,args.tool)
    if refusal: status=refusal["status"]
    if status not in TERMINAL_STATUSES: status="failed"
    for field in ("reason","session_id","retryable","reset_at","retry_after","fix","error","evidence","applied","context","usage_source","warnings","reaped","spared","children","provenance","pgid","pgid_started_at","last_progress_at","landable","quarantine","result_partial","empty_patch","model_check","model_check_detail","model_changes"):
        if field in adapter: row[field]=adapter[field]
    if row.get("task_class") and isinstance(row.get("provenance"), dict):
        row["provenance"]["requested"] = {**(row["provenance"].get("requested") or {}), "task_class": row["task_class"]}
    # The provider's provenance replaces the recorded one; keep the confidential request so resume and handoff honour it.
    if getattr(args,"confidential",False) and isinstance(row.get("provenance"),dict):
        row["provenance"]["requested"]={**(row["provenance"].get("requested") or {}),"confidential":True}
    reason=getattr(args,"pick_reason",None)
    if reason and isinstance(row.get("provenance"),dict) and isinstance(row["provenance"].get("line"),str):
        row["provenance"]["line"]=with_pick_reason(row["provenance"]["line"],reason)
        row["provenance"]["pick_reason"]=reason
    row["warnings"] = list(dict.fromkeys(
        list(row.get("warnings", [])) + list(getattr(getattr(args, "_secret_scan", None), "warnings", []))
        + list(getattr(args, "_memory_warnings", [])) + list(getattr(args, "_branch_warnings", []))
        + list(getattr(args, "_resume_warnings", [])) + list(getattr(args, "_bookkeeping_warnings", []))))
    row["timing"]["queued_seconds"] = getattr(args, "_queued_seconds", 0.0)
    if legacy.get("process_error", "").startswith(("Set .agents/fabric-policy.json memory_floor_percent",
                                                    "Set .agents/fabric-policy.json disk_floor_bytes")):
        row["fix"] = legacy["process_error"]
    if legacy["outcome"] == "host_disk_full":
        row["error"] = "host_disk_full"
        row["fix"] = "host_disk_full: free disk space or lower the mode's disk_floor_bytes in .agents/fabric-policy.json."
    if legacy["outcome"] == "memory_unavailable":
        row["error"] = "memory_unavailable"
        row["fix"] = "memory_unavailable: free memory or lower the mode's memory_floor_percent in .agents/fabric-policy.json."
    if refusal:
        row["fix"]=adapter.get("fix") or adapter.get("reason") or refusal["fix"];row["evidence"]=refusal["evidence"];row["error"]=refusal["error"]
    row.update(state="terminal",status=status,ended_at=legacy["finished_at"],question=adapter.get("question") or (legacy.get("question") or {}).get("prompt"))
    row["usage_source"] = adapter.get("usage_source") or context_usage.usage_source(row.get("context") or {})
    if legacy.get("process_error") and legacy["outcome"] in {"provider_lost", "host_disk_full", "process_spawn_error"}:
        row["evidence"] = {**row["evidence"], "signature": legacy["outcome"],
                           "excerpt": legacy["process_error"][:400]}
    for name in ("events", "stderr"):
        value = (adapter.get("paths") or {}).get(name)
        if value:
            try:
                row["paths"][name] = run_records.relative_path(run_dir, run_dir / value)
            except ValueError:
                pass  # Keep the contract path when the adapter's lies outside the run.
    row["evidence"].update(process_evidence(run_dir, row))
    row["error_class"] = provider_output.error_class(status, row["evidence"].get("signature"))
    if not legacy.get("result"): row["paths"]["result"]=None
    row["legacy_attempt_path"]=legacy["attempt_path"]
    row["requested_route"]=legacy["requested_route"]
    run_lineage.annotate_child_errors(row)
    return row

def resume_relaunch_context(run_dir, previous):
    tail=""
    result=previous["paths"].get("result")
    if result:
        retained=run_records.retained_path(run_dir,result)
        try:
            (run_dir / retained).lstat()
        except FileNotFoundError:
            pass  # An interrupted provider may never have produced a result.
        else:
            fd, _, _ = open_contained_regular(run_dir, retained, os.O_RDONLY, label="resume result")
            try:
                os.lseek(fd,max(0,os.fstat(fd).st_size-4096),os.SEEK_SET)
                tail=os.read(fd,4096).decode(errors="replace")
            finally:
                os.close(fd)
    return (previous.get("question") or "")+"\n"+tail

class ResumeError(ValueError):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code

def prepare_resume(args):
    paths=sorted((args.run_dir.resolve()/"tasks").glob("*/attempt-*/attempt.json"))
    rows=[json.loads(path.read_text()) for path in paths]
    rows=[row for row in rows if row.get("run_id")==args.resume]
    if not rows: raise ValueError("resume run not found")
    tasks={row["task_id"] for row in rows}
    if args.task_id is not None:
        rows=[row for row in rows if row["task_id"]==args.task_id]
        if not rows: raise ResumeError("resume_task_unknown",f"task {args.task_id} has no attempt in this run")
    elif len(tasks)>1: raise ResumeError("resume_task_required","resume a batch task: pass task_id")
    previous=max(rows,key=lambda row:row["attempt"])
    if previous["state"]!="terminal": raise ValueError("resume requires a terminal attempt")
    pinned=getattr(args,"resume_attempt",None)
    if pinned is not None:
        # A named session continues from its last clean attempt, not the latest one.
        previous=next((row for row in rows if row["attempt"]==pinned),None)
        if previous is None: raise ResumeError("resume_attempt_unknown",f"attempt {pinned} is not recorded in this run")
    route=previous.get("requested_route") or {}
    requested=previous["provenance"]["requested"]
    inherit_delivery_run_id(args, previous)
    if (args.tool and args.tool!=requested["adapter"]) or (args.model and args.model!=previous["provenance"]["resolved_model"]):
        raise ValueError("dispatch a new run")
    args.tool=requested["adapter"];args.model=previous["provenance"]["resolved_model"];args.alias=None;args.task_class=None
    # An effort the provider only reported was never sent, so resume does not send it.
    args.effort=None if previous["provenance"].get("effort_observed_source") else previous["provenance"]["effort_applied"];args.task_id=previous["task_id"]
    # OpenCode applies effort only as a `#variant`; keep that intent so the router sends it again.
    if args.tool=="opencode" and args.effort and "#" not in args.model: args.model=args.model+"#"+args.effort
    args.access_mode=previous["mode"];args.worktree=Path(previous["worktree"]) if previous.get("worktree") else None
    supplied_cwd = args.provider_cwd
    args.provider_cwd=Path(previous["cwd"]) if previous["mode"]=="read_only" else None
    if (previous["mode"] == "read_only" and supplied_cwd is not None and
            os.environ.get("PROVENANT_REMOTE_LANE") == "1" and supplied_cwd != args.provider_cwd):
        dispatch_preflight._fabric_hosts_owner().lane_owner().code().validate_reader_resume(
            supplied_cwd, previous["run_id"], previous["task_id"])
        args.provider_cwd = supplied_cwd
    args.workspace_root=Path(previous.get("workspace_root") or (previous.get("workspace") or {}).get("root") or Path.cwd()).expanduser().resolve()
    args.sandbox=previous["applied"]["sandbox"];args.network=None if previous["applied"]["network"] is None else str(previous["applied"]["network"]).lower()
    applied_add_dirs = previous["applied"].get("add_dirs") or []
    valid_add_dirs = []
    missing_add_dirs = []
    for item in applied_add_dirs:
        p = Path(item).expanduser()
        target = p if p.is_absolute() else (args.workspace_root / p)
        if target.exists():
            valid_add_dirs.append(item)
        else:
            missing_add_dirs.append(item)
    args.add_dirs = valid_add_dirs
    if missing_add_dirs:
        args._resume_warnings = getattr(args, "_resume_warnings", []) + [
            f"dropped missing recorded add_dir: {item}" for item in missing_add_dirs
        ]
    args.resume_session=previous["session_id"]
    if args.tool == "claude":
        args.account=previous.get("account")
    args.read_roots=previous.get("read_roots") or []
    # A read root was canonical when granted; a directory since swapped for a link is not that grant.
    bound=[*args.read_roots,*([args.provider_cwd] if args.read_roots and args.provider_cwd else [])]
    if any(Path(path).resolve()!=Path(path) for path in bound):
        raise ResumeError("resume_read_root_changed","a read root or cwd now resolves elsewhere; dispatch a new run")
    args.capabilities=previous["applied"].get("capabilities", [])
    args.fallback="false"
    args.confidential=bool(getattr(args,"confidential",False) or requested.get("confidential") is True)
    for field in ("intent", "orchestrator_family", "role", "risk_tier", "model_override_tier", "reviewer_id", "preface"):
        if field in route:
            setattr(args, field, route[field])
    args.resume_previous=previous
    observed_session=False
    if args.tool=="claude" and incomplete_provider_turn(previous):
        events=previous["paths"].get("events")
        if events:
            retained=run_records.retained_path(args.run_dir,events)
            try:
                (args.run_dir / retained).lstat()
            except FileNotFoundError:
                data=b""
            else:
                data=read_bound_bytes(args.run_dir,retained,label="resume events")
            for line in data.splitlines():
                if b'"session_id"' not in line:
                    continue
                try:
                    event=json.loads(line)
                except ValueError:
                    continue
                if isinstance(event,dict) and event.get("session_id")==args.resume_session:
                    observed_session=True
                    break
    required=getattr(args,"require_session",None)
    if required is not None and args.resume_session!=required:
        raise ResumeError("continuation_unsupported",f"attempt {previous['attempt']} did not record provider session {required}; "
                          "pass fresh: true to start a new session primed with its last result")
    if not args.resume_session or args.tool=="copilot" or (
        args.tool=="claude" and incomplete_provider_turn(previous)
        and not observed_session
    ):
        if required is not None:
            raise ResumeError("continuation_unsupported",f"{args.tool} cannot continue provider session {required}; "
                              "pass fresh: true to start a new session primed with its last result")
        if (args.tool=="claude" and previous["mode"]=="worktree_write"
            and incomplete_provider_turn(previous)):
            raise ValueError("Claude session unavailable after incomplete writer turn; review worktree changes, then dispatch a new run")
        args.resume_session=None
        args.resume_relaunch=resume_relaunch_context(args.run_dir,previous)
    if args.context_ceiling is None:
        prior=previous.get("applied") or {}
        args.context_ceiling=prior.get("context_ceiling_requested") or (prior.get("context_ceiling_tokens") if prior.get("context_ceiling")=="enforced" else None)
    if args.resume_session:
        args.resume_advice=(previous,previous["task_id"] if len(tasks)>1 else None)
    receipt=json.loads(read_bound_bytes(args.run_dir,"RUN_RECEIPT.json",label="RUN_RECEIPT.json"))
    receipt.update(status="active",closed_at=None)
    run_records.write_owned(args.run_dir,args.run_dir/"RUN_RECEIPT.json",json.dumps(receipt,indent=2)+"\n")

class _DispatchPlan(NamedTuple):
    owner_started_ms: Any
    run_dir: Path
    workspace: Path
    workspace_observation: dict[str, Any]
    run_receipt: dict[str, Any]
    batch_dir: Path | None
    original_prompt: str | None
    secret_scan_result: secret_scan.ScanResult
    git_evidence_requested: bool
    git_evidence_identity: dict[str, Any] | None
    retry_of: str | None
    attempt_number: int
    attempt_id: str
    attempt_dir: Path
    prompt_path: Path
    result_path: Path
    adapter_path: Path
    stderr_path: Path
    evidence_dir: Path | None
    evidence_path: Path | None
    git_evidence_digest: str | None
    command: list[str]
    requested_route: dict[str, Any]
    worktree_lease: Any

class _DispatchLaunch(NamedTuple):
    started_at: str
    started: float
    observed_exit: bool
    exit_code: int | None
    process_error: str
    process_errno: int | None
    process: subprocess.Popen[Any] | None
    active: dict[str, Any] | None
    old_handlers: dict[int, Any]

class _DispatchSettlement(NamedTuple):
    finalize_started: float
    adapter: dict[str, Any]
    status: str
    record: dict[str, Any]
    checkpoint_written: bool

def classify_attempt(
    *,
    process_error,
    process_errno,
    process,
    observed_exit,
    result_invalid,
    adapter_status,
    deadline_reached,
    completed_cleanly,
    receipt_error,
    result_integrity_error,
    terminal_envelope_error,
    adapter,
    question,
    exit_code,
    result_nonempty,
) -> tuple[str, str, dict[str, Any] | None]:
    if process_error in {"memory_unavailable", "host_disk_full"}:
        status = "failed"
        outcome = process_error
    elif process_error == "timeout":
        status = "timed_out"
        outcome = "timeout"
    elif process_error == "cancelled":
        status = "cancelled"
        outcome = "cancelled"
    elif not observed_exit:
        status = "failed"
        outcome = "host_disk_full" if process_errno == errno.ENOSPC else "process_spawn_error" if process is None else "provider_lost"
    elif result_invalid:
        status = "failed"
        outcome = "result_invalid_path"
    elif adapter_status in {"timeout","timed_out"}:
        # The provider reached its own deadline first, which is what the smaller
        # provider timeout is for.
        status = "timed_out"
        outcome = "provider_timeout"
    elif deadline_reached and not completed_cleanly:
        status = "timed_out"
        outcome = "deadline_exceeded"
    elif adapter_status in {"ok", "quarantined"} and receipt_error:
        status = "failed"
        outcome = "adapter_receipt_invalid"
    elif result_integrity_error:
        status = "failed"
        outcome = "result_integrity_error"
    elif terminal_envelope_error:
        status = "failed"
        outcome = "terminal_envelope_invalid"
    elif adapter_status == "input_required":
        question={"code":"needs_input","prompt":adapter.get("question") or "Input required"}
        status="blocked"
        outcome="question"
    elif question is not None:
        status = "blocked"
        outcome = "question"
    elif adapter_status == "quarantined" and adapter.get("exit") == 0 and result_nonempty:
        status = "quarantined"
        outcome = "quarantined"
    elif exit_code == 0 and adapter_status == "ok" and result_nonempty:
        status = "ok"
        outcome = "ok"
    elif exit_code == 0 and adapter_status == "ok":
        status = "failed"
        outcome = "result_missing_or_empty"
    else:
        status = "failed"
        outcome = adapter_status or ("adapter_exit" if exit_code else "empty_result")

    return status, outcome, question

def _plan_dispatch(args: argparse.Namespace, custody=None) -> int | _DispatchPlan:
    args._last_plan={}
    args._last_row=None
    args._attempt_scopes={}
    args._bookkeeping_warnings=[]
    args._memory_warnings=[]
    owner_started_ms = None
    try:
        incoming = ({} if getattr(args, "retry_of", None) or getattr(args, "fallback_from", None)
                    else json.loads(os.environ.get("PROVENANT_FABRIC_PHASES", "{}")))
        owner_started_ms = incoming.get("owner_started_at_ms")
        measured = {key: round(float(value), 3) for key, value in incoming.items()
                    if key in {"validate", "run_dir_init", "snapshot"}
                    and type(value) in (int, float) and math.isfinite(value) and value >= 0}
    except (ValueError, TypeError, AttributeError):
        measured = {}
    args._phase_timings = dict.fromkeys(
        ("validate", "run_dir_init", "owner_setup", "route_plan", "snapshot", "spawn", "provider", "finalize")
    )
    args._phase_timings.update(measured)
    run_dir = args.run_dir.resolve()
    workspace = Path(getattr(args, "workspace_root", None) or Path.cwd()).expanduser().resolve()
    provider_cwd = Path(args.provider_cwd).expanduser().resolve() if args.provider_cwd else workspace
    workspace_observation = writer_lease.workspace_identity(workspace, provider_cwd)
    if not contains_run(run_dir, workspace):
        return fail(run_dir, "run_dir_invalid", "run directory must be inside run_root(cwd)")
    if not run_dir.is_dir():
        return fail(run_dir, "run_custody_missing", f"run directory does not exist: {run_dir}")
    try:
        (run_dir / "RUN_RECEIPT.json").lstat()
    except FileNotFoundError:
        return fail(run_dir, "run_custody_missing", "RUN_RECEIPT.json does not exist")
    except OSError:
        return fail(run_dir, "run_custody_invalid", "RUN_RECEIPT.json is unavailable")
    try:
        (run_dir / "MANIFEST.md").lstat()
    except FileNotFoundError:
        return fail(run_dir, "run_custody_missing", "MANIFEST.md does not exist")
    except OSError:
        return fail(run_dir, "run_custody_invalid", "MANIFEST.md is unavailable")
    try:
        contained_regular_path(run_dir, "MANIFEST.md", "MANIFEST.md")
        run_receipt = json.loads(
            read_bound_bytes(run_dir, "RUN_RECEIPT.json", label="RUN_RECEIPT.json").decode("utf-8")
        )
    except (OSError, UnicodeDecodeError, ValueError, OwnedFileError):
        return fail(run_dir, "run_custody_invalid", "RUN_RECEIPT.json is not valid JSON")
    receipt_error = active_receipt_error(run_receipt)
    if receipt_error:
        status = "run_custody_closed" if "active orchestration run" in receipt_error else "run_custody_invalid"
        return fail(run_dir, status, receipt_error)
    if not dispatch_preflight.TASK_ID_RE.fullmatch(args.task_id):
        return fail(run_dir, "invalid_task_id", "task id must contain only letters, numbers, '.', '_' or '-'")
    batch_dir = None
    if args.batch_id is not None:
        if not BATCH_ID_RE.fullmatch(args.batch_id):
            return fail(run_dir, "invalid_batch_id", "batch id is invalid")
        batch_dir = run_dir / "dispatch" / "batches" / args.batch_id
        if batch_dir.is_symlink() or not batch_dir.is_dir():
            return fail(run_dir, "batch_path_invalid", "batch directory does not exist")
    original_prompt = None
    if args.prompt_file is not None:
        source = args.prompt_file.expanduser()
        original_prompt = str((source if source.is_absolute() else workspace / source).resolve())
    args.original_prompt_file = original_prompt
    try:
        read_roots = [str(root) for root in getattr(args, "read_roots", None) or []]
        protected = provider_exec.provider_confinement.protected_paths(workspace, provider_cwd, args.worktree, read_roots)
        if protected and (args.prompt_file is not None or args.add_dirs):
            task = {"id": args.task_id, "adapter": args.tool, "access_mode": args.access_mode,
                    "worktree": str(args.worktree) if args.worktree else None,
                    "cwd": str(provider_cwd), "prompt_file": str(args.prompt_file) if args.prompt_file else None,
                    "add_dirs": args.add_dirs, "read_roots": read_roots, "alias": args.alias, "model": args.model,
                    "effort": args.effort, "fallback": args.fallback,
                    "role": args.role, "task_class": args.task_class,
                    "confidential": bool(getattr(args, "confidential", False))}
            if args.prompt_file is None:
                task.pop("prompt_file")
                task["prompt"] = "protected input preflight"
            checked = dispatch_preflight.preflight_tasks([task], workspace)
            if checked["status"] != "validated":
                return fail(run_dir, checked.get("error", "protected_path_denied"), checked["fix"])
    except (OSError, ValueError) as exc:
        return fail(run_dir, "protected_path_denied", str(exc))
    try:
        prompt_bytes = (dispatch_preflight.read_prompt_input(args.prompt_file, workspace, run_dir, getattr(args, "read_roots", None) or [])
                        if args.prompt_file is not None else sys.stdin.buffer.read())
    except dispatch_preflight.PreflightError as exc:
        return fail(run_dir, exc.code, str(exc))
    except OSError as exc:
        return fail(run_dir, "prompt_unavailable", str(exc))
    try:
        secret_scan_result = secret_scan.scan_inputs(
            prompt_bytes or b"", str(args.prompt_file) if args.prompt_file else "<prompt>", args.add_dirs,
            workspace_root=workspace)
    except (OSError, subprocess.SubprocessError) as exc:
        return fail(run_dir, "secret_scan_unavailable", f"Make dispatch inputs readable for the secret scan: {exc}")
    if secret_scan_result.budget_exceeded and not getattr(args, "allow_secrets", False):
        print(json.dumps({"status": "rejected", "error": "secret_scan_budget_exceeded", "fix": secret_scan_result.fix()}))
        return 2
    if secret_scan_result.findings and not getattr(args, "allow_secrets", False):
        print(json.dumps({"status": "rejected", "error": "secret_detected", "fix": secret_scan_result.fix()}))
        return 2
    args._secret_scan = secret_scan_result
    try:
        run_records.ensure_owned_directory(run_dir, run_dir / "dispatch" / "tasks")
        if not args.batch_child:
            run_records.reconcile_manifest(run_dir, custody)
            run_records.ensure_manifest_appendable(run_dir)
    except run_records.AttemptEvidenceError as exc:
        return fail(run_dir, "attempt_evidence_incomplete", str(exc))
    except OSError as exc:
        return fail(run_dir, "manifest_not_appendable", f"MANIFEST.md is not appendable: {exc}")
    git_evidence_requested = args.git_evidence is not None
    git_evidence_identity: dict[str, Any] | None = None
    git_evidence_source: Path | None = None
    if args.git_evidence is not None:
        if args.tool != "agy":
            return fail(run_dir, "git_evidence_requires_agy", "Git evidence is currently supported only for Agy")
        git_evidence_source = args.git_evidence.expanduser()
        if not git_evidence_source.is_absolute():
            git_evidence_source = workspace / git_evidence_source
    if args.access_mode == "worktree_write":
        # The writable route stays an explicit, ordinary-intent request bound to
        # one worktree. Assurance work keeps the read-only guarantee it certifies.
        if args.worktree is None:
            return fail(run_dir, "worktree_required", "worktree_write access requires --worktree")
        if args.intent != "ordinary":
            return fail(run_dir, "worktree_write_intent_denied", "worktree_write access requires ordinary intent")
        if args.tool not in writer_lease.WORKTREE_WRITE_ADAPTERS:
            return fail(
                run_dir, "worktree_write_adapter_unsupported",
                f"worktree_write access is unsupported for adapter: {args.tool}",
            )
        try:
            args.worktree = writer_lease.resolve_writer_worktree(args.worktree)
            warning = writer_lease.branch_name_warning(args.worktree)
            args._branch_warnings = [warning] if warning else []
        except writer_lease.WorktreeLeaseError as exc:
            return fail(run_dir, "worktree_invalid", str(exc))
    elif args.worktree is not None:
        return fail(run_dir, "worktree_not_applicable", "--worktree requires --access-mode worktree_write")
    try:
        sandbox = args.sandbox or (
            "workspace-write" if args.access_mode == "worktree_write" else "read-only"
        )
        network = None if args.network is None else args.network == "true"
        args.capabilities = provider_exec.validate_capabilities(
            args.capabilities or [], adapter=args.tool, mode=args.access_mode,
            sandbox=sandbox, network=network,
        )
    except provider_exec.CapabilityError as exc:
        return fail(run_dir, exc.code, str(exc))
    if not CF_DISPATCH.is_file() or not os.access(CF_DISPATCH, os.X_OK):
        return fail(run_dir, "adapter_unavailable", f"provider adapter is missing or not executable: {CF_DISPATCH}")

    task_dir = run_dir / "dispatch" / "tasks" / args.task_id
    try:
        run_records.ensure_owned_directory(run_dir, task_dir)
    except run_records.AttemptEvidenceError as exc:
        return fail(run_dir, "attempt_path_invalid", str(exc))
    retry_of = None
    if args.escalation_reason and not args.retry_of:
        return fail(run_dir, "escalation_reason_invalid", "escalation-reason requires retry-of")
    if args.retry_of:
        retry_ref = Path(args.retry_of)
        if retry_ref.is_absolute() or retry_ref.name != args.retry_of or not ATTEMPT_ID_RE.fullmatch(args.retry_of):
            return fail(run_dir, "retry_of_invalid", "retry-of must name an attempt under the same task")
        retry_dir = task_dir / args.retry_of
        if not (retry_dir.is_dir() and (retry_dir / "attempt.json").is_file()):
            return fail(run_dir, "retry_of_missing", f"retry attempt does not exist: {args.retry_of}")
        retry_of = args.retry_of
        try:
            previous = retry_receipt(run_dir, retry_dir)
        except RetryReceiptInvalid as exc:
            return fail(run_dir, exc.code, str(exc))
        try:
            inherit_delivery_run_id(args, previous)
        except ValueError as exc:
            return fail(run_dir, 'invalid_delivery_run_id', str(exc))
    attempt_number = max(existing_attempt_number(task_dir), existing_attempt_number(run_dir / "tasks" / args.task_id))
    attempt_id = f"attempt-{attempt_number:03d}"
    attempt_dir = task_dir / attempt_id
    if os.environ.get('PROVENANT_REMOTE_LANE') == '1' and 'browser' in args.capabilities:
        hosts = dispatch_preflight._fabric_hosts_owner()
        browser_tmp = hosts.browser_temporary_directory(attempt_dir)
        socket = hosts.browser_socket_paths(browser_tmp)
        if not socket['fits']:
            return fail(run_dir, 'remote_temporary_path_too_long',
                f"Chromium SingletonSocket under {browser_tmp} needs "
                f"{socket['max_path_bytes']} bytes; the socket path must be shorter than "
                f"{socket['limit_bytes']} bytes. The short browser temporary path exceeds the socket limit.")
    try:
        create_contained_directory(
            run_dir, attempt_dir.relative_to(run_dir), label="attempt directory"
        )
    except OwnedFileError as exc:
        return fail(run_dir, "attempt_path_invalid", str(exc))
    prompt_path = attempt_dir / "prompt.md"
    result_path = attempt_dir / "result.md"
    adapter_path = attempt_dir / "adapter-receipt.json"
    stderr_path = attempt_dir / "stderr.log"
    evidence_dir = attempt_dir / "evidence" if git_evidence_requested else None
    evidence_path = evidence_dir / "git-evidence.md" if evidence_dir is not None else None
    if evidence_dir is not None and evidence_path is not None:
        try:
            create_contained_directory(run_dir, evidence_dir.relative_to(run_dir), label="Git evidence directory")
            git_evidence_identity, git_evidence_digest = _copy_git_evidence(
                workspace,
                run_dir,
                git_evidence_source if git_evidence_source is not None else evidence_path,
                evidence_path,
            )
        except (OSError, OwnedFileError, run_records.AttemptEvidenceError) as exc:
            return fail(run_dir, "git_evidence_invalid", str(exc))
        prompt_bytes = (
            (f"Read the supplied evidence files under {evidence_dir}. In particular, read "
             f"{evidence_path}. Use file-reading tools only; do not invoke shell or Git; "
             "answer from the supplied files.\n\n").encode("utf-8")
            + (prompt_bytes or b"")
        )
    try:
        atomic_write_contained(
            run_dir, prompt_path.relative_to(run_dir), prompt_bytes or b"", label="prompt"
        )
    except OwnedFileError as exc:
        return fail(run_dir, "attempt_path_invalid", str(exc))
    command = build_command(args, prompt_path, result_path, evidence_dir)
    requested_route = {
        "intent": args.intent,
        "preface": args.preface,
        "adapter": args.tool,
        "alias": args.alias or "",
        "task_class": args.task_class or getattr(args, "_rate_limit_task_class", ""),
        "role": args.role,
        "model": args.model or "",
        "effort": args.effort or "",
        "orchestrator_family": args.orchestrator_family or "",
        "risk_tier": args.risk_tier or "",
        "model_override_tier": args.model_override_tier or "",
        "reviewer_id": args.reviewer_id or "",
        "access_mode": args.access_mode,
        "worktree": str(args.worktree) if args.worktree else "",
        "allow_secrets": args.allow_secrets,
    }
    worktree_lease = None
    if args.access_mode == "worktree_write" and args.worktree is not None:
        refusal = writer_lease.graph_reservation_refusal(args.worktree)
        if refusal is not None:
            return fail(run_dir, "worktree_reserved", refusal)
        try:
            worktree_lease = writer_lease.acquire_worktree_lease(args.worktree)
        except writer_lease.WorktreeLeaseError as exc:
            return fail(run_dir, "worktree_busy", str(exc))
        hosts = dispatch_preflight._fabric_hosts_owner()
        try:
            transport = hosts._load_module('provenant_fabric_git', hosts.PRODUCT_ROOT / 'scripts/fabric_git.py')
            transport.validate_launch(hosts, hosts.config_path().parent, args.worktree,
                run_records.run_identity(run_dir, run_receipt), args.task_id)
        except hosts.HostError as exc:
            writer_lease.release_worktree_lease(worktree_lease)
            return fail(run_dir, exc.code, str(exc))
    return _DispatchPlan(
        owner_started_ms=owner_started_ms,
        run_dir=run_dir,
        workspace=workspace,
        workspace_observation=workspace_observation,
        run_receipt=run_receipt,
        batch_dir=batch_dir,
        original_prompt=original_prompt,
        secret_scan_result=secret_scan_result,
        git_evidence_requested=git_evidence_requested,
        git_evidence_identity=git_evidence_identity,
        retry_of=retry_of,
        attempt_number=attempt_number,
        attempt_id=attempt_id,
        attempt_dir=attempt_dir,
        prompt_path=prompt_path,
        result_path=result_path,
        adapter_path=adapter_path,
        stderr_path=stderr_path,
        evidence_dir=evidence_dir,
        evidence_path=evidence_path,
        git_evidence_digest=git_evidence_digest if git_evidence_identity is not None else None,
        command=command,
        requested_route=requested_route,
        worktree_lease=worktree_lease,
    )

def _launch_dispatch(args: argparse.Namespace, *, plan: _DispatchPlan) -> _DispatchLaunch:
    adapter_path = plan.adapter_path
    attempt_dir = plan.attempt_dir
    attempt_number = plan.attempt_number
    batch_dir = plan.batch_dir
    command = plan.command
    evidence_dir = plan.evidence_dir
    git_evidence_requested = plan.git_evidence_requested
    original_prompt = plan.original_prompt
    owner_started_ms = plan.owner_started_ms
    prompt_path = plan.prompt_path
    requested_route = plan.requested_route
    result_path = plan.result_path
    run_dir = plan.run_dir
    run_receipt = plan.run_receipt
    stderr_path = plan.stderr_path
    workspace = plan.workspace
    workspace_observation = plan.workspace_observation
    worktree_lease = plan.worktree_lease
    started_at = now()
    started = time.monotonic()
    observed_exit = False
    exit_code: int | None = None
    process_error = ""
    process_errno = None
    process = None
    active = None
    memory_lease = None
    admission_queued_seconds = 0.0
    provider_temporary = None
    cancelled = False
    old_handlers: dict[int, Any] = {}
    def admit_attempt(active, cancelled_now, *, cooldown_blocked=False):
        nonlocal started, started_at, memory_lease, process_error, admission_queued_seconds
        def warning(message):
            active["warnings"].append(message)
            args._memory_warnings = getattr(args, "_memory_warnings", []) + [message]
        def disk_available():
            floor = disk_floor_bytes(workspace, args.access_mode)
            if not floor:
                return True
            try:
                return shutil.disk_usage(run_dir).free >= floor
            except OSError as exc:
                warning(f"disk admission probe failed: {exc}; admitting attempt")
                return True
        if not disk_available():
            process_error = "host_disk_full"
            return False
        pacing = vars(args).pop("_rate_limit_retry", None)
        if pacing and not cooldown_blocked:
            active.update(state="queued", queue_reason="rate_limit", reason="waiting for model cooldown")
            publish_contract(run_dir, active)
            while not cancelled_now():
                delay = (datetime.fromisoformat(pacing["until"].replace("Z", "+00:00")) - datetime.now(UTC)).total_seconds()
                remaining = pacing["deadline"] - time.monotonic()
                if delay <= 0 or remaining <= 0:
                    break
                time.sleep(min(0.1, delay, remaining))
            args.timeout_seconds = min(args.timeout_seconds, pacing["deadline"] - time.monotonic())
            if cancelled_now() or args.timeout_seconds < pacing_policy()["minimum_budget_seconds"]:
                process_error = "cancelled" if cancelled_now() else "timeout"
                return False
        waiting_since = time.monotonic()
        def waiting(reason):
            active["state"] = "queued"
            active["queue_reason"] = "memory"
            if "admission" not in active:
                active["admission"] = memory_admission.queue_entry(workspace, args.access_mode)
            active["reason"] = reason
            active["timing"]["queued_since"] = waiting_since
            active["timing"]["queued_seconds"] = getattr(args, "_queued_seconds", 0.0)
            publish_contract(run_dir, active)
        try:
            memory_lease = memory_admission.admit(waiting, cancelled_now, warning,
                                                   waited_seconds=admission_queued_seconds,
                                                   workspace_root=workspace, mode=args.access_mode,
                                                   timeout_seconds=args.timeout_seconds,
                                                   queue_root=run_root(workspace) / ".agent-run",
                                                   queued_since=waiting_since)
        except memory_admission.MemoryUnavailableError as exc:
            process_error = exc.code
            memory_lease = None
        queued_seconds = time.monotonic() - waiting_since
        admission_queued_seconds += queued_seconds
        args._queued_seconds = getattr(args, "_queued_seconds", 0.0) + queued_seconds
        active["timing"].pop("queued_since", None)
        active.pop("admission", None)
        active["timing"]["queued_seconds"] = args._queued_seconds
        started = time.monotonic()
        started_at = now()
        if memory_lease is not None and not disk_available():
            process_error = "host_disk_full"
            memory_lease.close()
            memory_lease = None
        if memory_lease is not None and pacing and active["mode"] == "worktree_write":
            blocked = {**pacing["row"], "exit": 1, "pgid": None}
            writer_lease.guard_writer_retry(blocked, pacing["writer_start"], active["cwd"])
            if not blocked["retryable"]:
                blocked.update(status="writer_changed", retryable=False, reason=blocked["fix"],
                               evidence={"signature": "worktree_changed", "excerpt": blocked["fix"]})
                args._pacing_blocked = blocked
                memory_lease.close()
                memory_lease = None
        if memory_lease is not None:
            active["state"] = "running"
            active.pop("queue_reason", None)
            active.pop("reason", None)
            active["started_at"] = started_at
            active["last_progress_at"] = started_at
            publish_contract(run_dir, active)
        return memory_lease is not None
    if CF_DISPATCH == Path(__file__).with_name("cf_dispatch.py"):
        owner_cancel=[False]
        def cancel_owner(_signal,_frame): owner_cancel[0]=True
        old_handlers={sig:signal.getsignal(sig) for sig in (signal.SIGTERM,signal.SIGHUP)}
        for sig in old_handlers: signal.signal(sig,cancel_owner)
        try:
            plan_started = time.monotonic()
            if (type(owner_started_ms) in (int, float) and math.isfinite(owner_started_ms)
                    and not getattr(args, "fallback_from", None)):
                args._phase_timings["owner_setup"] = round(max(0, time.time() * 1000 - owner_started_ms), 3)
            plan_environment=dispatch_preflight.routing_environment(preserve_instance=True)
            if git_evidence_requested: plan_environment.pop("CF_DISPATCH_AGY_ADD_DIR",None)
            fast_plan = dispatch_preflight.fast_fabric_plan(args, prompt_path, result_path, workspace)
            planning = None
            if fast_plan is None:
                planning = subprocess.run([*command,"--plan-only"],cwd=workspace,env=plan_environment,capture_output=True,text=True,timeout=30)
            args._phase_timings["route_plan"] = round((time.monotonic() - plan_started) * 1000, 3)
            plan = fast_plan if fast_plan is not None else dispatch_preflight.planner_result(planning)
            if getattr(args, "confidential", False) and plan.get("schema") == "fabric.exec-plan.v1":
                refusal = dispatch_preflight.confidential_refusal(plan.get("adapter", args.tool), plan.get("model"), plan.get("route"))
                if refusal and not args.model:
                    # Step to the alias's first safe member before refusing; the preflight made the same choice.
                    route_record = plan.get("route") if isinstance(plan.get("route"), dict) else {}
                    for alternative in exec_routing.confidential_alternatives(
                            args.tool, plan.get("model"), route_record.get("alias") or args.alias):
                        alternate = argparse.Namespace(**vars(args))
                        alternate.model, alternate.alias, alternate.task_class = alternative, None, None
                        resolved = subprocess.run([*build_command(alternate, prompt_path, result_path, evidence_dir), "--plan-only"],
                                                  cwd=workspace, env=dispatch_preflight.routing_environment(preserve_instance=True), capture_output=True, text=True, timeout=30)
                        try:
                            replacement = json.loads(resolved.stdout)
                        except ValueError:
                            continue
                        if (replacement.get("schema") == "fabric.exec-plan.v1" and not dispatch_preflight.confidential_refusal(
                                args.tool, replacement.get("model"), replacement.get("route"))):
                            replacement.setdefault("warnings", []).append(
                                f"confidential: skipped {args.tool}/{plan.get('model')}; using {alternative}")
                            plan, refusal = replacement, None
                            break
                if refusal:
                    plan = {**dispatch_preflight.router_failure("confidential_route", args.tool), "fix": refusal}
            if plan.get("schema") == "fabric.exec-plan.v1":
                old_run_dir = plan.get("run_dir")
                plan["run_dir"] = str(attempt_dir)
                boundary_paths = plan.get("applied", {}).get("write_boundary", {}).get("writable_paths")
                if isinstance(boundary_paths, list):
                    plan["applied"]["write_boundary"]["writable_paths"] = [
                        str(attempt_dir) if path == old_run_dir else path for path in boundary_paths]
                plan["original_prompt_file"] = original_prompt
                provider_cwd = Path(plan.get("cwd") or workspace).resolve()
                workspace_observation["cwd"] = str(provider_cwd)
                plan.update(timeout_seconds=args.timeout_seconds,run_id=run_records.run_identity(run_dir,run_receipt),chair=os.environ.get("PROVENANT_CHAIR") or os.environ.get("AGENT_FABRIC_SEAT", ""),fallback_from=getattr(args,"fallback_from",None))
                if hasattr(args,"resume_relaunch"):
                    plan["prompt"] += "\n\nPrevious turn and question:\n"+args.resume_relaunch
                    plan["argv"] = provider_exec.profile(args.tool).argv(plan)
                pinned_account = getattr(args, "_explicit_account", getattr(args, "account", None))
                # A resume can launch on its earlier account; only the named account counts as pinned.
                explicitly_pinned = (plan["adapter"] == "claude" and pinned_account is not None
                                     and plan.get("account") == pinned_account)
                cooling=exec_routing.cooling(args.tool,plan["model"], account=plan.get("account"),
                                             explicit_account=explicitly_pinned)
                if cooling:
                    pacing = getattr(args, "_rate_limit_retry", None)
                    if (args.model and getattr(args, "fallback_from", None)
                            and (not pacing or datetime.fromisoformat(cooling["cooling_until"].replace("Z", "+00:00"))
                                 > datetime.fromisoformat(pacing["until"].replace("Z", "+00:00")))):
                        plan["cooldown_blocked"] = cooling
                    elif args.model:
                        plan["warnings"].append("explicit model is cooling until "+cooling["cooling_until"])
                    else:
                        for candidate in exec_routing.candidates(plan,True,confidential=getattr(args,"confidential",False)):
                            if (candidate["adapter"] != "claude"
                                    and exec_routing.cooling(candidate["adapter"],candidate["model"])): continue
                            alternate=argparse.Namespace(**vars(args))
                            alternate.tool=candidate["adapter"];alternate.model=candidate["model"];alternate.alias=None;alternate.task_class=None;alternate.effort=candidate.get("effort")
                            resolved=subprocess.run([*build_command(alternate,prompt_path,result_path,evidence_dir),"--plan-only"],cwd=workspace,env=dispatch_preflight.routing_environment(preserve_instance=True),capture_output=True,text=True,timeout=30)
                            try: replacement=json.loads(resolved.stdout)
                            except ValueError: continue
                            if replacement.get("schema")!="fabric.exec-plan.v1": continue
                            if exec_routing.cooling(candidate["adapter"], candidate["model"],
                                                    account=replacement.get("account"),
                                                    explicit_account=explicitly_pinned and candidate["adapter"] == "claude"): continue
                            old_run_dir = replacement.get("run_dir")
                            replacement["run_dir"] = str(attempt_dir)
                            boundary_paths = replacement.get("applied", {}).get("write_boundary", {}).get("writable_paths")
                            if isinstance(boundary_paths, list):
                                replacement["applied"]["write_boundary"]["writable_paths"] = [
                                    str(attempt_dir) if path == old_run_dir else path for path in boundary_paths]
                            replacement["original_prompt_file"] = original_prompt
                            replacement.update(run_id=plan["run_id"],chair=plan["chair"])
                            replacement["warnings"].append("skipped cooling alias candidate "+args.tool+"/"+plan["model"])
                            plan=replacement;args.tool=candidate["adapter"];break
                        else:
                            plan["warnings"].append("all alias candidates cooling")
                            plan["cooldown_blocked"]=cooling
                if explicitly_pinned and plan["adapter"] == "claude" and not plan.get("cooldown_blocked"):
                    warning = exec_routing.pinned_account_warning(plan["adapter"], pinned_account)
                    if warning:
                        plan["warnings"].append(warning)
                        print(warning, file=sys.stderr)
                ceiling = args.context_ceiling if args.context_ceiling is not None else dispatch_preflight.profile_knob(args, "context.ceiling_tokens")
                if ceiling is not None:
                    context_usage.apply_ceiling(plan,ceiling,provider_exec.profile(plan["adapter"]).argv)
                if getattr(args,"resume_advice",None) and plan.get("resume_session"):
                    previous_row,advice_task=args.resume_advice
                    args.resume_warning=context_usage.resume_warning(previous_row,plan["adapter"],
                        context_usage.effective_ceiling(plan["applied"]),args.resume,task_id=advice_task)
                    if args.resume_warning: plan["warnings"].insert(0,args.resume_warning)
                plan["silence_seconds"] = dispatch_preflight.profile_knob(args, "lanes.silence_seconds", 300)
                plan["_lineage_token"] = run_lineage.owner_token(run_dir)
                plan["attempt_identity"] = {"run_id": run_records.run_identity(run_dir, run_receipt),
                    "task_id": args.task_id, "attempt": attempt_number, "run_dir": str(run_dir)}
                active = contract_row(args,run_dir,attempt_number,attempt_dir,plan,started_at)
                active["requested_route"] = requested_route
                spawn_started = time.monotonic()
                provider_started_at = [None]
                def bookkeeping_warning(exc):
                    if not getattr(args, "_bookkeeping_warnings", []):
                        warning = "provider bookkeeping write failed: " + str(exc)
                        active["warnings"].append(warning)
                        args._bookkeeping_warnings = [warning]
                def provider_started(child):
                    nonlocal process
                    process=child
                    if memory_lease is not None:
                        memory_lease.started()
                    provider_started_at[0] = time.monotonic()
                    args._phase_timings["spawn"] = round((provider_started_at[0] - spawn_started) * 1000, 3)
                    active["pgid"]=child.pid
                    active["pgid_started_at"]=process_info.start_time(child.pid)
                    _record_provider_process(run_dir,child,task_id=args.task_id,attempt=attempt_number)
                    try:
                        run_records.write_owned(run_dir,attempt_dir/"pgid",str(child.pid)+"\n")
                        publish_contract(run_dir,active)
                    except (OSError, OwnedFileError) as exc:
                        bookkeeping_warning(exc)
                progress_publication=[0.0]
                def progress(at):
                    active["last_progress_at"]=at
                    if time.monotonic()-progress_publication[0]>=1:
                        try:
                            publish_contract(run_dir,active)
                        except (OSError, OwnedFileError) as exc:
                            bookkeeping_warning(exc)
                        else:
                            progress_publication[0]=time.monotonic()
                def cancellation():
                    return owner_cancel[0] or run_records.cancellation_marker_present(run_dir,attempt_dir) or (batch_dir is not None and run_records.cancellation_marker_present(run_dir,batch_dir))
                admitted = admit_attempt(active, cancellation, cooldown_blocked=bool(plan.get("cooldown_blocked")))
                plan["timeout_seconds"] = dispatch_preflight.provider_timeout_seconds(args.timeout_seconds)
                if plan["adapter"] == "agy" and admitted:
                    plan["argv"] = provider_exec.profile("agy").argv(plan)
                # Snapshot before launch: the timeout walk compares mtimes with this wall time.
                writer_start = ((writer_lease.writer_worktree_state(plan["cwd"]), time.time())
                                if admitted and plan["mode"] == "worktree_write" else None)
                spawn_started = time.monotonic()
                if not admitted:
                    plan["warnings"] = active["warnings"]
                    adapter_record = vars(args).pop("_pacing_blocked", None)
                    if adapter_record is None:
                        process_error = process_error or "cancelled"
                        adapter_record = {"status": "failed" if process_error in {"memory_unavailable", "host_disk_full"} else
                                          "timed_out" if process_error == "timeout" else "cancelled",
                                          "exit": 1, "warnings": active["warnings"]}
                else:
                    plan["warnings"] = active["warnings"]
                    adapter_record=provider_exec.execute(plan,result_path,events_path=attempt_dir/"events.jsonl",stderr_path=stderr_path,
                        on_start=provider_started,on_progress=progress,cancelled=cancellation)
                    _retire_provider_process(run_dir,process,task_id=args.task_id,attempt=attempt_number)
                    retry_plan = dispatch_preflight.effort_retry_plan(plan, adapter_record)
                    if retry_plan is not None and not cancellation():
                        plan = retry_plan
                        active["provenance"]["effort_applied"] = plan["effort"]
                        active["warnings"].append(plan["warnings"][-1])
                        publish_contract(run_dir, active)
                        provider_started_at[0] = None
                        adapter_record = provider_exec.execute(
                            plan, result_path, events_path=attempt_dir / "events.jsonl",
                            stderr_path=stderr_path, on_start=provider_started,
                            on_progress=progress, cancelled=cancellation,
                        )
                        _retire_provider_process(run_dir, process, task_id=args.task_id, attempt=attempt_number)
                relaunch_skipped = False
                args._phase_timings["provider"] = round((time.monotonic() - (provider_started_at[0] or spawn_started)) * 1000, 3)
                if (args.resume and args.tool=="claude" and plan.get("resume_session")
                    and adapter_record.get("status")=="failed"
                    and re.search(r"no conversation found",adapter_record.get("evidence",{}).get("excerpt") or "",re.I)):
                    previous=args.resume_previous
                    if getattr(args,"require_session",None) is not None:
                        # A named session never silently becomes a new provider session.
                        adapter_record["status"]="rejected"
                        adapter_record["error"]="continuation_unsupported"
                        adapter_record["fix"]="The provider no longer has this session; pass fresh: true to start a new session primed with its last result."
                        adapter_record["evidence"]["signature"]="continuation_unsupported"
                    elif (previous["mode"]=="worktree_write" and incomplete_provider_turn(previous)):
                        adapter_record["status"]="rejected"
                        adapter_record["fix"]="Review worktree changes, then dispatch a new run."
                        adapter_record["evidence"]["signature"]="resume_session_missing"
                    else:
                        for diagnostic in (attempt_dir/"events.jsonl",stderr_path):
                            if diagnostic.exists(): diagnostic.rename(diagnostic.with_name(diagnostic.name+".resume-failed"))
                        plan["resume_session"]=None
                        if getattr(args,"resume_warning",None) in plan["warnings"]: plan["warnings"].remove(args.resume_warning)
                        plan["session_id"]=str(uuid.uuid4())
                        args.resume_relaunch=resume_relaunch_context(run_dir,previous)
                        plan["prompt"] += "\n\nPrevious turn and question:\n"+args.resume_relaunch
                        plan["argv"]=provider_exec.profile(args.tool).argv(plan)
                        active["session_id"]=plan["session_id"]
                        publish_contract(run_dir,active)
                        if memory_lease is not None:
                            memory_lease.close()
                        if admit_attempt(active, cancellation):
                            adapter_record=provider_exec.execute(plan,result_path,events_path=attempt_dir/"events.jsonl",stderr_path=stderr_path,
                                on_start=provider_started,on_progress=progress,cancelled=cancellation)
                        else:
                            relaunch_skipped = True
                            process_error = process_error or "cancelled"
                            adapter_record = {"status": "failed" if process_error in {"memory_unavailable", "host_disk_full"} else "cancelled",
                                              "exit": 1, "warnings": active["warnings"]}
                if hasattr(args,"resume_relaunch") and not relaunch_skipped:
                    adapter_record["provenance"]["notes"].append("resumed_by_relaunch")
                    adapter_record["warnings"].append("resume: relaunched")
                if admitted and plan["mode"] == "worktree_write":
                    writer_lease.guard_writer_retry(adapter_record, writer_start, plan["cwd"])
                    adapter_record["empty_patch"] = writer_lease.worktree_unchanged(writer_start[0], plan["cwd"])
                args._writer_start = writer_start
                args._last_plan=plan
            else:
                adapter_record=plan
                run_records.write_owned(run_dir,stderr_path,planning.stderr if planning is not None else "")
            if not stderr_path.exists():
                run_records.write_owned(run_dir, stderr_path, "")
            run_records.write_owned(run_dir,adapter_path,json.dumps(adapter_record)+"\n")
            exit_code=adapter_record.get("exit")
            if type(exit_code) is not int: exit_code=1
            observed_exit=True
            if adapter_record.get("status")=="cancelled" or adapter_record.get("evidence",{}).get("signature")=="wall_clock":
                process_error="cancelled" if adapter_record["status"]=="cancelled" else "timeout"
        except (OSError,ValueError,subprocess.SubprocessError) as exc:
            process_error=f"{exc.__cause__}: {exc}" if isinstance(exc.__cause__, OSError) else str(exc)
            process_errno=getattr(exc, "errno", None) or getattr(exc.__cause__, "errno", None)
            for path, content in ((adapter_path, "{}\n"), (stderr_path, str(exc))):
                try:
                    if not path.exists(): run_records.write_owned(run_dir, path, content)
                except (OSError, OwnedFileError):
                    pass
        finally:
            if memory_lease is not None:
                memory_lease.close()
            writer_lease.release_worktree_lease(worktree_lease)
    else:
        try:
            with run_records.owned_text_file(run_dir, adapter_path, "w") as adapter_stream, run_records.owned_text_file(
                run_dir, stderr_path, "w"
            ) as stderr_stream:
                attempt_cancelled = run_records.cancellation_marker_present(run_dir, attempt_dir)
                batch_cancelled = batch_dir is not None and run_records.cancellation_marker_present(run_dir, batch_dir)
                if attempt_cancelled or batch_cancelled:
                    # The owner still writes the ordinary attempt evidence, but no
                    # provider process is created for a pre-launch request.
                    process_error = "cancelled"
                    observed_exit = True
                else:
                    cancel_pending = False

                    def cancel_handler(_signum: int, _frame: Any) -> None:
                        nonlocal cancel_pending
                        # Popen can have spawned the provider before returning.
                        # Keep the handler signal-safe: normal control flow
                        # reconciles the intent and owns process-group cleanup.
                        cancel_pending = True

                    old_handlers = {sig: signal.getsignal(sig) for sig in (signal.SIGTERM, signal.SIGHUP)}
                    signal.signal(signal.SIGTERM, cancel_handler)
                    signal.signal(signal.SIGHUP, cancel_handler)
                    active = contract_row(args, run_dir, attempt_number, attempt_dir, {}, started_at)
                    active["requested_route"] = requested_route
                    if not admit_attempt(active, lambda: cancel_pending or run_records.cancellation_marker_present(run_dir, attempt_dir)
                                         or (batch_dir is not None and run_records.cancellation_marker_present(run_dir, batch_dir))):
                        process_error = process_error or "cancelled"
                        observed_exit = True
                        raise InterruptedError("memory wait cancelled")
                    provider_environment = os.environ.copy()
                    # Providers discover their own seat and checkout; Codex writers share
                    # the configured Fabric store so they can coordinate with the owner.
                    for name in ("AGENT_FABRIC_STATE_DIRECTORY", "AGENT_FABRIC_SEAT",
                                 "AGENT_FABRIC_CLIENT_LABEL", "AGENT_FABRIC_LABEL", "AGENT_FABRIC_PRODUCT_ROOT",
                                 "PROVENANT_FABRIC_PHASES", "PROVENANT_NO_OS_CONFINEMENT",
                                 "PROVENANT_HOST_LOCAL_ONLY", "PROVENANT_REMOTE_LANE", "PROVENANT_PLACEMENT_FALLBACK"):
                        if name == "AGENT_FABRIC_STATE_DIRECTORY" and args.tool == "codex" and args.access_mode == "worktree_write":
                            continue
                        provider_environment.pop(name, None)
                    for name in list(provider_environment):
                        if name.startswith(("PROVENANT_RUN_", "PROVENANT_PREFLIGHT_")):
                            provider_environment.pop(name)
                    provider_environment[run_lineage.PARENT_TOKEN_ENV] = run_lineage.owner_token(run_dir)
                    provider_environment[run_lineage.PARENT_ENV] = json.dumps({
                        "run_id": run_records.run_identity(run_dir, run_receipt), "task_id": args.task_id,
                        "attempt": attempt_number, "run_dir": str(run_dir)})
                    provider_environment.pop("PROVENANT_PARENT_REMOTE", None)
                    run_records.ensure_owned_directory(run_dir, attempt_dir / "tmp" / "children")
                    if os.environ.get("PROVENANT_RUN_TOKEN"):
                        # cf_dispatch buffers stdout/stderr here until completion.
                        # Status can observe mtimes without reading provider output.
                        provider_temporary = tempfile.TemporaryDirectory(prefix="fabric-provider-", ignore_cleanup_errors=True)
                        provider_environment["TMPDIR"] = provider_temporary.name
                        atomic_write_contained(run_dir, (attempt_dir / "provider-output.json").relative_to(run_dir),
                                               (json.dumps({"directory": provider_temporary.name}) + "\n").encode(), label="provider output location")
                    if git_evidence_requested:
                        provider_environment.pop("CF_DISPATCH_AGY_ADD_DIR", None)
                    process = subprocess.Popen(
                        command,
                        cwd=workspace,
                        stdout=adapter_stream,
                        stderr=stderr_stream,
                        env=provider_environment,
                        start_new_session=True,
                    )
                    if memory_lease is not None:
                        memory_lease.started()
                    _record_provider_process(run_dir, process, task_id=args.task_id, attempt=attempt_number)

                    # A request can arrive after the provider is spawned but
                    # before Popen returns. Reconcile it before entering the
                    # normal wait loop, preserving natural exit at the boundary.
                    marker_seen = run_records.cancellation_marker_present(run_dir, attempt_dir)
                    if batch_dir is not None:
                        marker_seen = marker_seen or run_records.cancellation_marker_present(run_dir, batch_dir)
                    if cancel_pending or marker_seen:
                        exit_code = process.poll()
                        if exit_code is None:
                            cancelled = True
                            stop_process_group(process)
                            exit_code = process.wait()
                            process_error = "cancelled"
                            observed_exit = True
                    try:
                        if not observed_exit:
                            deadline = time.monotonic() + args.timeout_seconds
                        while not observed_exit:
                            # Poll first so an already-observed natural exit wins a
                            # marker race.
                            exit_code = process.poll()
                            if exit_code is not None:
                                observed_exit = True
                                break
                            if cancel_pending:
                                stop_process_group(process)
                                exit_code = process.wait()
                                process_error = "cancelled"
                                observed_exit = True
                                break
                            marker_seen = run_records.cancellation_marker_present(run_dir, attempt_dir)
                            if batch_dir is not None:
                                marker_seen = marker_seen or run_records.cancellation_marker_present(run_dir, batch_dir)
                            if marker_seen:
                                # Re-check before stopping to preserve a natural
                                # exit that became observable at the boundary.
                                exit_code = process.poll()
                                if exit_code is not None:
                                    observed_exit = True
                                    break
                                stop_process_group(process)
                                exit_code = process.wait()
                                process_error = "cancelled"
                                observed_exit = True
                                break
                            remaining = deadline - time.monotonic()
                            if remaining <= 0:
                                stop_process_group(process)
                                exit_code = process.wait()
                                process_error = "timeout"
                                observed_exit = True
                                break
                            try:
                                exit_code = process.wait(timeout=min(0.1, remaining))
                                observed_exit = True
                                break
                            except subprocess.TimeoutExpired:
                                continue
                            except KeyboardInterrupt:
                                cancelled = True
                                stop_process_group(process)
                                exit_code = process.wait()
                                observed_exit = True
                                break
                    finally:
                        # Signal ownership deliberately remains with this
                        # dispatch owner through evidence publication below.
                        pass
                    if cancelled:
                        process_error = "cancelled"
                    _retire_provider_process(run_dir, process, task_id=args.task_id, attempt=attempt_number)
        except InterruptedError:
            pass
        except (OSError, ValueError) as exc:
            process_error = f"{exc.__cause__}: {exc}" if isinstance(exc.__cause__, OSError) else str(exc)
            process_errno = getattr(exc, "errno", None) or getattr(exc.__cause__, "errno", None)
        finally:
            if memory_lease is not None:
                memory_lease.close()
            writer_lease.release_worktree_lease(worktree_lease)
            if provider_temporary is not None:
                provider_temporary.cleanup()
    return _DispatchLaunch(
        started_at=started_at,
        started=started,
        observed_exit=observed_exit,
        exit_code=exit_code,
        process_error=process_error,
        process_errno=process_errno,
        process=process,
        active=active,
        old_handlers=old_handlers,
    )

def _settle_dispatch(args: argparse.Namespace, *, plan: _DispatchPlan, launched: _DispatchLaunch) -> _DispatchSettlement:
    adapter_path = plan.adapter_path
    attempt_dir = plan.attempt_dir
    attempt_id = plan.attempt_id
    attempt_number = plan.attempt_number
    command = plan.command
    evidence_path = plan.evidence_path
    exit_code = launched.exit_code
    git_evidence_digest = plan.git_evidence_digest
    git_evidence_identity = plan.git_evidence_identity
    observed_exit = launched.observed_exit
    original_prompt = plan.original_prompt
    process = launched.process
    process_errno = launched.process_errno
    process_error = launched.process_error
    prompt_path = plan.prompt_path
    requested_route = plan.requested_route
    result_path = plan.result_path
    retry_of = plan.retry_of
    run_dir = plan.run_dir
    secret_scan_result = plan.secret_scan_result
    started = launched.started
    started_at = launched.started_at
    stderr_path = plan.stderr_path
    workspace = plan.workspace
    workspace_observation = plan.workspace_observation
    finalize_started = time.monotonic()
    finished_at = now()
    duration_seconds = round(time.monotonic() - started, 6)
    adapter: dict[str, Any] = {}
    adapter_text = adapter_path.read_text(encoding="utf-8", errors="replace") if adapter_path.exists() else ""
    try:
        parsed_lines = [json.loads(line) for line in adapter_text.splitlines() if line.strip()]
        if parsed_lines and isinstance(parsed_lines[-1], dict):
            adapter = parsed_lines[-1]
        else:
            raise ValueError("adapter did not emit a JSON object")
    except (ValueError, json.JSONDecodeError):
        pass

    try:
        result_metadata = result_path.lstat()
    except OSError:
        result_metadata = None
    result_invalid = result_metadata is not None and not run_records.valid_regular_result(run_dir, result_path)
    if result_invalid and result_metadata is not None and not stat.S_ISDIR(result_metadata.st_mode):
        result_path.unlink(missing_ok=True)
    result_exists = run_records.valid_regular_result(run_dir, result_path)
    result_nonempty = result_exists and result_path.stat().st_size > 0
    result_digest = run_records.digest(result_path) if result_exists else ""
    adapter_status = str(adapter.get("status", "")) if adapter else "adapter_receipt_invalid"
    receipt_error = (
        success_receipt_error({**adapter, "status": "ok"}, args, result_path, result_digest)
        if adapter_status in {"ok", "quarantined"} and result_exists
        else None
    )
    question: dict[str, Any] | None = None
    terminal_envelope_error = False
    result_integrity_error = False
    if (observed_exit and (exit_code == 0 and adapter_status == "ok"
                           or adapter_status == "quarantined" and adapter.get("exit") == 0)
            and not receipt_error and result_nonempty):
        try:
            question = run_records.worker_question_envelope(result_path, result_digest)
        except run_records.TerminalEnvelopeIntegrityError:
            result_integrity_error = True
        except ValueError:
            terminal_envelope_error = True
    # A result that is missing, empty or truncated because the deadline expired is
    # a timeout, not a content or receipt defect, so the deadline is checked
    # before any of those classifications. `completed_cleanly` keeps a run that
    # answered inside its budget out of the timeout arm even when it finished at
    # the boundary.
    completed_cleanly = (
        observed_exit and (exit_code == 0 and adapter_status == "ok"
                           or adapter_status == "quarantined" and adapter.get("exit") == 0)
        and result_nonempty and not receipt_error
        and not result_integrity_error and not terminal_envelope_error
    )
    deadline_reached = duration_seconds >= dispatch_preflight.provider_timeout_seconds(args.timeout_seconds)
    status, outcome, question = classify_attempt(
        process_error=process_error,
        process_errno=process_errno,
        process=process,
        observed_exit=observed_exit,
        result_invalid=result_invalid,
        adapter_status=adapter_status,
        deadline_reached=deadline_reached,
        completed_cleanly=completed_cleanly,
        receipt_error=receipt_error,
        result_integrity_error=result_integrity_error,
        terminal_envelope_error=terminal_envelope_error,
        adapter=adapter,
        question=question,
        exit_code=exit_code,
        result_nonempty=result_nonempty,
    )

    try:
        preflight_route = json.loads(os.environ.get("PROVENANT_PREFLIGHT_ROUTES", "{}"))[args.task_id]
        if not isinstance(preflight_route, dict):
            preflight_route = {}
    except (ValueError, KeyError, TypeError):
        preflight_route = {}
    record: dict[str, Any] = {
        "schema_version": 1,
        "record_type": "dispatch-attempt",
        **run_lineage.fields(run_dir),
        "children": run_lineage.children(run_dir, args.task_id, attempt_number),
        **attempt_scope(args, attempt_number),
        "usage_source": adapter.get("usage_source") or context_usage.usage_source(adapter.get("context") or {}),
        "error_class": provider_output.error_class(status, outcome if process_error else (adapter.get("evidence") or {}).get("signature")),
        **({"result_partial": True} if adapter.get("result_partial") else {}),
        **({"landable": False, "quarantine": adapter.get("quarantine")} if adapter_status == "quarantined" else {}),
        "run_id": run_dir.name,
        "task_id": args.task_id,
        "attempt_id": attempt_id,
        "retry_of": retry_of,
        "intent": args.intent,
        "requested_route": requested_route,
        "secret_scan": {"allow_secrets": args.allow_secrets,
                        "finding_names": secret_scan_result.names(),
                        "warnings": secret_scan_result.warnings},
        "route": {
            "adapter": args.tool, "alias": args.alias or "", "model": args.model or "", "effort": args.effort or "",
            **{key: value for key, value in preflight_route.items() if key in {
                "adapter", "alias", "model", "effort", "resolved_model", "provider_family", "model_family", "execution_intent",
            }},
            **adapter,
            "adapter_receipt": {
                "path": run_records.relative_path(run_dir, adapter_path),
                "digest": run_records.digest(adapter_path) if adapter_path.exists() else None,
            },
        },
        "outcome": outcome,
        "failure_code": (
            None if status == "ok" else (question["code"] if question is not None else outcome)
        ),
        "status": status,
        "started_at": started_at,
        "finished_at": finished_at,
        "duration_seconds": duration_seconds,
        "workspace": workspace_observation,
        "prompt": {"path": run_records.relative_path(run_dir, prompt_path), "digest": run_records.digest(prompt_path),
                   "original_path": original_prompt},
        "result": (
            {"path": run_records.relative_path(run_dir, result_path), "digest": result_digest}
            if result_exists
            else None
        ),
        "stderr": {"path": run_records.relative_path(run_dir, stderr_path), "digest": run_records.digest(stderr_path) if stderr_path.exists() else None},
        "process": {
            "pid": process.pid if process is not None else None,
            "started_at": started_at,
            "finished_at": finished_at,
            "exit_code": exit_code,
            "observed_exit": observed_exit,
            "terminating_signal": -exit_code if isinstance(exit_code, int) and exit_code < 0 else None,
        },
        "argv_digest": run_records.json_digest(command),
        "retry_lineage": [retry_of] if retry_of else [],
        **({'delivery_run_id': args.delivery_run_id} if getattr(args, 'delivery_run_id', None) is not None else {}),
    }
    run_lineage.annotate_child_errors(record)
    if evidence_path is not None and git_evidence_identity is not None:
        record["git_evidence"] = {
            "path": run_records.relative_path(run_dir, evidence_path),
            "digest": git_evidence_digest,
            "checkout": git_evidence_identity,
        }
    if question is not None:
        record["question"] = question
    if process_error:
        record["process_error"] = process_error
    checkpoint_written = False
    if record["status"] == "failed" and process is not None and process.poll() is not None:
        try:
            stall_watchdog.checkpoint(attempt_dir, getattr(args, "worktree", None) or workspace, process.pid)
        except (OSError, ValueError) as exc:
            args._bookkeeping_warnings.append("checkpoint unavailable: " + str(exc))
        else:
            checkpoint_written = True
    return _DispatchSettlement(
        finalize_started=finalize_started,
        adapter=adapter,
        status=status,
        record=record,
        checkpoint_written=checkpoint_written,
    )

def _publish_dispatch(args: argparse.Namespace, custody=None, *, plan: _DispatchPlan, launched: _DispatchLaunch, settled: _DispatchSettlement) -> int:
    active = launched.active
    adapter = settled.adapter
    attempt_dir = plan.attempt_dir
    attempt_number = plan.attempt_number
    checkpoint_written = settled.checkpoint_written
    finalize_started = settled.finalize_started
    old_handlers = launched.old_handlers
    record = settled.record
    run_dir = plan.run_dir
    status = settled.status
    attempt_path = attempt_dir / "attempt.json"
    record["attempt_path"] = run_records.relative_path(run_dir, attempt_path)
    digest_path = attempt_dir / "attempt.sha256"
    record["attempt_digest_path"] = run_records.relative_path(run_dir, digest_path)
    run_records.write_owned(run_dir, attempt_path, json.dumps(record, indent=2, sort_keys=True) + "\n")
    attempt_digest = run_records.digest(attempt_path)
    run_records.write_owned(run_dir, digest_path, f"{attempt_digest}  {attempt_path.name}\n")
    manifest_error = False
    try:
        if args.batch_child:
            manifest_error = False
        else:
            run_records.append_manifest(run_dir, record, custody)
    except OSError as exc:
        manifest_error = True
        record["status"] = "failed"
        record["error_class"] = provider_output.error_class(record["status"], (adapter.get("evidence") or {}).get("signature"))
        record["outcome"] = "manifest_write_error"
        record["failure_code"] = "manifest_write_error"
        record["manifest_error"] = str(exc)
        run_records.write_owned(run_dir, attempt_path, json.dumps(record, indent=2, sort_keys=True) + "\n")
        attempt_digest = run_records.digest(attempt_path)
        run_records.write_owned(run_dir, digest_path, f"{attempt_digest}  {attempt_path.name}\n")
    run_records.remove_cancellation_marker(run_dir, attempt_dir)
    for sig, handler in old_handlers.items():
        signal.signal(sig, handler)
    row = terminal_contract(args,run_dir,record,adapter,attempt_number,attempt_dir,active)
    if checkpoint_written:
        row["paths"]["checkpoint"] = run_records.relative_path(run_dir, attempt_dir / "wip-checkpoint.json")
    def prune_tmp():
        left=attempt_storage.prune_private_tmp(run_dir,attempt_dir,row["status"])[1]
        if left: row["warnings"].append("attempt tmp left in place (mount or unremovable): "+", ".join(left[:3]))
    # Close registration under journal custody before removing its private tmp.
    terminal_published = False
    try:
        publish_contract(run_dir, row)
        terminal_published = True
    except (OSError, OwnedFileError) as exc:
        row["warnings"].append("terminal publication unavailable: " + str(exc))
        row["digest"] = render_digest(row)
    if terminal_published: prune_tmp()
    for action in (lambda: write_cooldown(row),lambda: write_route_health(row),lambda: append_index(row,run_dir,root=run_workspace(run_dir, Path.cwd())/".agent-run")):
        try: action()
        except (OSError,ValueError) as exc: row["warnings"].append("terminal index unavailable: "+str(exc))
    # Include the first durable terminal publication; rewrite only its timing.
    row["timing"]["phases"]["finalize"] = round((time.monotonic() - finalize_started) * 1000, 3)
    try:
        publish_contract(run_dir,row)
        if not terminal_published: prune_tmp()
    except (OSError, OwnedFileError) as exc:
        warning = "terminal publication unavailable: " + str(exc)
        if warning not in row["warnings"]: row["warnings"].append(warning)
        row["digest"] = render_digest(row)
    args._last_row=row
    output_record = {**record, "attempt_digest": attempt_digest, "fabric":row, "digest":row["digest"]}
    # Batch children retain the legacy record for the batch evidence validator.
    # The MCP front door consumes the canonical attempt as its terminal row.
    print(json.dumps(row if os.environ.get("PROVENANT_RUN_TOKEN") and not args.batch_child else output_record, sort_keys=True))
    return 0 if status == "ok" and not manifest_error else 1

def _dispatch(args: argparse.Namespace, custody=None) -> int:
    plan = _plan_dispatch(args, custody)
    if isinstance(plan, int):
        return plan
    launched = _launch_dispatch(args, plan=plan)
    settled = _settle_dispatch(args, plan=plan, launched=launched)
    return _publish_dispatch(args, custody, plan=plan, launched=launched, settled=settled)


def close_mcp_run(run_dir: Path) -> None:
    """Close execution custody only; this is not a delivery/assurance final gate."""
    if run_dir.parent.name != "runs" and (not os.environ.get("PROVENANT_RUN_TOKEN") or os.environ.get("PROVENANT_RUN_DIR") != str(run_dir)):
        return
    try:
        attempts = list((run_dir / "dispatch/tasks").glob("*/attempt-*/attempt.json"))
        records = [json.loads(read_bound_bytes(run_dir, path.relative_to(run_dir), label="attempt.json")) for path in attempts]
        # The batch owner calls this after joining all children, under its custody lock.
        receipt = json.loads(read_bound_bytes(run_dir, "RUN_RECEIPT.json", label="RUN_RECEIPT.json"))
        if receipt.get("status") != "active":
            return
        statuses_by_task = {record["task_id"]: canonical_success_status(record["status"]) for record in records}
        for summary_path in (run_dir / "dispatch/batches").glob("*/summary.json"):
            summary = json.loads(read_bound_bytes(run_dir, summary_path.relative_to(run_dir), label="summary.json"))
            if summary.get("status") not in {"completed", "failed", "cancelled"}:
                return
            statuses_by_task.update({task["task_id"]: canonical_success_status(task.get("status", "failed")) for task in summary.get("tasks", [])})
        canonical = [json.loads(read_bound_bytes(run_dir, path.relative_to(run_dir), label="attempt.json"))
                     for path in (run_dir / "tasks").glob("*/attempt-*/attempt.json")]
        if canonical:
            latest={}
            for row in canonical:
                if row["task_id"] not in latest or row["attempt"]>latest[row["task_id"]]["attempt"]: latest[row["task_id"]]=row
            if any(row.get("state") != "terminal" or canonical_success_status(row.get("status")) not in TERMINAL_STATUSES for row in latest.values()):
                return
            receipt["attempts"]=sorted(({**row, "status": canonical_success_status(row["status"])} for row in canonical),
                                       key=lambda row:(row["task_id"],row["attempt"]))
            receipt["run_id"]=canonical[0]["run_id"]
            receipt["resumable"]=any(row.get("status")=="input_required" for row in latest.values())
            statuses_by_task.update({task_id: canonical_success_status(row["status"])
                                     for task_id, row in latest.items()})
        elif any(canonical_success_status(record.get("status")) not in {"ok", "quarantined", "failed", "blocked", "timed_out", "cancelled", "lost"} for record in records):
            return
        statuses = set(statuses_by_task.values())
        if not statuses:
            return
        receipt.update(status="ok" if statuses == {"ok"} else "quarantined" if statuses <= {"ok", "quarantined"} else "cancelled" if statuses == {"cancelled"} else "input_required" if receipt.get("resumable") else "failed",
                       closed_at=now(), terminal_reason=None if statuses == {"ok"} else "MCP execution attempts are terminal")
        run_records.write_owned(run_dir, run_dir / "RUN_RECEIPT.json", json.dumps(receipt, indent=2) + "\n")
        status_path = run_dir / "dispatch-status.json"
        if status_path.exists():
            status = json.loads(read_bound_bytes(run_dir, "dispatch-status.json", label="dispatch status"))
            status.update(status=receipt["status"], finished_at=receipt["closed_at"])
            run_records.write_owned(run_dir, status_path, json.dumps(status, indent=2) + "\n")
    except (OSError, ValueError, TypeError, KeyError, AttributeError, OwnedFileError):
        pass

def rate_limit_delay(previous, cooling, policy):
    """A missing store must still pace; jitter never shortens a provider window."""
    delay = None
    if cooling:
        try:
            delay = max(0, (datetime.fromisoformat(cooling["cooling_until"].replace("Z", "+00:00"))
                            - datetime.now(UTC)).total_seconds())
        except (KeyError, TypeError, ValueError):
            pass
    if delay is None:
        delay = previous.get("retry_after")
        if not isinstance(delay, (int, float)) or not math.isfinite(delay) or delay <= 0:
            delay = policy["base_delay_seconds"]
    return delay + random.uniform(0, delay * policy["jitter_fraction"])

def execute_attempt_sequence(args,custody=None):
    try:
        exec_routing.validate_policy(args.fallback)
    except ValueError as exc:
        return fail(args.run_dir, "fallback_invalid", str(exc))
    sequence_start=time.monotonic()
    sequence_budget=args.timeout_seconds
    explicit_account = getattr(args, "_explicit_account", getattr(args, "account", None))
    args._explicit_account = explicit_account
    result = _dispatch(args, custody)
    previous=getattr(args,"_last_row",None)
    plan=getattr(args,"_last_plan",None)
    if plan and previous and previous["retryable"] and not args.resume:
        alternatives = iter(exec_routing.candidates(plan,args.fallback,confidential=getattr(args,"confidential",False)))
        retried = set()
        attempted = {(plan["adapter"], exec_routing.registered_model(plan["adapter"], plan["model"]))}
        while previous and previous["retryable"]:
            deadline = sequence_start + sequence_budget + getattr(args,"_queued_seconds",0.0)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            current = getattr(args, "_last_plan", plan)
            retry_account = current.get("account")
            if (current["adapter"] == "claude" and explicit_account is None
                    and exec_routing.claude_account_needs_reselection(retry_account)):
                retry_account = exec_routing.reselect_claude_account()
            key = (current["adapter"], exec_routing.registered_model(current["adapter"], current["model"]))
            cooling = exec_routing.cooling(*key, account=retry_account,
                                           explicit_account=current["adapter"] == "claude" and explicit_account is not None)
            policy = pacing_policy()
            # A healthy reselected account owes nothing to the released account's retry window.
            switched = retry_account != current.get("account")
            delay = 0.0 if switched and not cooling else rate_limit_delay(previous, cooling, policy)
            pace = (previous["status"] == "rate_limited" and key not in retried
                    and (previous.get("evidence") or {}).get("signature") != "cooldown_active"
                    and delay + policy["minimum_budget_seconds"] <= remaining)
            if pace:
                args._rate_limit_task_class = previous.get("task_class", "")
                retried.add(key)
                candidate = {"adapter": current["adapter"], "model": current["model"], "effort": current.get("effort")}
                if current["adapter"] == "claude":
                    candidate["account"] = explicit_account if explicit_account is not None else retry_account
                args._rate_limit_retry = {"until": (datetime.now(UTC) + timedelta(seconds=delay)).isoformat(),
                                          "deadline": deadline, "row": previous,
                                          "writer_start": getattr(args, "_writer_start", None)}
            else:
                candidate = None
                for item in alternatives:
                    route = (item["adapter"], exec_routing.registered_model(item["adapter"], item["model"]))
                    if route in attempted:
                        continue
                    # Reuse the selected identity for automatic same-provider fallback.
                    # An explicitly pinned Claude account is checked by its candidate plan.
                    account = retry_account if route[0] == current["adapter"] else None
                    check_cooling = (route[0] != "claude" or
                                     (account is not None and explicit_account is None))
                    if check_cooling and exec_routing.cooling(*route, account=account):
                        continue
                    attempted.add(route)
                    candidate = dict(item)
                    if route[0] == "claude":
                        # Model fallback retains the identity whose cooldown we checked.
                        candidate["account"] = explicit_account if explicit_account is not None else account
                    break
                if candidate is None:
                    break
                vars(args).pop("_rate_limit_task_class", None)
            args.task_class = None
            args.timeout_seconds = remaining
            args.fallback_from={"attempt":previous["attempt"],"status":previous["status"],"reset_at":previous.get("reset_at"),"route":previous["provenance"]["line"].split(" (",1)[0].removeprefix("Route: ")}
            args.tool=candidate["adapter"];args.model=candidate["model"];args.effort=candidate.get("effort")
            if "account" in candidate:
                args.account = candidate["account"]
            args.alias=None;args.retry_of=f"attempt-{previous['attempt']:03d}"
            args.escalation_reason = None  # Automatic retries keep their cause in fallback_from.
            result=_dispatch(args,custody)
            previous=getattr(args,"_last_row",None)
    return result

def cleanup_settled_reader(args) -> None:
    row = getattr(args, "_last_row", None) or {}
    if row.get("mode") == "read_only" and os.environ.get("PROVENANT_REMOTE_LANE") == "1":
        try:
            dispatch_preflight._fabric_hosts_owner().lane_owner().code().cleanup_readers([row])
        except (OSError, ValueError, RuntimeError, subprocess.SubprocessError):
            # A subsequent host read retries operation-owned cleanup.
            pass

def dispatch(args: argparse.Namespace) -> int:
    """Run one attempt while serialising standalone run-ledger mutation."""
    args._explicit_account = getattr(args, "account", None)
    # Publish under custody before re-exec and before peer placement consumes stdin.
    run_dir = args.run_dir.resolve()
    standalone_batch_child = False
    if contains_run(run_dir, Path.cwd()) and (run_dir / "MANIFEST.md").is_file():
        try:
            standalone_batch_child = run_lineage.ensure_nested_owner(run_dir, "dispatch", batch_child=args.batch_child)
        except (OSError, ValueError):
            return fail(run_dir, "run_custody_busy", "another dispatch, batch or finalizer owns the run")
    try:
        dispatch_preflight.export_knob_overrides(args)
    except harness_profile.ProfileError as exc:
        print(json.dumps({'status': 'rejected', 'error': 'invalid_knob', 'fix': str(exc)}))
        return 1
    if os.environ.get('PROVENANT_HOST_LOCAL_ONLY') != '1':
        hosts = dispatch_preflight._fabric_hosts_owner()
        try:
            owner = hosts.lane_owner()
        except hosts.HostError as exc:
            print(json.dumps({'status': 'rejected', 'error': exc.code, 'fix': str(exc)}))
            return 1
        if owner.config.peers or owner.config_error or getattr(args, 'host', None) or any('@' in str(value) for value in (args.resume or '', args.task_id or '')):
            request = {key: value for key, value in {
                'host': getattr(args, 'host', None), 'operation_id': getattr(args, 'operation_id', None),
                'adapter': args.tool, 'model': args.model, 'alias': args.alias, 'task_class': args.task_class, 'effort': args.effort,
                'intent': args.intent, 'orchestrator_family': args.orchestrator_family,
                'role': args.role, 'risk_tier': args.risk_tier, 'model_override_tier': args.model_override_tier,
                'reviewer_id': args.reviewer_id, 'preface': args.preface, 'read_roots': args.read_roots,
                'fallback': exec_routing.validate_policy(args.fallback), 'pick_reason': args.pick_reason,
                'mode': args.access_mode, 'cwd': str(args.provider_cwd) if args.provider_cwd else None,
                'worktree': str(args.worktree) if args.worktree else None,
                'task_id': args.task_id, 'timeout_seconds': args.timeout_seconds,
                'resume': args.resume, 'context_ceiling': args.context_ceiling,
                'sandbox': args.sandbox, 'network': None if args.network is None else args.network == 'true',
                'account': args.account, 'capabilities': args.capabilities, 'add_dirs': args.add_dirs,
                'allow_secrets': args.allow_secrets, 'confidential': args.confidential,
                'delivery_run_id': getattr(args, 'delivery_run_id', None),
                'knobs': getattr(args, 'knobs', None) or None, 'wait_seconds': 0,
                '_lineage': run_lineage.parent_attempt(),
            }.items() if value is not None and value != []}
            if args.prompt_stdin:
                request['prompt'] = sys.stdin.buffer.read().decode('utf-8')
            else:
                request['prompt_file'] = str(args.prompt_file)
            if args.resume:
                request = {key: value for key, value in request.items() if key in {
                    'host', 'operation_id', 'resume', 'task_id', 'prompt', 'prompt_file',
                    'context_ceiling', 'timeout_seconds', 'allow_secrets', 'wait_seconds', 'knobs'}}
                if args.resume_attempt is not None:
                    request['resume_attempt'] = args.resume_attempt
                if args.require_session:
                    request['require_session'] = args.require_session
            if args.git_evidence:
                request['git_evidence'] = str(args.git_evidence)
            placement_custody = None
            try:
                if run_lineage.parent_attempt() and (run_dir / "MANIFEST.md").is_file() and not args.batch_child:
                    placement_custody = acquire_run_custody(run_dir)
                    run_lineage.record_owner(run_dir)
                result = owner.client_call('resume' if args.resume else 'dispatch', str(Path.cwd()), request)
            except hosts.HostError as exc:
                result = {'status': 'rejected', 'error': exc.code, 'fix': str(exc)}
            except (OSError, OwnedFileError) as exc:
                result = {'status': 'rejected', 'error': 'run_custody_busy', 'fix': str(exc)}
            finally:
                if placement_custody is not None:
                    placement_custody.close()
            if not result.get('local'):
                run_lineage.register_external(result)
                print(json.dumps(result))
                return 1 if result.get('status') == 'rejected' else 0
            if result.get('fallback'):
                os.environ['PROVENANT_PLACEMENT_FALLBACK'] = json.dumps(result['fallback'])
                print(result['digest'], file=sys.stderr)
            if result.get('input'):
                args.resume = result['input'].get('resume', args.resume)
                args.task_id = result['input'].get('task_id', args.task_id)
            if args.prompt_stdin:
                # The local owner still consumes the same original stdin bytes.
                import io
                sys.stdin = io.TextIOWrapper(io.BytesIO(request['prompt'].encode('utf-8')))
    # Canonical before first use, so the saved roots are what resume re-checks strictly.
    args.read_roots = [str(Path(root).expanduser().resolve()) for root in getattr(args, "read_roots", None) or []]
    if args.timeout_seconds is None:
        args.timeout_seconds = 10800.0 if args.access_mode == "worktree_write" else DEFAULT_TIMEOUT_SECONDS
    run_dir = args.run_dir.resolve()
    workspace = Path.cwd().resolve()
    owned = contains_run(run_dir, workspace) and run_dir.is_dir() and (run_dir / "MANIFEST.md").is_file()
    if args.task_id is None and not (args.resume and owned and not args.batch_child):
        args.task_id = "dispatch-001"  # A resume names its task, or takes the run's only one.
    if not owned:
        result = _dispatch(args)
        cleanup_settled_reader(args)
        return result
    if args.batch_child:
        # Real batch children share their launching owner's custody. A direct
        # standalone child must hold its own lock for the complete attempt.
        try:
            custody = acquire_run_custody(run_dir) if standalone_batch_child else None
        except (OSError, OwnedFileError):
            return fail(run_dir, "run_custody_busy", "another dispatch, batch or finalizer owns the run")
        try:
            if custody is not None:
                run_lineage.record_owner(run_dir)
            result = execute_attempt_sequence(args, custody)
            cleanup_settled_reader(args)
            return result
        finally:
            if custody is not None:
                custody.close()
    try:
        custody = acquire_run_custody(run_dir)
    except (OSError, OwnedFileError):
        return fail(run_dir, "run_custody_busy", "another dispatch, batch or finalizer owns the run")
    try:
        try:
            run_lineage.record_owner(run_dir)
            run_lineage.register_run(run_dir, run_records.run_identity(run_dir))
        except (OSError, ValueError, OwnedFileError) as exc:
            return fail(run_dir, "lineage_unavailable", str(exc))
        if args.resume:
            try: prepare_resume(args)
            except (OSError,ValueError) as exc: return fail(run_dir,"rejected",str(exc),getattr(exc,"code",None))
        if not args.tool or not any((args.alias,args.task_class,args.model)):
            return fail(run_dir,"rejected","adapter and alias or model are required")
        result = execute_attempt_sequence(args,custody)
        close_mcp_run(run_dir)
        cleanup_settled_reader(args)
        return result
    finally:
        fcntl.flock(custody.fileno(), fcntl.LOCK_UN)
        custody.close()


def parser() -> argparse.ArgumentParser:
    root = BudgetArgumentParser(command_name="dispatch", description=__doc__)
    root.add_argument("--run-dir", type=Path, required=True)
    root.add_argument("--task-id")
    root.add_argument("--delivery-run-id", type=run_records.delivery_run_id, help="RUN.json run_id of the delivery this lane serves")
    root.add_argument("--host")
    root.add_argument("--operation-id")
    adapter = root.add_mutually_exclusive_group(required=False)
    adapter.add_argument("--adapter", "--tool", dest="tool")
    prompt = root.add_mutually_exclusive_group(required=True)
    prompt.add_argument("--prompt-file", type=Path)
    prompt.add_argument("--prompt-stdin", action="store_true", help="read the prompt bytes from stdin")
    root.add_argument("--intent", choices=("ordinary", "assurance"), default="ordinary")
    root.add_argument("--orchestrator-family")
    selector = root.add_mutually_exclusive_group(required=False)
    selector.add_argument("--alias")
    selector.add_argument("--task-class")
    selector.add_argument("--model")
    root.add_argument("--role", default="worker")
    root.add_argument("--risk-tier")
    root.add_argument(
        "--model-override-tier",
        choices=("routine", "substantial", "crucial", "terminal"),
        help="explicit special-model selection; independent of lifecycle risk metadata",
    )
    root.add_argument("--reviewer-id")
    root.add_argument(
        "--access-mode", choices=dispatch_preflight.ACCESS_MODES, default="read_only",
        help="read_only (default) or worktree_write for a worker that owns a worktree",
    )
    root.add_argument(
        "--worktree", type=Path,
        help="Git worktree root the writer owns exclusively; requires worktree_write",
    )
    root.add_argument("--effort")
    root.add_argument("--account", help="Claude account label from telemetry configuration")
    root.add_argument(
        "--timeout", "--timeout-seconds", dest="timeout_seconds", type=timeout_value,
        default=None,
        help="maximum provider runtime: read_only 3600 seconds, worktree_write 10800",
    )
    root.add_argument("--retry-of", help="existing attempt id under this task, for lineage only")
    root.add_argument("--escalation-reason", choices=ESCALATION_REASONS, help="reason for escalating retry-of")
    root.add_argument(
        "--git-evidence", type=Path,
        help="run-owned Git evidence packet to copy into an Agy attempt",
    )
    root.add_argument("--batch-child", action="store_true", help=argparse.SUPPRESS)
    root.add_argument("--batch-id", help=argparse.SUPPRESS)
    root.add_argument("--cwd",dest="provider_cwd",type=Path)
    root.add_argument("--resume", help="resume this run id; inherit route and controls")
    root.add_argument("--resume-attempt", type=int, help="resume from this attempt instead of the latest")
    root.add_argument("--require-session", help="continue exactly this provider session; never relaunch a new one")
    root.add_argument("--context-ceiling", type=float, help="auto-compaction threshold in tokens, clamped to 100k-1M")
    root.add_argument("--knob", dest="knobs", action="append", default=[], metavar="NAME=VALUE",
                      help="harness-profile override for this dispatch; repeatable (provenant profile list)")
    root.add_argument("--sandbox", choices=("read-only","workspace-write","full"))
    root.add_argument("--network", choices=("true","false"))
    root.add_argument("--capabilities", type=provider_exec.parse_capabilities_argument)
    root.add_argument("--add-dir", dest="add_dirs", action="append", default=[])
    root.add_argument("--read-root", dest="read_roots", action="append", default=[],
                      help="outside directory a read-only cwd or prompt file may use; repeatable")
    root.add_argument("--allow-secrets", action="store_true")
    root.add_argument("--no-preface", dest="preface", action="store_false")
    root.add_argument("--fallback", default=None, help="false, true, any, or JSON route list")
    root.add_argument("--confidential", action="store_true",
                      help="never route this task, or any fallback, to a prompt-training model; "
                           "a free model counts unless its entry sets trains_on_prompts false")
    root.add_argument("--pick-reason", help="why a route pool picked this model; appended to the Route line")
    return root

if __name__ == "__main__":
    if sys.argv[1:] == ["--preflight-json"]:
        print(json.dumps(dispatch_preflight.preflight_tasks(json.load(sys.stdin)["tasks"])))
    elif sys.argv[1:] == ["--reap-attempts"]:
        print(json.dumps([str(run) for run in json.load(sys.stdin) if reap_attempts(Path(run))]))
    else:
        raise SystemExit(dispatch(parser().parse_args()))
