"""Configure project-dynamic Agent Fabric MCP entries for supported clients."""

from __future__ import annotations

import argparse
import ctypes
from dataclasses import dataclass
import errno
from hashlib import sha256
import json
import os
from pathlib import Path
import re
import stat
import sys
import tempfile
import tomllib
from typing import Any


SERVER_NAME = "agent-fabric"
CODEX_TABLES = {"mcp_servers.agent-fabric", "mcp_servers.agent-fabric.env"}
TABLE_HEADER = re.compile(r"^\s*\[([^\[\]]+)]\s*(?:#.*)?$")
CLIENT_LABELS = {"opencode": "OpenCode"}


class RegistrationError(ValueError):
    pass


class RegistrationConflictError(RegistrationError):
    pass


class RegistrationPartialStateError(RegistrationConflictError):
    """A config write failed after the live path may have changed."""


class RegistrationOutputError(RegistrationError):
    pass


def _client_label(client: str) -> str:
    return CLIENT_LABELS.get(client, client.title())


@dataclass(frozen=True)
class FileIdentity:
    device: int
    inode: int
    mode: int
    size: int
    modified_ns: int


@dataclass(frozen=True)
class ConfigSnapshot:
    requested_path: Path
    target_path: Path
    source_kind: str
    source_identity: FileIdentity | None
    symlink_value: str | None
    target_identity: FileIdentity | None
    digest: str | None
    content: bytes


@dataclass(frozen=True)
class ConfigProposal:
    client: str
    snapshot: ConfigSnapshot
    content: str
    status: str


def _identity(value: os.stat_result) -> FileIdentity:
    return FileIdentity(value.st_dev, value.st_ino, value.st_mode, value.st_size, value.st_mtime_ns)


def _lstat(path: Path) -> os.stat_result | None:
    try:
        return path.lstat()
    except FileNotFoundError:
        return None


def _read_regular(path: Path, client: str) -> tuple[os.stat_result, bytes]:
    descriptor: int | None = None
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode):
            raise RegistrationError(f"{client} config must be a regular file")
        with os.fdopen(descriptor, "rb") as handle:
            descriptor = None
            content = handle.read()
            after = os.fstat(handle.fileno())
        if _identity(after) != _identity(before):
            raise RegistrationConflictError(f"{client} config changed while it was being read")
        return before, content
    except OSError as exc:
        if exc.errno == errno.ELOOP:
            raise RegistrationError(f"{client} config must not resolve through a symbolic link") from exc
        raise
    finally:
        if descriptor is not None:
            os.close(descriptor)


def _capture(path: Path, client: str) -> ConfigSnapshot:
    source_before = _lstat(path)
    if source_before is None:
        return ConfigSnapshot(path, path, "absent", None, None, None, None, b"")
    source_identity = _identity(source_before)
    symlink_value: str | None = None
    if stat.S_ISLNK(source_before.st_mode):
        try:
            symlink_value = os.readlink(path)
            target = path.resolve(strict=True)
        except (OSError, RuntimeError) as exc:
            raise RegistrationError(f"{client} config symlink cannot be resolved: {exc}") from exc
        source_kind = "symlink"
    else:
        target = path
        source_kind = "direct"
    try:
        target_before, content = _read_regular(target, client)
    except OSError as exc:
        raise RegistrationError(f"{client} config cannot be read: {exc}") from exc
    source_after = _lstat(path)
    target_after = _lstat(target)
    if (
        source_after is None or target_after is None or
        _identity(source_after) != source_identity or
        _identity(target_after) != _identity(target_before) or
        (source_kind == "symlink" and os.readlink(path) != symlink_value)
    ):
        raise RegistrationConflictError(f"{client} config changed while it was being read")
    return ConfigSnapshot(
        path, target, source_kind, source_identity, symlink_value,
        _identity(target_before), sha256(content).hexdigest(), content,
    )


def _text(snapshot: ConfigSnapshot, client: str) -> str:
    try:
        return snapshot.content.decode()
    except UnicodeDecodeError as exc:
        raise RegistrationError(f"{client} config is not UTF-8: {exc}") from exc


