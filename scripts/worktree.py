#!/usr/bin/env python3
"""Create and remove shared, project-local Git worktrees safely."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import errno
import fcntl
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
IGNORE_RULE = "/.worktrees/"
MAX_DIAGNOSTIC_BYTES = 8192
ADMISSION_DIRECTORY = "worktree-admission"
AUTHENTICATED_URL = re.compile(r"(?i)(https?://)([^/\s:@]*(?::[^@\s/]*)?@)")
PARTIAL_CREDENTIAL_URL = re.compile(r"(?i)(https?://)([^/\s:@]*:[^/\s@]*)(?=$|[\s])")
PARTIAL_AUTHENTICATED_URL = re.compile(r"(?i)(https?://)[^/\s]*$")
PORCELAIN_FLAG_FIELDS = {"bare", "detached"}
PORCELAIN_REQUIRED_VALUE_FIELDS = {"worktree", "HEAD", "branch"}
PORCELAIN_OPTIONAL_VALUE_FIELDS = {"locked", "prunable"}
TRUSTED_TOOL_DIRECTORIES = (
    "/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin",
    "/usr/local/sbin", "/opt/local/bin", "/usr/bin", "/bin",
    "/usr/sbin", "/sbin",
)
PROVISION_ENVIRONMENT_KEYS = (
    "PATH",
    "TMPDIR",
    "TMP",
    "TEMP",
    "CI",
    "npm_config_registry",
    "npm_config_proxy",
    "npm_config_http_proxy",
    "npm_config_https_proxy",
    "npm_config_noproxy",
    "NPM_CONFIG_REGISTRY",
    "NPM_CONFIG_PROXY",
    "NPM_CONFIG_HTTP_PROXY",
    "NPM_CONFIG_HTTPS_PROXY",
    "NPM_CONFIG_NOPROXY",
)
SENSITIVE_ENVIRONMENT_KEYS = tuple(
    key for key in PROVISION_ENVIRONMENT_KEYS
    if "PROXY" in key.upper() or "REGISTRY" in key.upper()
)


class PolicyError(RuntimeError):
    """A requested operation violates the shared-worktree contract."""

    def __init__(
        self,
        message: str,
        *,
        failed_step: str = "policy-gate",
        command: Sequence[str] = (),
        exit_code: int | None = None,
        stdout: str = "",
    ) -> None:
        super().__init__(message)
        self.failed_step = failed_step
        self.command = list(command)
        self.exit_code = exit_code
        self.stdout = stdout
        self.stderr = message


def trusted_tool_path() -> str:
    directories = list(TRUSTED_TOOL_DIRECTORIES)
    for parent in (Path("/opt/homebrew/opt"), Path("/usr/local/opt")):
        if parent.is_dir():
            directories[0:0] = [str(path) for path in sorted(parent.glob("node*/bin")) if path.is_dir()]
    return os.pathsep.join(dict.fromkeys(directories))


def git_environment() -> dict[str, str]:
    return {"PATH": trusted_tool_path(), "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull, "GIT_TERMINAL_PROMPT": "0"}


def _drain_process(process: subprocess.Popen[bytes], *, stdout_limit: int = MAX_DIAGNOSTIC_BYTES, stderr_limit: int = MAX_DIAGNOSTIC_BYTES) -> dict[str, dict[str, object]]:
    selector = selectors.DefaultSelector()
    captures: dict[str, dict[str, object]] = {}
    for label, stream, limit in (("stdout", process.stdout, stdout_limit), ("stderr", process.stderr, stderr_limit)):
        assert stream is not None
        captures[label] = {"buffer": bytearray(), "size": 0, "digest": hashlib.sha256(), "limit": limit}
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
        marker = f"\n[truncated; bytes={capture['size']}; sha256={capture['digest'].hexdigest()}]"
        if bounded:
            marker_size = len(marker.encode("utf-8"))
            prefix_size = max(0, MAX_DIAGNOSTIC_BYTES - marker_size)
            prefix = message.encode("utf-8", errors="replace")[:prefix_size].decode("utf-8", errors="ignore")
            return prefix + marker
        message += marker
    return bounded_diagnostic(message) if bounded else message


def _run_git(repo: Path, *args: str) -> tuple[subprocess.CompletedProcess[str], dict[str, dict[str, object]]]:
    command = ["git", "-C", str(repo), *args]
    try:
        process = subprocess.Popen(command, env=git_environment(), stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    except OSError as exc:
        raise PolicyError(
            f"could not launch git: {exc}",
            failed_step="git",
            command=command,
        ) from exc
    captures = _drain_process(process)
    return subprocess.CompletedProcess(
        command,
        process.returncode,
        _capture_text(captures["stdout"]),
        _capture_text(captures["stderr"]),
    ), captures


def git(repo: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    result, _ = _run_git(repo, *args)
    if check and result.returncode != 0:
        raise PolicyError(
            result.stderr.strip() or f"git {' '.join(args)} failed",
            failed_step="git",
            command=["git", "-C", str(repo), *args],
            exit_code=result.returncode,
            stdout=result.stdout,
        )
    return result


def owning_root(repo: Path) -> Path:
    result = git(repo.expanduser().resolve(), "rev-parse", "--show-toplevel")
    return Path(result.stdout.strip()).resolve()


def worktree_records(repo: Path) -> list[dict[str, object]]:
    command = ["git", "-C", str(repo), "worktree", "list", "--porcelain", "-z"]
    result, captures = _run_git(repo, "worktree", "list", "--porcelain", "-z")
    if result.returncode != 0:
        raise PolicyError(
            _capture_text(captures["stderr"]).strip() or "git worktree list failed",
            failed_step="git",
            command=command, exit_code=result.returncode, stdout=_capture_text(captures["stdout"]),
        )
    if captures["stdout"]["size"] > MAX_DIAGNOSTIC_BYTES:
        raise PolicyError("git worktree list output exceeded the diagnostic bound", failed_step="git", command=command, exit_code=0, stdout=_capture_text(captures["stdout"]))
    raw = bytes(captures["stdout"]["buffer"])
    if not raw.endswith(b"\0\0"):
        raise PolicyError(
            "git worktree list output was incomplete",
            failed_step="git",
            command=command,
            exit_code=result.returncode,
            stdout=_capture_text(captures["stdout"]),
        )

    def malformed(reason: str) -> None:
        raise PolicyError(
            f"git worktree list output was malformed: {reason}",
            failed_step="git",
            command=command,
            exit_code=result.returncode,
            stdout=_capture_text(captures["stdout"]),
        )

    records: list[dict[str, object]] = []
    current: dict[str, object] = {}

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
        if name not in PORCELAIN_FLAG_FIELDS | PORCELAIN_REQUIRED_VALUE_FIELDS | PORCELAIN_OPTIONAL_VALUE_FIELDS:
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
    if probe.returncode == 1:
        raise PolicyError("failed to protect .worktrees with a repository-local ignore rule")
    if probe.returncode != 0:
        raise PolicyError(
            probe.stderr.strip() or "git check-ignore failed",
            failed_step="git",
            command=["git", "-C", str(root), "check-ignore", "--no-index", ".worktrees/.probe"],
            exit_code=probe.returncode,
            stdout=probe.stdout,
        )
    return shared


def provision_environment(target: Path) -> dict[str, str]:
    environment = {
        key: os.environ[key]
        for key in PROVISION_ENVIRONMENT_KEYS
        if key in os.environ
    }
    environment["PATH"] = trusted_tool_path()
    environment["AGENTS_HOME"] = str(target)
    return environment


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


def bounded_command(command: Sequence[str]) -> list[str]:
    result: list[str] = []
    used = 0
    for value in command:
        safe = bounded_diagnostic(value)
        size = len(safe.encode()) + (1 if result else 0)
        if used + size > MAX_DIAGNOSTIC_BYTES:
            marker = "[truncated]"
            marker_size = len(marker.encode()) + (1 if result else 0)
            if used + marker_size <= MAX_DIAGNOSTIC_BYTES:
                result.append(marker)
            break
        result.append(safe)
        used += size
    return result


def provision_step(target: Path, step: str, command: list[str]) -> dict[str, object] | None:
    environment = provision_environment(target)
    try:
        process = subprocess.Popen(command, cwd=target, env=environment, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        captures = _drain_process(process, stdout_limit=0, stderr_limit=0)
        return_code = process.returncode
        captured_stdout = f"[child output omitted; bytes={captures['stdout']['size']}; sha256={captures['stdout']['digest'].hexdigest()}]"
        captured_stderr = f"[child output omitted; bytes={captures['stderr']['size']}; sha256={captures['stderr']['digest'].hexdigest()}]"
    except OSError as exc:
        return {
            "status": "failed",
            "failed_step": step,
            "command": bounded_command(command),
            "exit_code": None,
            "stdout": "",
            "stderr": bounded_diagnostic(exc),
        }
    if return_code == 0:
        return None
    return {
        "status": "failed",
        "failed_step": step,
        "command": bounded_command(command),
        "exit_code": return_code,
        "stdout": captured_stdout,
        "stderr": captured_stderr,
    }


@contextmanager
def admission_reservation(root: Path, name: str):
    validate_name(name)
    directory = common_git_dir(root) / ADMISSION_DIRECTORY
    try:
        directory.mkdir(mode=0o700, exist_ok=True)
        path = directory / f"{name}.lock"
        descriptor = os.open(str(path), os.O_CREAT | os.O_RDWR, 0o600)
    except OSError as exc:
        raise PolicyError("cannot create worktree admission reservation") from exc
    try:
        fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError as exc:
        os.close(descriptor)
        if exc.errno in {errno.EACCES, errno.EAGAIN}:
            raise PolicyError("worktree name is already admitted") from exc
        raise PolicyError(f"cannot acquire worktree admission reservation: {exc}") from exc
    try:
        yield
    finally:
        fcntl.flock(descriptor, fcntl.LOCK_UN)
        os.close(descriptor)


@contextmanager
def exclusive_provision_lock(target: Path):
    git_file = target / ".git"
    if git_file.is_symlink() or not git_file.is_file():
        raise PolicyError("worktree has no regular .git file for provisioning ownership")
    with git_file.open("rb") as lock:
        try:
            fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as exc:
            if exc.errno in {errno.EACCES, errno.EAGAIN}:
                raise PolicyError("worktree provisioning is already in progress") from exc
            raise PolicyError(f"cannot acquire worktree provisioning lock: {exc}") from exc
        try:
            yield
        finally:
            fcntl.flock(lock.fileno(), fcntl.LOCK_UN)


def install_attestation_path(target: Path) -> Path:
    return target / "runtime" / "agent-fabric" / ".npm-ci-attestation"


def provision_target_locked(
    root: Path,
    target: Path,
    name: str,
    start_step: str,
    operation: str,
) -> dict[str, object]:
    receipt = worktree_receipt(root, target, name)
    assumed_steps: list[str] = []
    if start_step == "protocol-build":
        if not install_attestation_path(target).is_file():
            raise PolicyError(
                "--from-step protocol-build requires a prior npm install attestation",
                failed_step="provision-gate",
            )
        assumed_steps = ["dependency-installation"]
    else:
        install_command = [str(target / "scripts" / "install-agent-fabric-dependencies")]
        failure = provision_step(target, "dependency-installation", install_command)
        if failure is not None:
            return {
                **receipt,
                **failure,
                "operation": operation,
                "provisioned_steps": [],
                "assumed_steps": [],
            }

    build_command = [str(target / "scripts" / "agent-fabric-protocol-build")]
    failure = provision_step(target, "protocol-build", build_command)
    if failure is not None:
        return {
            **receipt,
            **failure,
            "operation": operation,
            "provisioned_steps": [] if assumed_steps else ["dependency-installation"],
            "assumed_steps": assumed_steps,
        }
    return {
        **receipt,
        "operation": operation,
        "status": "ready",
        "provisioned_steps": ["protocol-build"] if assumed_steps else [
            "dependency-installation", "protocol-build",
        ],
        "assumed_steps": assumed_steps,
    }


def provision_target(
    root: Path,
    target: Path,
    name: str,
    start_step: str = "dependency-installation",
    require_clean: bool = False,
    operation: str = "provision",
) -> dict[str, object]:
    with exclusive_provision_lock(target):
        if require_clean:
            dirty = git(target, "status", "--porcelain=v1", "--untracked-files=all").stdout
            if dirty:
                raise PolicyError(
                    "worktree is dirty; preserve or hand off its changes before provisioning"
                )
        return provision_target_locked(root, target, name, start_step, operation)


def worktree_receipt(root: Path, target: Path, name: str) -> dict[str, object]:
    head_revision = git(target, "rev-parse", "HEAD").stdout.strip()
    branch_result = git(target, "symbolic-ref", "--quiet", "--short", "HEAD", check=False)
    if branch_result.returncode not in {0, 1}:
        raise PolicyError(
            branch_result.stderr.strip() or "cannot determine worktree branch identity",
            failed_step="git",
            command=["git", "-C", str(target), "symbolic-ref", "--quiet", "--short", "HEAD"],
            exit_code=branch_result.returncode,
            stdout=branch_result.stdout,
        )
    branch = branch_result.stdout.strip() if branch_result.returncode == 0 else None
    return {
        "name": name,
        "primary_root": str(root),
        "worktree_root": str(target),
        "common_git_dir": str(common_git_dir(root)),
        "head_revision": head_revision,
        "branch": branch,
        "detached": branch is None,
    }


def failed_receipt(
    operation: str,
    name: str | None,
    root: Path | None,
    target: Path | None,
    error: Exception,
) -> dict[str, object]:
    if not isinstance(error, PolicyError):
        error = PolicyError(str(error))
    return {
        "status": "failed",
        "operation": operation,
        "name": name,
        "primary_root": str(root) if root is not None else None,
        "worktree_root": str(target) if target is not None else None,
        "failed_step": error.failed_step,
        "command": bounded_command(error.command),
        "exit_code": error.exit_code,
        "stdout": bounded_diagnostic(error.stdout),
        "stderr": bounded_diagnostic(error.stderr),
        "provisioned_steps": [],
        "assumed_steps": [],
    }


def create(args: argparse.Namespace) -> dict[str, object]:
    root: Path | None = None
    target: Path | None = None
    try:
        if not args.human_authorised:
            raise PolicyError("creating a worktree requires explicit human authorisation")
        validate_name(args.name)
        root = primary_root(args.repo)
        with admission_reservation(root, args.name):
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
            return provision_target(root, target, args.name, operation="create")
    except (PolicyError, OSError) as exc:
        return failed_receipt("create", args.name, root, target, exc)


def provision(args: argparse.Namespace) -> dict[str, object]:
    root: Path | None = None
    target: Path | None = None
    try:
        if not args.human_authorised:
            raise PolicyError("provisioning an existing worktree requires explicit human authorisation")
        validate_name(args.name)
        root = primary_root(args.repo)
        with admission_reservation(root, args.name):
            shared = ensure_shared_root(root)
            target = shared / args.name
            if target.is_symlink() or not target.is_dir() or target.resolve().parent != shared.resolve():
                raise PolicyError(f"worktree target is outside canonical .worktrees: {target}")
            registered = {
                Path(str(item["worktree"])).resolve()
                for item in worktree_records(root)
                if item.get("worktree")
            }
            if target.resolve() not in registered:
                raise PolicyError(f"not a registered project worktree: {target}")
            return provision_target(
                root,
                target,
                args.name,
                args.start_step,
                require_clean=True,
                operation="provision",
            )
    except (PolicyError, OSError) as exc:
        return failed_receipt("provision", args.name, root, target, exc)


def remove(args: argparse.Namespace) -> dict[str, object]:
    root: Path | None = None
    target: Path | None = None
    try:
        if not args.human_authorised:
            raise PolicyError("removing a worktree requires explicit human authorisation")
        validate_name(args.name)
        root = primary_root(args.repo)
        with admission_reservation(root, args.name):
            shared = ensure_shared_root(root)
            target = shared / args.name
            if target.is_symlink() or not target.is_dir() or target.resolve().parent != shared.resolve():
                raise PolicyError(f"worktree target is outside canonical .worktrees: {target}")
            with exclusive_provision_lock(target):
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
    except (PolicyError, OSError) as exc:
        return failed_receipt("remove", args.name, root, target, exc)


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

    provision_parser = sub.add_parser("provision")
    provision_parser.add_argument("name")
    provision_parser.add_argument("--repo", type=Path, default=Path.cwd())
    provision_parser.add_argument("--human-authorised", action="store_true")
    provision_parser.add_argument(
        "--from-step",
        dest="start_step",
        choices=("dependency-installation", "protocol-build"),
        default="dependency-installation",
    )
    provision_parser.set_defaults(handler=provision)

    list_parser = sub.add_parser("list")
    list_parser.add_argument("--repo", type=Path, default=Path.cwd())
    list_parser.set_defaults(handler=list_worktrees)

    check_parser = sub.add_parser("check")
    check_parser.add_argument("--repo", type=Path, default=Path.cwd())
    check_parser.set_defaults(handler=check_worktrees)

    remove_parser = sub.add_parser("remove")
    remove_parser.add_argument("name")
    remove_parser.add_argument("--repo", type=Path, default=Path.cwd())
    remove_parser.add_argument("--human-authorised", action="store_true")
    remove_parser.set_defaults(handler=remove)
    return result


def policy_failure_receipt(args: argparse.Namespace, error: PolicyError) -> dict[str, object]:
    return failed_receipt(args.command, getattr(args, "name", None), None, None, error)


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        receipt = args.handler(args)
    except (PolicyError, OSError) as exc:
        print(json.dumps(policy_failure_receipt(args, exc), indent=2, sort_keys=True))
        print(f"worktree policy: {exc}", file=sys.stderr)
        return 2
    print(json.dumps(receipt, indent=2, sort_keys=True))
    if receipt.get("status") == "failed":
        print(f"worktree policy: {receipt.get('stderr', 'operation failed')}", file=sys.stderr)
    return 2 if receipt.get("status") in {"fail", "failed"} else 0


if __name__ == "__main__":
    raise SystemExit(main())
