#!/usr/bin/env python3
"""Create and remove shared, project-local Git worktrees safely."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import subprocess
import sys
from typing import Sequence


SAFE_NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")
COMMIT_SHA = re.compile(r"^[0-9a-fA-F]{40}(?:[0-9a-fA-F]{24})?$")
IGNORE_RULE = "/.worktrees/"
MAX_DIAGNOSTIC_BYTES = 8192
AUTHENTICATED_URL = re.compile(r"(?i)(https?://)([^/\s:@]*(?::[^@\s/]*)?@)")
PARTIAL_CREDENTIAL_URL = re.compile(r"(?i)(https?://)([^/\s:@]*:[^/\s@]*)(?=[/\s]|$)")
PARTIAL_AUTHENTICATED_URL = re.compile(r"(?i)(https?://)[^/\s]*$")
PORCELAIN_FLAG_FIELDS = {"bare", "detached"}
PORCELAIN_REQUIRED_VALUE_FIELDS = {"worktree", "HEAD", "branch"}
PORCELAIN_OPTIONAL_VALUE_FIELDS = {"locked", "prunable"}
ALLOWED_GENERATED_IGNORED_PREFIXES = (
    ".agent-fabric/",
    ".agent-run/",
    ".pytest_cache/",
    ".review-snapshots/",
    ".venv/",
    "node_modules/",
)
TRUSTED_TOOL_DIRECTORIES = (
    "/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin",
    "/usr/local/sbin", "/opt/local/bin", "/usr/bin", "/bin",
    "/usr/sbin", "/sbin",
)
SENSITIVE_ENVIRONMENT_KEYS = (
    "npm_config_registry",
    "npm_config_proxy",
    "npm_config_http_proxy",
    "npm_config_https_proxy",
    "NPM_CONFIG_REGISTRY",
    "NPM_CONFIG_PROXY",
    "NPM_CONFIG_HTTP_PROXY",
    "NPM_CONFIG_HTTPS_PROXY",
)


class PolicyError(RuntimeError):
    """A requested operation violates the shared-worktree contract."""


def git(repo: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    result, _ = _run_git(repo, *args)
    if check and result.returncode != 0:
        raise PolicyError(result.stderr.strip() or f"git {' '.join(args)} failed")
    return result


def trusted_tool_path() -> str:
    return os.pathsep.join(TRUSTED_TOOL_DIRECTORIES)


def git_environment() -> dict[str, str]:
    return {
        "PATH": trusted_tool_path(),
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_TERMINAL_PROMPT": "0",
    }


def _drain_process(
    process: subprocess.Popen[bytes],
    *,
    stdout_limit: int = MAX_DIAGNOSTIC_BYTES,
    stderr_limit: int = MAX_DIAGNOSTIC_BYTES,
) -> dict[str, dict[str, object]]:
    selector = selectors.DefaultSelector()
    captures: dict[str, dict[str, object]] = {}
    for label, stream, limit in (
        ("stdout", process.stdout, stdout_limit),
        ("stderr", process.stderr, stderr_limit),
    ):
        assert stream is not None
        captures[label] = {
            "buffer": bytearray(),
            "size": 0,
            "digest": hashlib.sha256(),
            "limit": limit,
        }
        selector.register(stream, selectors.EVENT_READ, label)
    while selector.get_map():
        for key, _ in selector.select():
            chunk = os.read(key.fd, 8192)
            if not chunk:
                selector.unregister(key.fileobj)
                continue
            capture = captures[key.data]
            capture["digest"].update(chunk)
            capture["size"] += len(chunk)
            limit = capture["limit"]
            if limit is None:
                capture["buffer"].extend(chunk)
            else:
                capture["buffer"].extend(chunk[:max(0, limit - len(capture["buffer"]))])
    selector.close()
    process.wait()
    return captures


def diagnostic_redaction_values() -> list[str]:
    return sorted(
        {
            value
            for key in SENSITIVE_ENVIRONMENT_KEYS
            if (value := os.environ.get(key))
        },
        key=len,
        reverse=True,
    )


def redact_diagnostic(value: object) -> str:
    message = str(value)
    secrets = diagnostic_redaction_values()
    if secrets:
        pattern = re.compile("|".join(re.escape(secret) for secret in secrets))
        message = pattern.sub("[REDACTED]", message)
    message = AUTHENTICATED_URL.sub(r"\1[REDACTED]@", message)
    return PARTIAL_CREDENTIAL_URL.sub(r"\1[REDACTED]", message)


def bounded_diagnostic(value: object) -> str:
    message = redact_diagnostic(value)
    encoded = message.encode("utf-8", errors="replace")
    marker = b"\n[truncated]"
    if len(encoded) <= MAX_DIAGNOSTIC_BYTES:
        return message
    prefix_size = max(0, MAX_DIAGNOSTIC_BYTES - len(marker))
    prefix = encoded[:prefix_size].decode("utf-8", errors="ignore")
    return prefix + marker.decode()


def _capture_text(capture: dict[str, object], *, bounded: bool = True) -> str:
    value = bytes(capture["buffer"])
    raw_truncated = capture["size"] > len(value)
    capture_limit = capture.get("limit")
    at_capture_boundary = isinstance(capture_limit, int) and len(value) >= capture_limit
    if at_capture_boundary:
        redaction_size = 0
        for secret in diagnostic_redaction_values():
            encoded_secret = secret.encode(errors="surrogateescape")
            for size in range(min(len(encoded_secret), len(value)), 0, -1):
                if value.endswith(encoded_secret[:size]):
                    redaction_size = max(redaction_size, size)
                    break
        if redaction_size:
            value = value[:-redaction_size] + b"[REDACTED]"
    message = "".join(
        "\ufffd" if 0xDC80 <= ord(character) <= 0xDCFF else character
        for character in redact_diagnostic(value.decode(errors="surrogateescape"))
    )
    if at_capture_boundary:
        message = PARTIAL_AUTHENTICATED_URL.sub(r"\1[REDACTED]", message)
    if raw_truncated or len(message.encode("utf-8", errors="replace")) > MAX_DIAGNOSTIC_BYTES:
        marker = (
            f"\n[truncated; bytes={capture['size']}; "
            f"sha256={capture['digest'].hexdigest()}]"
        )
        if bounded:
            marker_size = len(marker.encode("utf-8"))
            prefix_size = max(0, MAX_DIAGNOSTIC_BYTES - marker_size)
            prefix = message.encode("utf-8", errors="replace")[:prefix_size]
            return prefix.decode("utf-8", errors="ignore") + marker
        message += marker
    return bounded_diagnostic(message) if bounded else message


def _run_git(
    repo: Path,
    *args: str,
    stdout_limit: int | None = MAX_DIAGNOSTIC_BYTES,
    stderr_limit: int | None = MAX_DIAGNOSTIC_BYTES,
) -> tuple[subprocess.CompletedProcess[str], dict[str, dict[str, object]]]:
    command = ["git", "-C", str(repo), *args]
    try:
        process = subprocess.Popen(
            command,
            env=git_environment(),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    except OSError as exc:
        raise PolicyError(f"could not launch git: {exc}") from exc
    captures = _drain_process(
        process,
        stdout_limit=stdout_limit,
        stderr_limit=stderr_limit,
    )
    return subprocess.CompletedProcess(
        command,
        process.returncode,
        _capture_text(captures["stdout"]),
        _capture_text(captures["stderr"]),
    ), captures


def owning_root(repo: Path) -> Path:
    result = git(repo.expanduser().resolve(), "rev-parse", "--show-toplevel")
    return Path(result.stdout.strip()).resolve()


def worktree_records(repo: Path) -> list[dict[str, object]]:
    result, captures = _run_git(
        repo,
        "worktree",
        "list",
        "--porcelain",
        "-z",
        stdout_limit=None,
    )
    if result.returncode != 0:
        raise PolicyError(
            _capture_text(captures["stderr"]).strip() or "git worktree list failed"
        )
    raw = bytes(captures["stdout"]["buffer"])
    if not raw.endswith(b"\0\0"):
        raise PolicyError("git worktree list output was incomplete")

    def malformed(reason: str) -> None:
        raise PolicyError(f"git worktree list output was malformed: {reason}")

    records: list[dict[str, object]] = []
    current: dict[str, object] = {}
    known_fields = (
        PORCELAIN_FLAG_FIELDS
        | PORCELAIN_REQUIRED_VALUE_FIELDS
        | PORCELAIN_OPTIONAL_VALUE_FIELDS
    )

    def append_record() -> None:
        nonlocal current
        if not current:
            malformed("empty record")
        if "worktree" not in current:
            malformed("record has no worktree path")
        if current.get("bare") is True:
            if any(field in current for field in ("HEAD", "branch", "detached")):
                malformed("bare record has checkout state")
        elif "HEAD" not in current:
            malformed("record has no HEAD")
        elif ("branch" in current) == ("detached" in current):
            malformed("record needs exactly one branch or detached state")
        records.append(current)
        current = {}

    for field in raw[:-2].split(b"\0"):
        if not field:
            append_record()
            continue
        key, separator, value = field.partition(b" ")
        name = key.decode(errors="replace")
        if name not in known_fields:
            malformed(f"unknown field {name!r}")
        if name in current:
            malformed(f"duplicate field {name!r}")
        if name in PORCELAIN_FLAG_FIELDS and separator:
            malformed(f"flag field {name!r} has a value")
        if name in PORCELAIN_REQUIRED_VALUE_FIELDS and (not separator or not value):
            malformed(f"value field {name!r} has no value")
        current[name] = value.decode(errors="surrogateescape") if value else True
    if current:
        append_record()
    if not records:
        malformed("no worktree records")
    return records


def primary_root(repo: Path) -> Path:
    root = owning_root(repo)
    records = worktree_records(root)
    if not records or records[0].get("bare") is True or not records[0].get("worktree"):
        raise PolicyError("repository has no primary checkout root for project-local worktrees")
    return Path(str(records[0]["worktree"])).resolve()


def validate_name(name: str) -> None:
    if not SAFE_NAME.fullmatch(name) or name in {".", ".."}:
        raise PolicyError("worktree name must be 1-64 safe filename characters without slashes")


def common_git_dir(root: Path) -> Path:
    value = git(root, "rev-parse", "--git-common-dir").stdout.strip()
    path = Path(value)
    return (root / path).resolve() if not path.is_absolute() else path.resolve()


def ignored_path_is_generated(path: str) -> bool:
    normalized = path.rstrip("/")
    return (
        any(
            normalized == prefix.rstrip("/")
            or f"/{prefix}" in f"/{normalized}/"
            for prefix in ALLOWED_GENERATED_IGNORED_PREFIXES
        )
        or "/__pycache__/" in f"/{normalized}/"
        or normalized.endswith((".pyc", ".pyo", ".pyd"))
    )


def worktree_residue(root: Path) -> list[str]:
    status = git(
        root, "status", "--porcelain=v1", "--untracked-files=all", "--ignored=matching",
    )
    residue: list[str] = []
    for line in status.stdout.splitlines():
        code = line[:2]
        path = line[3:]
        if code == "!!" and ignored_path_is_generated(path):
            continue
        residue.append(line)
    return residue


def verify_claim(
    expected_worktree: Path,
    claimed_worktree: Path,
    claimed_commit: str | None,
    expected_common: Path | None = None,
    *,
    base_revision: str | None = None,
) -> dict[str, object]:
    """Verify one chair-bound, base-descended clean linked-worktree claim."""
    if not claimed_commit:
        return {"status": "rejected", "reason": "missing claimed commit SHA"}
    if not COMMIT_SHA.fullmatch(claimed_commit):
        return {"status": "rejected", "reason": "claimed commit SHA must be a full Git object ID"}
    if not base_revision:
        return {"status": "rejected", "reason": "missing pre-dispatch base revision"}
    if not COMMIT_SHA.fullmatch(base_revision):
        return {"status": "rejected", "reason": "base revision must be a full Git object ID"}

    expected_path = expected_worktree.expanduser().resolve()
    claimed_path = claimed_worktree.expanduser().resolve()
    if expected_worktree.is_symlink() or claimed_worktree.is_symlink():
        return {"status": "rejected", "reason": "worktree context must not be a symlink"}

    expected_common_path = common_git_dir(expected_path)
    if expected_common is not None and expected_common.expanduser().resolve() != expected_common_path:
        return {"status": "rejected", "reason": "expected common Git directory does not match worktree"}
    claimed_common_path = common_git_dir(claimed_path)
    if claimed_common_path != expected_common_path:
        return {"status": "rejected", "reason": "claimed worktree is from a different common Git directory"}

    project_root = primary_root(expected_path)
    canonical_shared = project_root / ".worktrees"
    if (
        canonical_shared.is_symlink()
        or not canonical_shared.is_dir()
        or expected_path.parent != canonical_shared
        or not SAFE_NAME.fullmatch(expected_path.name)
    ):
        return {"status": "rejected", "reason": "expected worktree is outside canonical .worktrees"}

    records = worktree_records(expected_path)
    expected_record = next(
        (
            item for item in records
            if item.get("worktree") and Path(str(item["worktree"])).resolve() == expected_path
        ),
        None,
    )
    primary_path = Path(str(records[0]["worktree"])).resolve() if records and records[0].get("worktree") else None
    if expected_record is None or expected_path == primary_path:
        return {"status": "rejected", "reason": "expected worktree is not a registered linked worktree"}
    if claimed_path != expected_path:
        return {"status": "rejected", "reason": "claimed worktree context does not match expected worktree"}

    head_before = git(expected_path, "rev-parse", "--verify", "HEAD", check=False).stdout.strip()
    if not COMMIT_SHA.fullmatch(head_before):
        return {"status": "rejected", "reason": "expected worktree HEAD cannot be resolved"}
    resolved = git(
        expected_path, "rev-parse", "--verify", f"{claimed_commit}^{{commit}}", check=False,
    )
    resolved_commit = resolved.stdout.strip()
    if resolved.returncode != 0 or not COMMIT_SHA.fullmatch(resolved_commit):
        return {"status": "rejected", "reason": "claimed commit does not resolve in expected common Git directory"}
    base = git(
        expected_path, "rev-parse", "--verify", f"{base_revision}^{{commit}}", check=False,
    )
    resolved_base = base.stdout.strip()
    if base.returncode != 0 or not COMMIT_SHA.fullmatch(resolved_base):
        return {"status": "rejected", "reason": "base revision does not resolve in expected common Git directory"}
    if resolved_base.lower() != base_revision.lower():
        return {"status": "rejected", "reason": "base revision must be an exact object ID"}
    if resolved_commit.lower() == resolved_base.lower():
        return {"status": "rejected", "reason": "claimed commit is unchanged from pre-dispatch base"}
    ancestry = git(
        expected_path, "merge-base", "--is-ancestor", resolved_base, resolved_commit, check=False,
    )
    if ancestry.returncode != 0:
        return {"status": "rejected", "reason": "claimed commit is not descended from pre-dispatch base"}

    residue_before = worktree_residue(expected_path)
    if residue_before:
        return {
            "status": "rejected",
            "reason": "worktree has implementation residue not captured by claimed commit: "
            + "; ".join(residue_before),
        }
    residue_after = worktree_residue(expected_path)
    if residue_after:
        return {
            "status": "rejected",
            "reason": "worktree gained implementation residue during claim verification: "
            + "; ".join(residue_after),
        }
    head_after = git(expected_path, "rev-parse", "--verify", "HEAD", check=False).stdout.strip()
    if head_before.lower() != head_after.lower():
        return {"status": "rejected", "reason": "worktree HEAD advanced during claim verification"}
    residue_final = worktree_residue(expected_path)
    if residue_final:
        return {
            "status": "rejected",
            "reason": "worktree gained implementation residue during claim verification: "
            + "; ".join(residue_final),
        }
    if head_after.lower() != resolved_commit.lower():
        return {"status": "rejected", "reason": "claimed commit does not match current worktree HEAD"}
    if resolved_commit.lower() != claimed_commit.lower():
        return {"status": "rejected", "reason": "claimed commit SHA is not an exact object ID"}
    if str(expected_record.get("HEAD", "")).lower() != resolved_commit.lower():
        return {"status": "rejected", "reason": "claimed commit does not match claimed worktree context"}

    return {
        "status": "accepted",
        "base_revision": resolved_base,
        "head_revision": resolved_commit,
        "clean": True,
        "claimed_commit": resolved_commit,
        "claimed_worktree": str(claimed_path),
        "common_git_dir": str(expected_common_path),
        "acceptance_owner": "chair-orchestrator",
        "acceptance_mode": "manual",
    }


def ensure_shared_root(root: Path) -> Path:
    shared = root / ".worktrees"
    if shared.is_symlink():
        raise PolicyError(".worktrees must be a real directory, not a symlink")
    tracked = git(root, "ls-files", "--", ".worktrees").stdout.strip()
    if tracked:
        raise PolicyError(".worktrees contains tracked paths; refusing to hide them")
    shared.mkdir(mode=0o755, exist_ok=True)
    if not shared.is_dir():
        raise PolicyError(".worktrees is not a directory")

    exclude = common_git_dir(root) / "info" / "exclude"
    exclude.parent.mkdir(parents=True, exist_ok=True)
    existing = exclude.read_text(errors="replace").splitlines() if exclude.exists() else []
    if IGNORE_RULE not in existing:
        with exclude.open("a") as handle:
            if exclude.stat().st_size:
                handle.write("\n")
            handle.write(IGNORE_RULE + "\n")
    probe = git(root, "check-ignore", "--no-index", ".worktrees/.probe", check=False)
    if probe.returncode != 0:
        raise PolicyError("failed to protect .worktrees with a repository-local ignore rule")
    return shared


def create(args: argparse.Namespace) -> dict[str, object]:
    if not args.human_authorised:
        raise PolicyError("creating a worktree requires explicit human authorisation")
    validate_name(args.name)
    root = primary_root(args.repo)
    shared = ensure_shared_root(root)
    target = shared / args.name
    if target.exists() or target.is_symlink():
        raise PolicyError(f"worktree target already exists: {target}")

    command = ["worktree", "add"]
    if args.detach is not None:
        command.extend(["--detach", str(target), args.detach])
    elif args.existing_branch is not None:
        command.extend([str(target), args.existing_branch])
    else:
        if not args.branch_authorised:
            raise PolicyError("creating a branch requires separate explicit human authorisation")
        command.extend(["-b", args.new_branch, str(target), args.start_point])
    git(root, *command)
    head_revision = git(target, "rev-parse", "HEAD").stdout.strip()
    branch_result = git(target, "symbolic-ref", "--quiet", "--short", "HEAD", check=False)
    if branch_result.returncode not in {0, 1}:
        raise PolicyError(branch_result.stderr.strip() or "cannot determine new worktree branch identity")
    branch = branch_result.stdout.strip() if branch_result.returncode == 0 else None
    return {
        "status": "created",
        "name": args.name,
        "primary_root": str(root),
        "worktree_root": str(target),
        "common_git_dir": str(common_git_dir(root)),
        "head_revision": head_revision,
        "branch": branch,
        "detached": branch is None,
    }


def remove(args: argparse.Namespace) -> dict[str, object]:
    if not args.human_authorised:
        raise PolicyError("removing a worktree requires explicit human authorisation")
    validate_name(args.name)
    root = primary_root(args.repo)
    shared = root / ".worktrees"
    if shared.is_symlink():
        raise PolicyError(".worktrees must be a real directory, not a symlink")
    target = shared / args.name
    registered = {
        Path(str(item["worktree"])).resolve()
        for item in worktree_records(root)
        if item.get("worktree")
    }
    if target.resolve() not in registered:
        raise PolicyError(f"not a registered project worktree: {target}")
    dirty = git(target, "status", "--porcelain=v1", "--untracked-files=all").stdout
    if dirty:
        raise PolicyError("worktree is dirty; preserve or hand off its changes before removal")
    git(root, "worktree", "remove", str(target))
    return {"status": "removed", "name": args.name, "primary_root": str(root)}


def list_worktrees(args: argparse.Namespace) -> dict[str, object]:
    root = primary_root(args.repo)
    return {"primary_root": str(root), "worktrees": worktree_records(root)}


def check_worktrees(args: argparse.Namespace) -> dict[str, object]:
    root = primary_root(args.repo)
    shared = root / ".worktrees"
    findings: list[str] = []
    if shared.is_symlink() or not shared.is_dir():
        findings.append("canonical .worktrees must be a real directory")
    if git(root, "ls-files", "--", ".worktrees").stdout.strip():
        findings.append("canonical .worktrees contains tracked paths")
    ignored = git(root, "check-ignore", "--no-index", ".worktrees/.probe", check=False)
    if ignored.returncode != 0:
        findings.append("canonical .worktrees is not protected by a repository-local ignore rule")

    for item in worktree_records(root):
        value = item.get("worktree")
        if value is None:
            findings.append("registered worktree record has no path")
            continue
        path = Path(str(value))
        resolved = path.resolve()
        if resolved == root:
            continue
        if path.is_symlink() or resolved.parent != shared.resolve() or not SAFE_NAME.fullmatch(resolved.name):
            findings.append(f"registered worktree is outside canonical .worktrees: {path}")
            continue
        if not resolved.is_dir():
            findings.append(f"registered worktree path is missing: {path}")

    return {
        "status": "pass" if not findings else "fail",
        "primary_root": str(root),
        "findings": sorted(findings),
    }


def verify_claim_command(args: argparse.Namespace) -> dict[str, object]:
    try:
        return verify_claim(
            args.expected_worktree,
            args.claimed_worktree,
            args.claimed_commit,
            args.expected_common,
            base_revision=args.base_revision,
        )
    except PolicyError as exc:
        return {"status": "rejected", "reason": str(exc)}


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    sub = result.add_subparsers(dest="command", required=True)

    create_parser = sub.add_parser("create")
    create_parser.add_argument("name")
    create_parser.add_argument("--repo", type=Path, default=Path.cwd())
    create_parser.add_argument("--human-authorised", action="store_true")
    create_parser.add_argument("--branch-authorised", action="store_true")
    mode = create_parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--detach", metavar="REV")
    mode.add_argument("--existing-branch", metavar="BRANCH")
    mode.add_argument("--new-branch", metavar="BRANCH")
    create_parser.add_argument("--start-point", default="HEAD")
    create_parser.set_defaults(handler=create)

    list_parser = sub.add_parser("list")
    list_parser.add_argument("--repo", type=Path, default=Path.cwd())
    list_parser.set_defaults(handler=list_worktrees)

    check_parser = sub.add_parser("check")
    check_parser.add_argument("--repo", type=Path, default=Path.cwd())
    check_parser.set_defaults(handler=check_worktrees)

    verify_parser = sub.add_parser(
        "verify-claim",
        help="verify a claimed commit and linked worktree at the chair acceptance boundary",
    )
    verify_parser.add_argument("--expected-worktree", type=Path, required=True)
    verify_parser.add_argument("--claimed-worktree", type=Path, required=True)
    verify_parser.add_argument("--claimed-commit")
    verify_parser.add_argument("--base-revision")
    verify_parser.add_argument("--expected-common", type=Path)
    verify_parser.set_defaults(handler=verify_claim_command)

    remove_parser = sub.add_parser("remove")
    remove_parser.add_argument("name")
    remove_parser.add_argument("--repo", type=Path, default=Path.cwd())
    remove_parser.add_argument("--human-authorised", action="store_true")
    remove_parser.set_defaults(handler=remove)
    return result


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        receipt = args.handler(args)
    except PolicyError as exc:
        print(f"worktree policy: {exc}", file=sys.stderr)
        return 2
    print(json.dumps(receipt, indent=2, sort_keys=True))
    return 2 if receipt.get("status") in {"fail", "rejected"} else 0


if __name__ == "__main__":
    raise SystemExit(main())