def registration(
    agents_home: Path,
    state_directory: Path,
    seat: str,
    client_label: str | None = None,
    shim_path: Path | None = None,
) -> dict[str, Any]:
    """Return a registration through the stable, relocation-safe Provenant shim.

    ``agents_home`` remains part of the public call shape while existing
    callers migrate; the registration intentionally contains no checkout path.
    Tests may override ``shim_path`` without touching the live installation.
    """
    del agents_home
    stable_shim = (shim_path or Path.home() / ".local/bin/provenant").expanduser()
    if not stable_shim.is_absolute():
        raise RegistrationError("stable Provenant shim path must be absolute")
    environment = {
        "AGENT_FABRIC_STATE_DIRECTORY": str(state_directory),
        "AGENT_FABRIC_SEAT": seat,
        "AGENT_FABRIC_CLIENT_LABEL": client_label or seat,
    }
    result: dict[str, Any] = {
        "command": str(stable_shim),
        "env": environment,
    }
    if seat == "claude":
        result = {"type": "stdio", **result, "args": []}
    return result


def claude_update(path: Path, desired: dict[str, Any]) -> ConfigProposal:
    return json_client_update(path, desired, "claude")


def json_client_update(path: Path, desired: dict[str, Any], client: str) -> ConfigProposal:
    label = client.title()
    snapshot = _capture(path, label)
    text = _text(snapshot, label)
    try:
        value: Any = json.loads(text) if snapshot.source_kind != "absent" else {}
    except json.JSONDecodeError as exc:
        raise RegistrationError(f"{label} config is invalid JSON: {exc}") from exc
    if not isinstance(value, dict):
        raise RegistrationError(f"{label} config root must be an object")
    servers = value.setdefault("mcpServers", {})
    if not isinstance(servers, dict):
        raise RegistrationError(f"{label} config mcpServers must be an object")
    if servers.get(SERVER_NAME) == desired:
        return ConfigProposal(client, snapshot, text, "existing")
    servers[SERVER_NAME] = desired
    return ConfigProposal(client, snapshot, json.dumps(value, indent=2, sort_keys=True) + "\n", "ready")


def opencode_update(path: Path, desired: dict[str, Any]) -> ConfigProposal:
    client = "opencode"
    label = "OpenCode"
    snapshot = _capture(path, label)
    text = _text(snapshot, label)
    try:
        value: Any = json.loads(text) if snapshot.source_kind != "absent" else {}
    except json.JSONDecodeError as exc:
        raise RegistrationError(f"{label} config is invalid JSON: {exc}") from exc
    if not isinstance(value, dict):
        raise RegistrationError(f"{label} config root must be an object")
    servers = value.setdefault("mcp", {})
    if not isinstance(servers, dict):
        raise RegistrationError(f"{label} config mcp must be an object")
    entry = {
        "type": "local",
        "command": [desired["command"]],
        "enabled": True,
        "environment": desired["env"],
    }
    if servers.get(SERVER_NAME) == entry:
        return ConfigProposal(client, snapshot, text, "existing")
    servers[SERVER_NAME] = entry
    return ConfigProposal(client, snapshot, json.dumps(value, indent=2, sort_keys=True) + "\n", "ready")


def _codex_value(text: str) -> dict[str, Any]:
    try:
        value = tomllib.loads(text) if text.strip() else {}
    except tomllib.TOMLDecodeError as exc:
        raise RegistrationError(f"Codex config is invalid TOML: {exc}") from exc
    if not isinstance(value, dict):
        raise RegistrationError("Codex config root must be a table")
    return value


def _remove_codex_tables(text: str) -> tuple[str, bool]:
    output: list[str] = []
    skipping = False
    found = False
    for line in text.splitlines(keepends=True):
        match = TABLE_HEADER.match(line)
        if match:
            name = match.group(1).strip()
            skipping = name in CODEX_TABLES
            found = found or skipping
        if not skipping:
            output.append(line)
    return "".join(output).rstrip(), found


def _toml_string(value: str) -> str:
    return json.dumps(value, ensure_ascii=True)


def _codex_block(desired: dict[str, Any]) -> str:
    environment = desired["env"]
    return "\n".join([
        "# agent-harness: project-dynamic agent-fabric MCP",
        "[mcp_servers.agent-fabric]",
        f"command = {_toml_string(desired['command'])}",
        "",
        "[mcp_servers.agent-fabric.env]",
        *[f"{key} = {_toml_string(value)}" for key, value in environment.items()],
        "",
    ])


def codex_update(path: Path, desired: dict[str, Any]) -> ConfigProposal:
    snapshot = _capture(path, "Codex")
    text = _text(snapshot, "Codex")
    value = _codex_value(text)
    servers = value.get("mcp_servers", {})
    if not isinstance(servers, dict):
        raise RegistrationError("Codex config mcp_servers must be a table")
    existing = servers.get(SERVER_NAME)
    if existing == desired:
        return ConfigProposal("codex", snapshot, text, "existing")
    prefix, found = _remove_codex_tables(text)
    if existing is not None and not found:
        raise RegistrationError("Codex agent-fabric entry uses an unsupported inline or quoted table form")
    updated = (prefix + "\n\n" if prefix else "") + _codex_block(desired)
    parsed = _codex_value(updated)
    if parsed.get("mcp_servers", {}).get(SERVER_NAME) != desired:
        raise RegistrationError("composed Codex agent-fabric entry is invalid")
    return ConfigProposal("codex", snapshot, updated, "ready")


def _assert_unchanged(proposal: ConfigProposal) -> None:
    label = _client_label(proposal.client)
    current = _capture(proposal.snapshot.requested_path, label)
    if current != proposal.snapshot:
        raise RegistrationConflictError(f"{label} config changed after registration was composed")


def atomic_exchange(first: Path, second: Path) -> None:
    """Atomically exchange two same-filesystem paths without an overwrite gap."""
    library = ctypes.CDLL(None, use_errno=True)
    first_bytes = os.fsencode(first)
    second_bytes = os.fsencode(second)
    if sys.platform == "darwin":
        operation = library.renamex_np
        operation.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
        arguments = (first_bytes, second_bytes, 0x00000002)  # RENAME_SWAP
    elif sys.platform.startswith("linux"):
        try:
            operation = library.renameat2
        except AttributeError as exc:
            raise RegistrationError("atomic config exchange is unavailable on this platform") from exc
        operation.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
        arguments = (-100, first_bytes, -100, second_bytes, 0x00000002)  # AT_FDCWD, RENAME_EXCHANGE
    else:
        raise RegistrationError("atomic config exchange is unavailable on this platform")
    operation.restype = ctypes.c_int
    if operation(*arguments) != 0:
        error_number = ctypes.get_errno()
        raise OSError(error_number, os.strerror(error_number), str(second))


def _displaced_matches(proposal: ConfigProposal, displaced: Path) -> bool:
    try:
        identity, content = _read_regular(displaced, _client_label(proposal.client))
    except (OSError, RegistrationError):
        return False
    return _identity(identity) == proposal.snapshot.target_identity \
        and sha256(content).hexdigest() == proposal.snapshot.digest \
        and content == proposal.snapshot.content


def _requested_resolves_to_installed(proposal: ConfigProposal, installed: FileIdentity) -> bool:
    try:
        source = _lstat(proposal.snapshot.requested_path)
        if source is None:
            return False
        if proposal.snapshot.source_kind == "symlink":
            if (
                _identity(source) != proposal.snapshot.source_identity or
                os.readlink(proposal.snapshot.requested_path) != proposal.snapshot.symlink_value
            ):
                return False
        elif _identity(source) != installed:
            return False
        resolved = proposal.snapshot.requested_path.resolve(strict=True)
        if proposal.snapshot.source_kind == "symlink" and resolved != proposal.snapshot.target_path:
            return False
        target, _ = _read_regular(resolved, _client_label(proposal.client))
        return _identity(target) == installed
    except (OSError, RegistrationError, RuntimeError):
        return False


def _fsync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | os.O_NOFOLLOW)
    try:
        if not stat.S_ISDIR(os.fstat(descriptor).st_mode):
            raise RegistrationError(f"recovery parent is not a directory: {path}")
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _cleanup_recovery_directory(recovery_directory: Path, target_parent: Path) -> None:
    try:
        _fsync_directory(recovery_directory)
    except OSError:
        print(
            "warning: post-commit recovery cleanup failed "
            f"operation=recovery-directory-fsync path={recovery_directory}",
            file=sys.stderr,
        )
    try:
        recovery_directory.rmdir()
    except OSError:
        print(
            "warning: post-commit recovery cleanup failed "
            f"operation=recovery-directory-rmdir path={recovery_directory}",
            file=sys.stderr,
        )
        return
    try:
        resolved_target_parent = target_parent.resolve(strict=True)
    except (OSError, RuntimeError):
        print(
            "warning: post-commit recovery cleanup failed "
            f"operation=target-parent-resolve path={target_parent}",
            file=sys.stderr,
        )
        return
    try:
        _fsync_directory(resolved_target_parent)
    except OSError:
        print(
            "warning: post-commit recovery cleanup failed "
            f"operation=target-parent-fsync path={resolved_target_parent}",
            file=sys.stderr,
        )


def write_proposal(proposal: ConfigProposal) -> None:
    target = proposal.snapshot.target_path
    label = _client_label(proposal.client)
    target.parent.mkdir(parents=True, exist_ok=True)
    recovery_directory = Path(tempfile.mkdtemp(
        dir=target.parent, prefix=f".{target.name}.recovery.",
    )).resolve(strict=True)
    recovery_stat = recovery_directory.lstat()
    if (
        not stat.S_ISDIR(recovery_stat.st_mode) or
        recovery_stat.st_uid != os.geteuid() or
        stat.S_IMODE(recovery_stat.st_mode) & 0o077
    ):
        recovery_directory.rmdir()
        raise RegistrationError("private recovery directory could not be established")
    temporary: Path | None = None
    retain_temporary = False
    installed = False
    try:
        try:
            with tempfile.NamedTemporaryFile(
                "w", dir=recovery_directory, prefix=f".{target.name}.", suffix=".tmp", delete=False,
            ) as handle:
                temporary = Path(handle.name)
                os.chmod(handle.fileno(), 0o600)
                handle.write(proposal.content)
                handle.flush()
                os.fsync(handle.fileno())
            installed_identity = _identity(temporary.stat(follow_symlinks=False))
            _assert_unchanged(proposal)
            if proposal.snapshot.target_identity is None:
                try:
                    os.link(temporary, target, follow_symlinks=False)
                except FileExistsError as exc:
                    raise RegistrationConflictError(
                        f"{label} config changed after registration was composed",
                    ) from exc
                installed = True
                retain_temporary = True
                try:
                    _fsync_directory(recovery_directory)
                    _fsync_directory(target.parent.resolve(strict=True))
                except OSError as exc:
                    raise RegistrationConflictError(
                        f"{label} config install durability failed; "
                        f"preserve private recovery file {temporary}",
                    ) from exc
                if not _requested_resolves_to_installed(proposal, installed_identity):
                    raise RegistrationConflictError(
                        f"{label} config changed during atomic install; "
                        f"preserve private recovery file {temporary}",
                    )
                retain_temporary = False
            else:
                try:
                    atomic_exchange(temporary, target)
                except (OSError, RegistrationError):
                    raise
                installed = True
                retain_temporary = True
                try:
                    _fsync_directory(recovery_directory)
                    _fsync_directory(target.parent.resolve(strict=True))
                except OSError as exc:
                    raise RegistrationConflictError(
                        f"{label} config exchange failed; "
                        f"preserve private recovery file {temporary}",
                    ) from exc
                if (
                    not _displaced_matches(proposal, temporary) or
                    not _requested_resolves_to_installed(proposal, installed_identity)
                ):
                    raise RegistrationConflictError(
                        f"{label} config changed during atomic exchange; "
                        f"preserve displaced recovery file {temporary}",
                    )
                retain_temporary = False
        finally:
            cleanup_ready = True
            if temporary and not retain_temporary and temporary.exists():
                try:
                    temporary.unlink()
                except OSError:
                    cleanup_ready = False
                    print(
                        f"warning: {label} config installed; cleanup failed; "
                        f"preserve private recovery file {temporary}",
                        file=sys.stderr,
                    )
            if not retain_temporary and cleanup_ready and recovery_directory.exists():
                _cleanup_recovery_directory(recovery_directory, target.parent)
    except RegistrationPartialStateError:
        raise
    except (OSError, RegistrationError) as exc:
        if installed:
            raise RegistrationPartialStateError(str(exc)) from exc
        raise


def _report(proposal: ConfigProposal, verb: str) -> None:
    try:
        print(
            f"agent-fabric MCP {verb} platform={proposal.client} config={proposal.snapshot.target_path}",
            flush=True,
        )
    except (OSError, ValueError) as exc:
        _neutralize_failed_stream("stdout")
        raise RegistrationOutputError(f"agent-fabric MCP receipt output failed: {exc}") from exc


def _neutralize_failed_stream(name: str) -> None:
    try:
        replacement = open(os.devnull, "w", encoding="utf-8")
    except OSError:
        return
    # Do not close the failed stream: closing can retry the same buffered write.
    # Replacing the interpreter-owned reference lets shutdown flush a clean sink.
    setattr(sys, name, replacement)


def _report_partial_state(
    *,
    cause: str,
    committed: list[str],
    remaining: list[str],
    config: Path,
    error: BaseException,
    recovery: str,
) -> None:
    diagnostic = (
        "partial-state: agent-fabric MCP registration "
        f"cause={cause} committed={','.join(committed) or 'none'} "
        f"remaining={','.join(remaining) or 'none'} config={config} "
        f"error={str(error).replace(chr(10), ' ')}; recovery={recovery}"
    )
    try:
        print(diagnostic, file=sys.stderr, flush=True)
    except (OSError, ValueError):
        # Exit code 4 remains the machine-readable partial-state signal when
        # the diagnostic stream is unavailable too.
        _neutralize_failed_stream("stderr")


def main(argv: list[str] | None = None) -> int:
    # Python 3.14 argparse probes sys.stdout for colour support while the
    # parser is constructed; with a closed stdout that raises before the
    # guarded output paths below can report partial state. PYTHON_COLORS=0
    # short-circuits the probe, and this mechanical configurer never
    # colourises its output anyway.
    os.environ.setdefault("PYTHON_COLORS", "0")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--platform",
        choices=("all", "claude", "codex", "cursor", "agy", "kiro", "opencode"),
        default="all",
    )
    parser.add_argument("--agents-home", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument(
        "--state-directory", type=Path,
        default=Path(os.environ.get("AGENT_FABRIC_STATE_DIRECTORY", Path.home() / ".local/state/agent-harness/fabric")),
    )
    parser.add_argument("--shim-path", type=Path, default=Path.home() / ".local/bin/provenant")
    parser.add_argument("--claude-config", type=Path, default=Path.home() / ".claude.json")
    parser.add_argument(
        "--codex-config", type=Path,
        default=Path(os.environ.get("CODEX_HOME", Path.home() / ".codex")) / "config.toml",
    )
    parser.add_argument("--cursor-config", type=Path, default=Path.home() / ".cursor/mcp.json")
    parser.add_argument("--agy-config", type=Path, default=Path.home() / ".gemini/config/mcp_config.json")
    parser.add_argument("--kiro-config", type=Path, default=Path.home() / ".kiro/settings/mcp.json")
    parser.add_argument(
        "--opencode-config", type=Path,
        default=Path.home() / ".config/opencode/opencode.jsonc",
    )
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true")
    mode.add_argument("--preflight", action="store_true")
    args = parser.parse_args(argv)
    try:
        agents_home = args.agents_home.resolve(strict=True)
        shim_path = args.shim_path.expanduser()
        if not shim_path.is_absolute():
            raise RegistrationError("stable Provenant shim path must be absolute")
        if (
            not args.preflight
            and (not shim_path.is_file() or not os.access(shim_path, os.X_OK))
        ):
            raise RegistrationError(
                f"stable Provenant shim is missing or not executable at {shim_path}"
            )
        state_directory = args.state_directory.expanduser()
        if not state_directory.is_absolute():
            raise RegistrationError("Agent Fabric state directory must be absolute")
        proposals: list[ConfigProposal] = []
        if args.platform in {"all", "claude"}:
            proposals.append(claude_update(
                args.claude_config,
                registration(agents_home, state_directory, "claude", shim_path=shim_path),
            ))
        if args.platform in {"all", "codex"}:
            proposals.append(codex_update(
                args.codex_config,
                registration(agents_home, state_directory, "codex", shim_path=shim_path),
            ))
        optional_configs = {
            "cursor": args.cursor_config,
            "agy": args.agy_config,
            "kiro": args.kiro_config,
        }
        for client, path in optional_configs.items():
            if args.platform in {"all", client}:
                proposals.append(json_client_update(
                    path,
                    registration(
                        agents_home,
                        state_directory,
                        "codex",
                        client,
                        shim_path=shim_path,
                    ),
                    client,
                ))
        if args.platform in {"all", "opencode"}:
            proposals.append(opencode_update(
                args.opencode_config,
                registration(
                    agents_home,
                    state_directory,
                    "codex",
                    "opencode",
                    shim_path=shim_path,
                ),
            ))
        if args.check:
            missing = [proposal.client for proposal in proposals if proposal.status != "existing"]
            if missing:
                print("missing: agent-fabric MCP registration for " + ", ".join(missing))
                return 1
            for proposal in proposals:
                _assert_unchanged(proposal)
                _report(proposal, "verified")
            return 0
        elif args.preflight:
            for proposal in proposals:
                _assert_unchanged(proposal)
                _report(proposal, f"preflight-{proposal.status}")
            return 0
        else:
            committed: list[str] = []
            for index, proposal in enumerate(proposals):
                if proposal.status == "existing":
                    try:
                        _assert_unchanged(proposal)
                        _report(proposal, "existing")
                    except RegistrationOutputError as exc:
                        if not committed:
                            raise
                        _report_partial_state(
                            cause="receipt-output",
                            committed=committed,
                            remaining=[candidate.client for candidate in proposals[index:]],
                            config=proposal.snapshot.target_path,
                            error=exc,
                            recovery=(
                                "restore stdout, inspect the committed configuration, "
                                "then rerun --platform all"
                            ),
                        )
                        return 4
                    except (OSError, RegistrationError) as exc:
                        if not committed:
                            raise
                        _report_partial_state(
                            cause="config-conflict",
                            committed=committed,
                            remaining=[candidate.client for candidate in proposals[index:]],
                            config=proposal.snapshot.target_path,
                            error=exc,
                            recovery=(
                                "reconcile the reported configuration and any recovery file, "
                                "then rerun --platform all"
                            ),
                        )
                        return 4
                    continue
                try:
                    write_proposal(proposal)
                except RegistrationPartialStateError as exc:
                    _report_partial_state(
                        cause="config-conflict",
                        committed=committed,
                        remaining=[candidate.client for candidate in proposals[index:]],
                        config=proposal.snapshot.target_path,
                        error=exc,
                        recovery=(
                            "reconcile the reported configuration and any recovery file, "
                            "then rerun --platform all"
                        ),
                    )
                    return 4
                except (OSError, RegistrationError) as exc:
                    if not committed:
                        raise
                    _report_partial_state(
                        cause="config-conflict",
                        committed=committed,
                        remaining=[candidate.client for candidate in proposals[index:]],
                        config=proposal.snapshot.target_path,
                        error=exc,
                        recovery=(
                            "reconcile the reported configuration and any recovery file, "
                            "then rerun --platform all"
                        ),
                    )
                    return 4
                committed.append(proposal.client)
                try:
                    _report(proposal, "configured")
                except RegistrationOutputError as exc:
                    _report_partial_state(
                        cause="receipt-output",
                        committed=committed,
                        remaining=[candidate.client for candidate in proposals[index + 1:]],
                        config=proposal.snapshot.target_path,
                        error=exc,
                        recovery=(
                            "restore stdout, inspect the committed configuration, "
                            "then rerun --platform all"
                        ),
                    )
                    return 4
            return 0
    except (OSError, RegistrationError) as exc:
        print(f"conflicting: {exc}", file=sys.stderr)
        return 3


if __name__ == "__main__":
    raise SystemExit(main())
