#!/usr/bin/env python3
"""One provider descendant supervisor, shared by direct and Fabric owners."""

from __future__ import annotations

import argparse
from collections import deque, namedtuple
import ctypes
import difflib
import fcntl
import functools
import hashlib
import importlib.util
import json
import logging
import math
import os
from pathlib import Path
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import threading
import time
import uuid
import unicodedata
from datetime import UTC, datetime, timedelta

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from adapters import profile
from adapters import claude as claude_adapter
from output_custody import install, verify, CustodyError
import attempt_storage
import fabric_policy
import layout
import context_usage
import process_info
import run_lineage
import provider_output
import provider_observer
import provider_confinement
try:
    from . import instruction_quarantine, agy_home
except ImportError:
    import instruction_quarantine
    import agy_home
instruction_quarantine.provider_exec = sys.modules[__name__]
agy_home.provider_exec = sys.modules[__name__]
import stall_watchdog
from _shared import harness_profile
from trusted_git import owner_location, prepare_writer_ref, writer_git_writes, writer_read_only


CAPABILITY_VALUES = frozenset({"postgres", "browser"})
# Every writer's Git write boundary is trusted_git.writer_git_writes: its private worktree Git
# directory, the object store and its own branch ref. The rest of the common directory (hooks,
# config, info, other refs and worktrees) runs code or holds state for every checkout, so it
# stays read-only.
# Automatic gc and rerere write gc.pid, gc.log and rr-cache in the read-only common directory.
WRITER_GIT_CONFIG = (("gc.auto", "0"), ("maintenance.auto", "false"), ("rerere.enabled", "false"))
# Repository agent instructions and skills. Codex protects them inside a writable root.
INSTRUCTION_DIR = ".agents"
# A provider that has written nothing to stdout this long after launch is failed
# as startup_timeout so the chair can re-route instead of waiting out the run.
STARTUP_SECONDS = 300.0
# Instructions are small text; a larger tree is refused rather than hashed at length.
INSTRUCTION_BYTES_LIMIT = 256 * 1024 * 1024


def now():
    return datetime.now(UTC).isoformat(timespec="milliseconds").replace("+00:00", "Z")


@functools.cache
def harness_revision():
    """Installed Provenant revision for lane provenance; None outside a Git checkout."""
    try:
        result = subprocess.run(
            ["git", "-C", str(Path(__file__).resolve().parents[3]), "rev-parse", "HEAD"],
            capture_output=True, text=True, timeout=2, stdin=subprocess.DEVNULL,
            env={key: value for key, value in os.environ.items() if not key.startswith("GIT_")},
        )
    except (OSError, subprocess.SubprocessError):
        return None
    return result.stdout.strip() or None if result.returncode == 0 else None


class CapabilityError(ValueError):
    def __init__(self, code, fix):
        self.code = code
        self.fix = fix
        super().__init__(fix)


def capability_values(value):
    """Return the canonical capability list, rejecting malformed opt-ins."""
    if not isinstance(value, list) or any(not isinstance(item, str) for item in value):
        raise CapabilityError("capabilities_invalid", "Pass capabilities as a list of distinct postgres or browser values.")
    if len(value) != len(set(value)) or not set(value) <= CAPABILITY_VALUES:
        raise CapabilityError("capabilities_invalid", "Pass capabilities as a list of distinct postgres or browser values.")
    return sorted(value)


def parse_capabilities_argument(value):
    try:
        decoded = json.loads(value)
    except ValueError as exc:
        raise argparse.ArgumentTypeError("Pass capabilities as a JSON list.") from exc
    if not isinstance(decoded, list):
        raise argparse.ArgumentTypeError("Pass capabilities as a JSON list.")
    return decoded


def validate_capabilities(value, *, adapter, mode, sandbox, network):
    """Only Codex's own sandbox withholds these. Other adapters' sandbox-exec profile leaves SysV IPC
    and Mach services open, so they need no grant; a full Codex sandbox has no boundary at all."""
    capabilities = capability_values(value)
    if not capabilities or adapter != "codex":
        return capabilities
    if mode != "worktree_write":
        raise CapabilityError("capabilities_mode_invalid", "Pass capabilities only with mode=worktree_write.")
    if sandbox == "full":
        return capabilities
    if sandbox != "workspace-write":
        raise CapabilityError("capabilities_sandbox_invalid", "Use sandbox workspace-write or full with Codex capabilities.")
    if network is None:
        network = os.environ.get("CF_DISPATCH_CODEX_NETWORK", "1") == "1"
    checks = (
        (sys.platform == "darwin", "capabilities_platform_invalid", "Pass capabilities only on macOS."),
        (provider_confinement._sandbox_exec_path() is not None, "capabilities_confinement_unavailable", "Use capabilities only with usable sandbox-exec outside another sandbox."),
        (network is True, "capabilities_network_required", "Pass network=true for Codex capabilities."),
    )
    for valid, code, fix in checks:
        if not valid:
            raise CapabilityError(code, fix)
    return capabilities


























def _ensure_locks_dir(run_root):
    """Create missing .agent-run and locks one leaf at a time; None if a link is in the way."""
    locks = run_root / ".agent-run" / "locks"
    for path in (locks.parent, locks):
        try:
            path.lstat()
        except FileNotFoundError:
            try:
                path.mkdir()
            except FileExistsError:
                pass
            except OSError:
                return None
    return str(locks) if provider_confinement._real_dir_chain(locks) else None




























# Project files and directories a provider loads from each directory above its cwd.















































def _require_kiro_engine():
    """Refuse a confined Kiro lane before kiro-cli would unpack its engine into a read-only directory."""
    cli = shutil.which("kiro-cli")
    if not cli:
        return
    words = subprocess.run([cli, "--version"], capture_output=True, text=True, timeout=10).stdout.split()
    version = words[-1] if words else ""
    support = Path.home() / "Library/Application Support/kiro-cli"
    if version and not (any((support / "kas").glob(f"{version}-*/node_modules")) and (support / "node").is_file()):
        raise ValueError(
            f"kiro-cli {version} has not installed its engine, and a confined lane cannot write it; "
            "fix: run kiro-cli once outside the sandbox, for example "
            "`kiro-cli chat --no-interactive --agent-engine v3 hello`"
        )


def build_plan(
    adapter,
    route,
    prompt,
    *,
    cwd=None,
    workspace_root=None,
    read_roots=(),
    mode="read_only",
    worktree=None,
    sandbox=None,
    network=None,
    add_dirs=(),
    capabilities=None,
    timeout_seconds=None,
    startup_seconds=None,
    preface=True,
    run_id="",
    chair="",
    resume_session=None,
    session_id=None,
    requested_model=None,
    requested_effort=None,
    intent="ordinary",
    context_ceiling=None,
    account=None,
    **metadata,
):
    metadata = dict(metadata)
    capabilities = capability_values([] if capabilities is None else capabilities)
    prompt_file = metadata.pop("prompt_file", None)
    original_prompt_file = metadata.pop("original_prompt_file", prompt_file)
    run_dir = metadata.pop("run_dir", None)
    config = profile(adapter)
    account_warnings = []
    if account is not None and adapter != "claude":
        raise ValueError("--account is supported only for Claude")
    if adapter == "claude":
        spec = importlib.util.spec_from_file_location(
            'provenant_lane_quota', Path(__file__).resolve().parents[3] / 'scripts/provider_quota.py')
        quota = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(quota)
        try:
            candidates = quota.accounts('claude')
            entries = quota.configured().get('claude', {})
        except ValueError as exc:
            if account is not None:
                raise ValueError('invalid Claude account configuration; cannot honour explicit --account') from exc
            candidates, entries, account = [], {}, None
            account_warnings.append('invalid Claude account configuration; using ambient default identity')
        if account is None and not resume_session and not claude_adapter.uses_api_key(route):
            account = quota.choose_claude_account({row.label: quota.load_snapshot('claude', row.label, row.identity_dir)
                                                  for row in candidates}, datetime.now(UTC))
        chosen = next((row for row in candidates if row.label == account), None)
        if account is not None and chosen is None:
            # Implicit default/path labels also pin receipts during preflight or session relaunch.
            ambient = quota.identity.selected_identity('claude')
            if account != quota.identity.account_key('claude', ambient, entries=entries):
                raise ValueError("unknown or disabled Claude account label")
        identity_dir = (chosen.identity_dir if chosen else quota.identity.selected_identity('claude')).expanduser().absolute()
        selected = identity_dir.resolve()
        # Reuse the existing provider-home guard; no credential contents are needed.
        metadata['provider_home'] = str(selected)
        # Unset and explicitly set default paths use different CLI config/Keychain names.
        # Preserve the caller's exact spelling when retaining its ambient identity.
        ambient = quota.identity.selected_identity('claude').resolve()
        if selected != ambient:
            metadata['claude_config_dir'] = (None if selected == quota.identity.default_identity('claude').resolve()
                                             else str(identity_dir))
        metadata['account'] = account or quota.identity.account_key('claude', selected, entries=entries)
        metadata['claude_account_dirs'] = list(dict.fromkeys(
            [str(quota.identity.default_identity('claude').resolve()),
             *[str(Path(entry.get('identity_dir') or quota.identity.default_identity('claude')).expanduser().resolve())
               for entry in entries.values() if isinstance(entry, dict)]]))
    if adapter == "opencode":
        version_error = provider_confinement._opencode_version_error(config.CLI, os.environ.get("PATH"))
        if version_error:
            raise CapabilityError("unsupported_cli_version", version_error)
    selected_cwd = Path(worktree or cwd or workspace_root or Path.cwd()).expanduser().resolve()
    if not selected_cwd.is_dir():
        raise ValueError("cwd must be a readable directory")
    # A writer's worktree may sit outside the caller's tree; only a read cwd is bounded here.
    if workspace_root and not worktree and not any(
            selected_cwd.is_relative_to(root) for root in provider_confinement.read_boundary(workspace_root, read_roots)):
        raise ValueError("cwd must be inside the workspace")
    workspace_root = str(Path(workspace_root or selected_cwd).expanduser().resolve())
    cwd = str(selected_cwd)
    git_common = None
    git_private = None
    git_writes = []
    if mode == "worktree_write" and worktree is not None:
        location = owner_location(cwd, common=metadata.pop("common_git_dir", None))
        location.validate()
        if location.private == location.common:
            raise ValueError("worktree_write requires a linked worktree; fix: create a linked worktree")
        git_private, git_common = location.private, location.common
        metadata["trusted_git"] = location.record()
        metadata["lane_ref"] = location.writer_ref()
        if metadata["lane_ref"]:
            prepare_writer_ref(git_common, metadata["lane_ref"])
        git_dirs, git_files = writer_git_writes(git_private, git_common, metadata["lane_ref"])
        git_writes = [str(path) for path in (*git_dirs, *git_files)]
    guarded = provider_confinement.check_protected_inputs(route, workspace_root, cwd, worktree=worktree,
                                     prompt_file=original_prompt_file, add_dirs=add_dirs,
                                     read_roots=read_roots)
    if prompt_file is not None and prompt_file != original_prompt_file:
        provider_confinement.check_protected_inputs(route, workspace_root, cwd, worktree=worktree,
                               prompt_file=prompt_file, add_dirs=add_dirs, read_roots=read_roots)
    sandbox = sandbox or (
        "workspace-write" if mode == "worktree_write" else "read-only"
    )
    if mode == "read_only" and sandbox != "read-only":
        raise ValueError("write sandbox on a read-only run is forbidden")
    if sandbox not in {"read-only", "workspace-write", "full"}:
        raise ValueError("invalid sandbox")
    if network is not None and type(network) is not bool:
        raise ValueError("network must be a boolean")
    # Catalogue notices demoted to receipt notes must not become warnings again.
    warnings = list(dict.fromkeys([
        *account_warnings,
        *(note for note in route.get("notes") or [] if " is newer than the catalogue;" not in note),
        *list(route.get("warnings") or []),
    ]))
    applied_network = network
    if adapter == "codex":
        applied_network = (
            os.environ.get("CF_DISPATCH_CODEX_NETWORK", "1") == "1"
            if network is None
            else network
        )
    elif network is not None:
        warnings.append("network control unsupported by " + adapter)
        applied_network = None
    if adapter == "codex" and sandbox == "full" and applied_network is False:
        warnings.append("network denial is unsupported with the full sandbox")
        applied_network = None
    capabilities = validate_capabilities(
        capabilities, adapter=adapter, mode=mode, sandbox=sandbox, network=applied_network,
    )
    if capabilities and adapter == "codex" and sandbox == "full":
        warnings.append("capabilities need no grant with sandbox full: the Codex lane runs unsandboxed")
        capabilities = []
    directories = list(dict.fromkeys(
        str((candidate if candidate.is_absolute() else Path(workspace_root) / candidate).resolve())
        for candidate in (Path(p).expanduser() for p in add_dirs)
    ))
    def safe_write_path(directory):
        candidate = Path(directory).resolve()
        if provider_confinement.credential_path(candidate) or Path.home().resolve().is_relative_to(candidate):
            warnings.append("credential or authentication add-dir dropped: " + directory)
        elif adapter == "codex" and git_common is not None and candidate.is_relative_to(git_common):
            warnings.append("Codex writer drops Git common directory add-dir: " + directory)
        else:
            return True
        return False

    directories = [directory for directory in directories if safe_write_path(directory)]
    shared_write_paths = (provider_confinement.codex_writer_shared_paths(cwd, safe_write_path)
                          if mode == "worktree_write" and adapter == "codex" else {})
    if any(not Path(p).is_dir() for p in directories):
        raise ValueError("add-dir must be a readable directory")
    if mode == "worktree_write" and git_private is not None:
        # execute() checks every writer's .agents against its start and integration branch.
        # Read before launch from the caller's workspace, which the lane cannot rewrite.
        metadata["instruction_policy"], policy_warnings = fabric_policy.instruction_changes(workspace_root)
        warnings.extend(policy_warnings)
    if mode == "worktree_write" and adapter == "codex" and git_private is not None:
        # Codex's workspace-write sandbox keeps each writable root's .agents read-only, even an
        # absent one, so a rebase or merge that updates or adds a tracked skill stops half-way.
        # Granting the directory (execute() creates an absent one) lets Git rewrite it.
        instructions = Path(cwd, INSTRUCTION_DIR)
        grantable = not os.path.lexists(instructions) or (instructions.is_dir() and not instructions.is_symlink())
        if grantable and str(instructions) not in directories:
            directories.append(str(instructions))
    model = route.get("resolved_model") or route.get("model") or ""
    effort = route.get("effort_applied", route.get("effort")) or ""
    if effort == "default":
        effort = ""
    # A model id that carries the effort (cursor grok-4.7-high) sends it without a flag.
    if effort and config.EFFORT_FLAG is None and not model.casefold().endswith("-" + effort.casefold()):
        warnings.append(f"{adapter} does not expose effort control; requested {effort}")
        effort = ""
    route_label = adapter + "/" + model + ("@" + effort if effort else "")
    if "\x00" in prompt:
        raise ValueError("prompt contains NUL")
    if preface:
        prompt = (
            f"You are {route_label} via Fabric. Attribute work to exactly this route; never guess a model name. "
            "This is a headless run that ends when you reply: run commands in the foreground and finish before answering; processes left running are then stopped. Keep tool output bounded: redirect large output to a file and read slices.\n\n"
            + prompt
        )
    guarantee = (
        "enforced"
        if adapter in {"claude", "codex", "cursor"}
        else "best_effort"
        if mode == "worktree_write" or adapter == "opencode"
        else "prompt_only"
    )
    if adapter == "kiro" and mode == "read_only":
        guarantee = config.read_only_guarantee(route)
    if sandbox == "full" or (
        mode == "worktree_write" and adapter in {"claude", "cursor"}
    ):
        guarantee = "best_effort"
    confinement = "none"
    confinement_requested = bool(guarded) or adapter != "codex" or bool(capabilities)
    if guarded and adapter == "codex" and not capabilities:
        raise ValueError("protected paths require OS read confinement; fix: use a non-training route")
    if guarded and not provider_confinement._sandbox_exec_path():
        raise ValueError("protected paths require sandbox-exec; fix: use a non-training route")
    if confinement_requested and provider_confinement._sandbox_exec_path():
        confinement = "sandbox-exec"
        if adapter == "agy":
            guarantee = "best_effort"
        if adapter == "kiro":
            _require_kiro_engine()
    elif confinement_requested:
        if mode == "worktree_write":
            warnings.append("worktree_write writes are unconfined: sandbox-exec unavailable or unusable")
        else:
            warnings.append(f"{adapter} read_only writes are unconfined: sandbox-exec unavailable or unusable")
    if mode == "read_only" and adapter == "codex":
        confinement = "provider-native"
    if mode == "worktree_write" and adapter == "codex" and sandbox == "workspace-write" and not capabilities:
        confinement = "provider-native"
    elif mode == "worktree_write" and adapter == "codex" and confinement != "sandbox-exec":
        warnings.append("worktree_write writes are unconfined: Codex sandbox is full")
    if mode == "worktree_write" and adapter == "agy" and confinement != "sandbox-exec":
        raise ValueError("agy worktree_write requires usable sandbox-exec")
    # Codex's native read-only sandbox reads everywhere whatever the cwd, so it gets no warning.
    if (
        mode == "read_only"
        and cwd != workspace_root
        and adapter in {"claude", "cursor", "kiro", "agy", "opencode"}
        and confinement != "sandbox-exec"
    ):
        warnings.append(f"{adapter} read_only: cwd is not a read boundary")
    if adapter == "codex" and sandbox == "read-only":
        writable = config.read_only_writable_dirs({"cwd": cwd, "applied": {"add_dirs": directories}})
        warnings.extend("codex read_only add-dir stays read-only (it holds cwd or a pattern character): "
                        + directory
                        for directory in directories if directory not in writable)
    if adapter in {"agy", "kiro"} or (
        mode == "worktree_write" and guarantee != "enforced"
    ):
        warnings.append(f"{adapter} {mode} guarantee={guarantee}")
    if adapter in {"cursor", "kiro", "copilot", "opencode"} and directories and not (
        mode == "worktree_write" and confinement == "sandbox-exec"
    ):
        warnings.append("additional directories unsupported by " + adapter)
        directories = []
    locks_dir = None
    if mode == "worktree_write" and git_private is not None:
        # Recomputed for every attempt, resume included, and kept out of add_dirs.
        locks_dir = _ensure_locks_dir(layout.run_root(cwd))
        if locks_dir is None:
            warnings.append("project locks directory is not grantable: a link or non-directory is in the way")
    attempt_dir = Path(run_dir or cwd).expanduser().resolve()
    browser_tmp = str(provider_confinement._fabric_hosts_owner().browser_temporary_directory(attempt_dir)) if "browser" in capabilities else None
    codex_home = attempt_dir.parent / "codex-home" if capabilities and adapter == "codex" else None
    codex_auth_path = None
    if codex_home is not None:
        source_codex_home = Path(os.environ.get("CODEX_HOME") or Path.home() / ".codex").expanduser().resolve()
        codex_auth_path = source_codex_home / "auth.json"
    if confinement == "sandbox-exec":
        writable_paths = [str(attempt_dir), *([browser_tmp] if browser_tmp else []), "/dev"]
        if mode == "worktree_write":
            writable_paths = [cwd, *directories, *shared_write_paths, *([locks_dir] if locks_dir else []), *writable_paths, *git_writes]
        if codex_home is not None:
            writable_paths.extend((str(codex_home), str(codex_auth_path)))
        write_boundary = {"kind": "sandbox-exec", "writable_paths": writable_paths}
    elif confinement == "provider-native":
        write_boundary = {"kind": "provider-native", "sandbox": sandbox}
        if adapter == "codex":
            write_boundary["profile"] = "provenant-" + uuid.uuid4().hex
        if mode == "worktree_write" and sandbox == "workspace-write":
            # Codex applies the nearest entry, so the common directory reads as read-only below
            # any add_dir while the named Git paths inside it stay writable.
            # :tmpdir is the attempt's tmp, which holds XDG_CACHE_HOME, COREPACK_HOME and tool caches.
            filesystem = {":tmpdir": "write", **dict.fromkeys([*directories, *shared_write_paths], "write"),
                          **({locks_dir: "write"} if locks_dir else {})}
            if git_private is not None:
                filesystem[str(git_common)] = "read"
                filesystem.update(dict.fromkeys(git_writes, "write"))
                for path in writer_read_only(cwd, git_private, git_common):
                    filesystem[str(path)] = "read"
            write_boundary["filesystem"] = filesystem
    else:
        write_boundary = {"kind": "none", "writable_paths": None}
    applied_sandbox = (
        sandbox
        if adapter == "codex"
        else "read-only"
        if mode == "read_only" and adapter in {"claude", "cursor"}
        else None
    )
    if sandbox == "full" and adapter != "codex":
        warnings.append("sandbox control unsupported by " + adapter)
    timeout = float(timeout_seconds or (10800 if mode == "worktree_write" else 3600))
    startup = float(
        startup_seconds
        or os.environ.get("CF_DISPATCH_STARTUP_SECONDS")
        or STARTUP_SECONDS
    )
    if not all(math.isfinite(value) and value > 0 for value in (timeout, startup)):
        raise ValueError("timeouts must be finite positive numbers")
    boundary = f"Workspace root: {cwd}\nResolve relative paths against this root. " + (
        "Write, run commands and commit only inside this owned worktree. Do not push or change other checkouts. "
        + ("" if metadata.get("instruction_policy") == "allow" else
           "Do not author changes under .agents/; stage proposed instruction edits elsewhere for chair review. ")
        + "Protected instruction changes or unverifiable checks quarantine a completed result: review it before landing."
        if mode == "worktree_write"
        else "Do not modify files or run commands that mutate state. Use file-reading tools only."
    )
    source = Path(original_prompt_file).expanduser() if original_prompt_file is not None else None
    original_path = str((source if source.is_absolute() else Path(workspace_root) / source).resolve()) if source is not None else None
    plan = {
        "schema": "fabric.exec-plan.v1",
        "adapter": adapter,
        "route": route,
        "model": model,
        "effort": effort,
        "prompt": prompt,
        "network_requested": network,
        "cwd": cwd,
        "workspace_root": workspace_root,
        "protected_paths": [str(path) for path in guarded],
        "mode": mode,
        "worktree": str(worktree) if worktree else None,
        "run_dir": str(attempt_dir),
        "original_prompt_file": original_path,
        "timeout_seconds": timeout,
        "startup_seconds": startup,
        "grace_seconds": 5.0,
        "session_id": session_id
        or (str(uuid.uuid4()) if adapter == "claude" else None),
        "resume_session": resume_session,
        "stdin_policy": config.STDIN,
        "output_format": config.OUTPUT_FORMAT,
        "run_id": run_id,
        "chair": chair,
        "route_label": route_label,
        "warnings": warnings,
        "boundary_prompt": boundary,
        "requested_model": requested_model,
        "requested_effort": requested_effort,
        "intent": intent,
        "applied": {
            "sandbox": applied_sandbox,
            "network": applied_network,
            "add_dirs": directories,
            "shared_write_paths": shared_write_paths,
            "locks_dir": locks_dir,
            "guarantee": guarantee,
            "confinement": confinement,
            "write_boundary": write_boundary,
            "capabilities": capabilities,
            **({"browser_tmpdir": browser_tmp} if browser_tmp else {}),
            "protected_paths": [str(path) for path in guarded],
        },
        **({"codex_home": str(codex_home), "codex_auth_path": str(codex_auth_path)}
           if codex_home is not None else {}),
        "agy_sandbox": intent == "assurance"
        or os.environ.get("CF_DISPATCH_AGY_SANDBOX", "0") == "1",
        **metadata,
    }
    if confinement == "sandbox-exec":
        state = provider_confinement._provider_state(plan)
        write_boundary["writable_paths"].extend(
            description for _, description in provider_confinement._state_write_grants(Path.home().resolve(), state))
    context_usage.apply_ceiling(plan, context_ceiling)
    if hasattr(config, "config_policy"):
        plan["applied"]["config"] = config.config_policy(plan)
    plan["argv"] = config.argv(plan)
    if config.PROMPT_TRANSPORT == "argv":
        ceiling = int(os.environ.get("CF_DISPATCH_ARGV_PROMPT_MAX_BYTES", "65536"))
        if any(len(arg.encode()) > ceiling for arg in plan["argv"]):
            raise ValueError(
                "prompt_too_large: argv prompt exceeds the single-argument ceiling"
            )
    return plan




# First matching diagnostic wins. Adapter rules keep their former priority,
# including the two permission patterns on either side of auth/rate limits.




















class BoundedCapture:
    """Keep a fixed head and rolling tail on disk; never discard the diagnostics file."""

    def __init__(self, path, limit=None):
        limit = provider_observer.MAX_EVENTS_BYTES if limit is None else limit
        self.path, self.limit = Path(path), limit
        parent, leaf = __import__("output_custody").open_parent(str(path))
        try:
            fd = os.open(
                leaf,
                os.O_RDWR | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0),
                0o600,
                dir_fd=parent,
            )
        finally:
            os.close(parent)
        self.file = os.fdopen(fd, "w+b", buffering=0)
        self.total = 0
        self.write_error = None
        self.tail = deque()
        self.tail_size = 0

    def write(self, data):
        # Append to disk only until the cap. Keep subsequent tail chunks in a
        # deque: every byte is copied a bounded number of times, even for JSONL.
        room = max(0, self.limit - self.total)
        if data and self.write_error is None:
            try:
                if room:
                    self.file.write(data[:room])
                else:
                    # Retained bytes are capped; the timestamp still tracks provider output.
                    os.utime(self.file.fileno(), None)
            except OSError as exc:
                self.write_error = exc
        self.total += len(data)
        half = self.limit - self.limit // 2
        self.tail.append(data[-half:])
        self.tail_size += len(self.tail[-1])
        while self.tail_size > half:
            excess = self.tail_size - half
            first = self.tail.popleft()
            if len(first) > excess:
                self.tail.appendleft(first[excess:])
                self.tail_size -= excess
            else:
                self.tail_size -= len(first)

    def _flush_tail(self):
        if self.total > self.limit and self.write_error is None:
            try:
                self.file.seek(self.limit // 2)
                self.file.write(b"".join(self.tail))
                self.file.truncate(self.limit)
            except OSError as exc:
                self.write_error = exc

    def text(self):
        self._flush_tail()
        if self.write_error is None:
            try:
                self.file.seek(0)
                return self.file.read().decode("utf-8", errors="replace")
            except OSError as exc:
                self.write_error = exc
        return b"".join(self.tail).decode("utf-8", errors="replace")

    def close(self):
        self._flush_tail()
        try:
            self.file.close()
        except OSError as exc:
            self.write_error = exc


from process_info import (
    _ProcessRow, _DarwinBsdInfo, _darwin_libproc, _linux_process_row,
    _linux_tree_snapshot, _darwin_process_row, _probe_process_row,
)


def _process_snapshot():
    try:
        return _process_snapshot_unchecked()
    except Exception:
        return None


def _process_snapshot_unchecked():
    if sys.platform == "darwin":
        libproc = _darwin_libproc()
        if libproc is None:
            return {}
        count = libproc.proc_listallpids(None, 0)
        if count <= 0:
            return {}
        pids = (ctypes.c_int * (count + 128))()
        count = libproc.proc_listallpids(pids, ctypes.sizeof(pids))
        rows = {}
        for pid in pids[:max(0, count)]:
            if pid > 0 and (row := _darwin_process_row(libproc, pid)) is not None:
                rows[pid] = row
        return rows
    if not sys.platform.startswith("linux"):
        return {}
    rows = {}
    try:
        pids = os.scandir("/proc")
    except OSError:
        return rows
    with pids:
        for entry in pids:
            if not entry.name.isdecimal():
                continue
            pid = int(entry.name)
            try:
                row = _linux_process_row(pid, Path(entry.path).parent)
            except OSError:
                continue
            if row is not None:
                rows[pid] = row
    return rows


def _process_environment(pid):
    if sys.platform.startswith("linux"):
        try:
            with Path(f"/proc/{pid}/environ").open("rb") as stream:
                return stream.read(1024 * 1024).split(b"\0")
        except OSError:
            return ()
    if sys.platform != "darwin":
        return ()
    libc = ctypes.CDLL(None, use_errno=True)
    mib = (ctypes.c_int * 3)(1, 49, pid)  # CTL_KERN, KERN_PROCARGS2, pid
    size = ctypes.c_size_t()
    if libc.sysctl(mib, 3, None, ctypes.byref(size), None, 0) != 0 or size.value > 1024 * 1024:
        return ()
    data = ctypes.create_string_buffer(size.value)
    if libc.sysctl(mib, 3, data, ctypes.byref(size), None, 0) != 0:
        return ()
    # KERN_PROCARGS2 stores argc, executable path, argv, then NUL-separated env.
    raw = data.raw[:size.value]
    if len(raw) < ctypes.sizeof(ctypes.c_int):
        return ()
    argc = ctypes.c_int.from_buffer_copy(raw).value
    if not 0 <= argc <= 65536:
        return ()
    offset = raw.find(b"\0", ctypes.sizeof(ctypes.c_int))
    if offset < 0:
        return ()
    while offset < len(raw) and raw[offset] == 0:
        offset += 1
    for _ in range(argc):
        offset = raw.find(b"\0", offset)
        if offset < 0:
            return ()
        offset += 1
    return raw[offset:].split(b"\0")


def _has_attempt_marker(pid, marker):
    return ("PROVENANT_ATTEMPT_MARKER=" + marker).encode() in _process_environment(pid)


_linux_boot_time = process_info._linux_boot_time


def _recorded_start_time(row):
    """Match the seconds-resolution `ps -o lstart=` used by run-registry.ts."""
    if sys.platform == "darwin":
        epoch = float(row.started)
    elif sys.platform.startswith("linux"):
        epoch = _linux_boot_time() + int(row.started) / os.sysconf("SC_CLK_TCK")
    else:
        return None
    return process_info._lstart(epoch)


def _ps_start_time(pid, *, canonical):
    if canonical and (observed := process_info.process(pid)) is not None:
        return observed.lstart
    try:
        result = subprocess.run(
            ["/bin/ps", "-o", "lstart=", "-p", str(pid)],
            capture_output=True, text=True, timeout=2, check=False,
            env={**os.environ, **({"LC_ALL": "C", "LANG": "C"} if canonical else {})},
        )
        return result.stdout.strip() if result.returncode == 0 else None
    except (OSError, subprocess.SubprocessError):
        return None


def _pid_exists(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def _signal_name(signum):
    try:
        return signal.Signals(signum).name
    except ValueError:
        return f"SIG{signum}"  # Intermediate realtime signals have no enum member.


def termination_request_matches(request, *, run_token, provider_pid, started_at, signals):
    """Bind controller intent and time; None opts out of provider PID binding."""
    return (isinstance(request, dict) and bool(run_token) and request.get("run_token") == run_token
            and (provider_pid is None or request.get("provider_pid") == provider_pid)
            and request.get("signal") in signals
            and request.get("source") in {"run_control", "orphan_reaper", "cancel_requested"}
            and isinstance(started_at, str) and isinstance(request.get("requested_at"), str)
            and started_at <= request["requested_at"] <= now()
            and type(request.get("sender_pid")) is int and request["sender_pid"] > 1)


def _termination_request(environment, process, exit_code, started_at, supervisor_signal=None):
    """Optional controller telemetry; waitpid itself never exposes the sender."""
    directory, token = environment.get("PROVENANT_RUN_DIR"), environment.get("PROVENANT_RUN_TOKEN")
    signum = -exit_code if exit_code and exit_code < 0 else supervisor_signal
    if not directory or not token or not process or not signum:
        return {}
    try:
        path = Path(directory, "termination-request.json")
        descriptor = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
        with os.fdopen(descriptor, "rb") as stream:
            if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
                return {}
            request = json.loads(stream.read(65537))
        if termination_request_matches(request, run_token=token, provider_pid=process.pid,
                                       started_at=started_at, signals={_signal_name(signum)}):
            return {"termination_source": request["source"], "signal_requester_pid": request["sender_pid"],
                    "signal_requested_at": request["requested_at"]}
    except (OSError, ValueError):
        pass
    return {}


def _nested_fabric_owner_token(row):
    try:
        values = {}
        for entry in _process_environment(row.pid):
            key, _, value = entry.partition(b"=")
            if key in {b"PROVENANT_RUN_DIR", b"PROVENANT_RUN_TOKEN"}:
                values[key] = value
        token = values.get(b"PROVENANT_RUN_TOKEN")
        directory = values.get(b"PROVENANT_RUN_DIR")
        if not token or not directory:
            return None
        run_dir = Path(os.fsdecode(directory))
        if not run_dir.is_absolute():
            return None
        path = run_dir / "dispatch-owner.json"
        if path.is_symlink() or not path.is_file() or path.stat().st_size > 65536:
            return None
        record = json.loads(path.read_text())
        started_at = record.get("owner_started_at")
        valid = (
            record.get("schema_version") == 1
            and record.get("kind") in {"dispatch", "batch"}
            and Path(record.get("run_dir", "")).resolve() == run_dir.resolve()
            and record.get("run_token") == os.fsdecode(token)
            and record.get("owner_pid") == row.pid
            and row.pgid == row.pid
            and record.get("owner_pgid") == row.pid
            and isinstance(started_at, str) and bool(started_at)
            and (
                started_at == _recorded_start_time(row)
                or started_at == _ps_start_time(row.pid, canonical=True)
                or started_at == _ps_start_time(row.pid, canonical=False)
            )
        )
        return os.fsdecode(token) if valid else None
    except Exception:
        return None


class _Descendants:
    def __init__(self, process, marker):
        self.process = process
        self.marker = marker
        self.root = None
        self.tracked = {}
        self.spared = {}
        self.verified_owners = set()
        self.verified_owner_tokens = {}
        self.shared_hosts = set()
        self.parents = {}
        self.spared_at_stop = set()
        self.snapshot_unavailable = False

    def sample(self, include_reparented=False):
        try:
            return self._sample(include_reparented)
        except Exception:
            self.snapshot_unavailable = True
            return {}

    def _sample(self, include_reparented):
        rows = None
        targeted = False
        if sys.platform.startswith("linux") and not include_reparented:
            rows = _linux_tree_snapshot(
                self.process.pid, {pid for pid, _ in (*self.tracked, *self.spared)}
            )
            targeted = rows is not None
        if rows is None:
            rows = _process_snapshot()
        if rows is None:
            self.snapshot_unavailable = True
            return {}
        if ((targeted and self.process.poll() is None and self.process.pid not in rows)
                or (not targeted and os.getpid() not in rows)):
            self.snapshot_unavailable = True
        root = rows.get(self.process.pid)
        if self.root is None and root is not None and self.process.poll() is None:
            self.root = root.identity
        parents = {self.root} if self.root else set()
        parents.update(self.tracked)
        parents.update(self.spared)
        while True:
            newly = {
                row.identity: row for row in rows.values()
                if row.identity not in parents
                and (parent := rows.get(row.ppid)) is not None
                and parent.identity in parents
            }
            if not newly:
                break
            for identity, row in newly.items():
                self.parents[identity] = rows[row.ppid].identity
                self.tracked[identity] = row
            parents.update(newly)
        # Inherited environment is not ancestry: a shared code-mode host can
        # retain a lane's marker after reparenting and serve sibling lanes.
        # Only identities observed below our root remain cleanup targets.
        self._refresh_spared(rows)
        return rows

    def _refresh_spared(self, rows):
        observed = {**self.tracked, **self.spared}
        # A row missing from one census is not evidence of death: probe that pid
        # alone, and when even that cannot be read, keep a verified owner spared
        # while its pid exists.
        def held(identity):
            row = rows.get(identity[0]) or _probe_process_row(identity[0])
            if row is not None:
                return row.identity == identity and not row.zombie
            return _pid_exists(identity[0])

        # A code-mode host may have been spawned here and later serve sibling
        # lanes. Its ancestry never grants this attempt exclusive custody.
        # Linux's comm field truncates the same executable name to 15 bytes.
        self.shared_hosts.update(
            identity for identity in observed
            if (row := rows.get(identity[0])) is not None and row.identity == identity
            and row.command in {"codex-code-mode-host", "codex-code-mode"}
        )
        def exclusively_observed(identity):
            while identity != self.root:
                if identity in self.shared_hosts or identity not in self.parents:
                    return False
                identity = self.parents[identity]
            return True

        self.verified_owner_tokens = {identity: token for identity, token in self.verified_owner_tokens.items()
                                      if exclusively_observed(identity)}
        spared = {identity for identity in self.verified_owners | self.shared_hosts if held(identity)}
        own_groups = {self.process.pid, os.getpgrp()}
        for identity in observed:
            if identity in self.verified_owners or identity in self.shared_hosts:
                continue
            row = rows.get(identity[0])
            parent = self.parents.get(identity)
            if (row is not None and row.identity == identity and not row.zombie
                    and row.pgid not in own_groups
                    and parent is not None and (parent == self.root or parent in observed)
                    and parent not in self.spared):
                if token := _nested_fabric_owner_token(row):
                    self.verified_owners.add(identity)
                    if exclusively_observed(identity):
                        self.verified_owner_tokens[identity] = token
                    spared.add(identity)
        changed = True
        while changed:
            changed = False
            for identity, parent in self.parents.items():
                if identity in observed and parent in spared and identity not in spared:
                    spared.add(identity)
                    changed = True
        self.spared = {identity: row for identity, row in observed.items() if identity in spared}
        self.tracked = {identity: row for identity, row in observed.items() if identity not in spared}

    def live(self, rows):
        return {
            identity: row for identity in (set(self.tracked) | ({self.root} if self.root else set()))
            if (row := rows.get(identity[0])) is not None
            and row.identity == identity and not row.zombie
        }

    def live_spared(self, rows):
        return {
            identity: row for identity in self.spared
            if (row := rows.get(identity[0])) is not None
            and row.identity == identity and not row.zombie
        }

    def signal(self, signum, *, root_group=True, skip=frozenset(), only=None):
        try:
            rows = _process_snapshot()
        except Exception:
            rows = None
        if rows is None:
            self.snapshot_unavailable = True
            rows = {}
        else:
            self._refresh_spared(rows)  # A fork observed before exec may now be a recorded owner.
        live = self.live(rows)
        live = {
            identity: row for identity, row in live.items()
            if identity not in skip and (only is None or identity in only)
        }
        spared_groups = {
            row.pgid for row in self.live_spared(rows).values()
        } | {identity[0] for identity in self.spared if identity in self.verified_owners}
        spared_groups -= {self.process.pid, os.getpgrp()}
        # A shared host inside the original group requires PID-only cleanup,
        # including the provider itself, so even that group cannot hit the host.
        if self.shared_hosts.intersection(self.spared):
            # A host can change groups between censuses. With a shared host
            # still held, use PID-only cleanup even after a partial census.
            spared_groups.add(self.process.pid)
        if not root_group:
            live.pop(self.root, None)
        # A descendant can join a shared or sibling process group. Its ancestry
        # authorises its PID, never every member of that other group.
        groups = set()
        root = rows.get(self.process.pid)
        if (root_group and self.process.pid != os.getpgrp()
                and self.process.pid not in spared_groups
                and (root is None or self.root is None or root.identity == self.root)):
            groups.add(self.process.pid)  # The original group may outlive its leader.
        signalled_groups = set()
        for pgid in groups:
            try:
                os.killpg(pgid, signum)
                signalled_groups.add(pgid)
            except (ProcessLookupError, PermissionError):
                pass
        for row in live.values():
            if signum == signal.SIGTERM and row.pgid in signalled_groups:
                continue
            try:
                os.kill(row.pid, signum)
            except (ProcessLookupError, PermissionError):
                pass
        if root_group and self.process.poll() is None and (
            signum != signal.SIGTERM or self.process.pid not in signalled_groups
        ):
            try:
                os.kill(self.process.pid, signum)
            except (ProcessLookupError, PermissionError):
                pass

    def stop(self, *, normal=False, terminal_grace=False,
             descendant_grace=0.5, root_grace=2.0):
        if normal:
            settle_deadline = time.monotonic() + 1.5
            while time.monotonic() < settle_deadline:
                rows = self.sample(include_reparented=True)
                if not any(identity != self.root for identity in self.live(rows)):
                    break
                time.sleep(0.05)
        rows = self.sample(include_reparented=True)
        self.spared_at_stop.update(self.live_spared(rows))
        pending = {
            identity for identity, row in self.live(rows).items()
            if terminal_grace and row.ppid == self.process.pid
        }
        deferred = {
            identity for identity, row in self.live(rows).items()
            if identity in pending and row.pgid != self.process.pid
        }
        leftovers = {
            identity: {"pid": row.pid, "command": row.command[:80]}
            for identity, row in self.live(rows).items()
            if identity != self.root and identity not in pending
        }
        self.signal(signal.SIGTERM, skip=deferred)
        started = time.monotonic()
        deadline = started + root_grace
        descendants_killed = False
        deferred_killed = False
        while time.monotonic() < deadline:
            self.process.poll()
            rows = self.sample(include_reparented=True)
            self.spared_at_stop.update(self.live_spared(rows))
            leftovers.update({
                identity: {"pid": row.pid, "command": row.command[:80]}
                for identity, row in self.live(rows).items() if identity != self.root
                and (identity not in pending or time.monotonic() - started >= descendant_grace)
            })
            if not descendants_killed and time.monotonic() - started >= descendant_grace:
                if deferred:
                    self.signal(signal.SIGTERM, root_group=False, only=deferred)
                self.signal(signal.SIGKILL, root_group=False, skip=deferred)
                descendants_killed = True
            if deferred and not deferred_killed and time.monotonic() - started >= 2 * descendant_grace:
                self.signal(signal.SIGKILL, root_group=False, only=deferred)
                deferred_killed = True
            if self.process.poll() is not None and not self.live(rows):
                break
            time.sleep(0.05)
        rows = self.sample(include_reparented=True)
        self.spared_at_stop.update(self.live_spared(rows))
        leftovers.update({
            identity: {"pid": row.pid, "command": row.command[:80]}
            for identity, row in self.live(rows).items() if identity != self.root
        })
        self.signal(signal.SIGKILL)
        self.spared_at_stop.update(self.spared)
        try:
            self.process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            pass
        if sys.platform.startswith("linux") and self.tracked:
            deadline = time.monotonic() + 2
            while True:
                rows = self.sample(include_reparented=True)
                remaining = []
                for identity in self.tracked:
                    row = rows.get(identity[0])
                    if row is None or row.identity != identity:
                        continue
                    try:
                        os.waitpid(row.pid, os.WNOHANG)
                    except ChildProcessError:
                        pass
                    remaining.append(identity)
                if not remaining or time.monotonic() >= deadline:
                    break
                time.sleep(0.02)
        return [item for identity, item in leftovers.items()
                if identity not in self.spared_at_stop]


_SUBREAPER_LOCK = threading.Lock()
_SUBREAPER = {"users": 0, "previous": 0}


def _prctl(option, argument):
    libc = ctypes.CDLL(None, use_errno=True)
    return libc.prctl(option, argument, 0, 0, 0)


def _enable_subreaper():
    """Adopt orphaned descendants for this attempt only; True when held."""
    if not sys.platform.startswith("linux"):
        return False
    with _SUBREAPER_LOCK:
        if _SUBREAPER["users"] == 0:
            try:
                previous = ctypes.c_int(0)
                _prctl(37, ctypes.byref(previous))  # PR_GET_CHILD_SUBREAPER
                _prctl(36, 1)  # PR_SET_CHILD_SUBREAPER
            except (AttributeError, OSError):
                return False
            _SUBREAPER["previous"] = previous.value
        _SUBREAPER["users"] += 1
    return True


def _release_subreaper():
    """Restore the host's own setting once no attempt in this process needs it."""
    with _SUBREAPER_LOCK:
        if _SUBREAPER["users"] == 0:
            return
        _SUBREAPER["users"] -= 1
        if _SUBREAPER["users"] == 0 and not _SUBREAPER["previous"]:
            try:
                _prctl(36, 0)
            except (AttributeError, OSError):
                pass


class WorkspaceProgress:
    """Rotate a bounded filesystem scan across writer watchdog samples."""

    IGNORED = {".git", ".agent-run", ".worktrees", "node_modules", ".venv", "__pycache__"}

    def __init__(self, cwd, excluded=()):
        self.cwd = Path(cwd)
        self.excluded = {os.fspath(path) for path in excluded}
        self.started_wall_ns = time.time_ns() - 20_000_000
        self.known = {}
        self.initial_pass_complete = False
        self.last_visited = 0
        self._stack = []
        self._seen = set()

    def _start_pass(self):
        self._seen = set()
        try:
            self._stack = [os.scandir(self.cwd)]
        except OSError:
            self._stack = []

    def probe(self):
        deadline = time.monotonic() + 0.025
        self.last_visited = 0
        if not self._stack:
            self._start_pass()
        changed = False
        while (self._stack and self.last_visited < 2000
               and (self.last_visited < 32 or time.monotonic() < deadline)):
            try:
                entry = next(self._stack[-1])
            except StopIteration:
                self._stack.pop().close()
                continue
            except OSError:
                self._stack.pop().close()
                continue
            self.last_visited += 1
            path = entry.path
            try:
                if entry.is_dir(follow_symlinks=False):
                    if entry.name not in self.IGNORED:
                        self._stack.append(os.scandir(entry.path))
                    continue
                if path in self.excluded:
                    continue
                metadata = entry.stat(follow_symlinks=False)
            except OSError:
                continue
            signature = (metadata.st_mtime_ns, metadata.st_size)
            previous = self.known.get(path)
            if previous is not None and previous != signature:
                changed = True
            elif previous is None and (self.initial_pass_complete
                                       or metadata.st_mtime_ns > self.started_wall_ns + 20_000_000
                                       or (metadata.st_mtime_ns < self.started_wall_ns
                                           and metadata.st_ctime_ns > self.started_wall_ns)):
                changed = True
            self.known[path] = signature
            self._seen.add(path)
        if not self._stack:
            if self.initial_pass_complete:
                missing = self.known.keys() - self._seen
                if missing:
                    changed = True
                    for path in missing:
                        del self.known[path]
            self.initial_pass_complete = True
        return changed

    def close(self):
        while self._stack:
            self._stack.pop().close()


def _cpu_stamp(pgid):
    try:
        return tuple(row.cpu for row in sorted(process_info.processes(), key=lambda row: row.pid)
                     if row.pgid == pgid)
    except (OSError, ValueError):
        return None






def _prepare_codex_state_dir(plan):
    """Create the Provenant state directory the plan granted, now rather than while planning."""
    state_dir = plan.get("applied", {}).get("write_boundary", {}).get("state_dir")
    if plan["adapter"] == "codex" and state_dir:
        try:
            Path(state_dir).mkdir(parents=True, exist_ok=True, mode=0o700)
        except OSError:
            pass


def _prepare_codex_capability_home(plan, environment):
    capabilities = plan.get("applied", {}).get("capabilities", [])
    if plan.get("adapter") != "codex" or not capabilities:
        return
    configured_home = Path(os.path.abspath(Path(environment.get("CODEX_HOME") or Path.home() / ".codex").expanduser()))
    source_home = configured_home.resolve()
    lane_home = Path(plan["run_dir"]).parent / "codex-home"
    old_auth = plan.get("codex_auth_path")
    boundary = plan.get("applied", {}).get("write_boundary", {}).get("writable_paths")
    # A source home the lane can write, or reach through a symlink, could be swapped before the next attempt.
    if configured_home != source_home:
        raise PermissionError(f"codex home must not pass through a symlink: {configured_home}")
    for root in [plan.get("cwd"), *plan.get("applied", {}).get("add_dirs", []), plan["run_dir"],
                 str(lane_home), *(boundary if isinstance(boundary, list) else [])]:
        if root and root != old_auth and source_home.is_relative_to(Path(root).expanduser().resolve()):
            raise PermissionError(f"codex home lies inside a lane-writable path: {source_home}")
    try:
        lane_mode = lane_home.lstat().st_mode
    except FileNotFoundError:
        lane_home.mkdir(parents=True, exist_ok=True, mode=0o700)
    else:
        if stat.S_ISLNK(lane_mode) or not stat.S_ISDIR(lane_mode):
            raise NotADirectoryError(f"codex capability home is not a directory: {lane_home}")
    for name in ("auth.json", "AGENTS.md", "HARNESS.md", "skills"):
        source = source_home / name
        target = lane_home / name
        try:
            target_mode = target.lstat().st_mode
        except FileNotFoundError:
            pass
        else:
            if stat.S_ISDIR(target_mode):
                shutil.rmtree(target)
            else:
                target.unlink()
        if source.exists():
            target.symlink_to(source, target_is_directory=source.is_dir())
    # The lane may write this one file, so a symlink planted there must not redirect the grant.
    auth_path = source_home / "auth.json"
    if auth_path.is_symlink() or (auth_path.exists() and not auth_path.is_file()):
        raise PermissionError(f"codex auth store is not a regular file: {auth_path}")
    plan["codex_home"] = str(lane_home)
    plan["codex_auth_path"] = str(auth_path)
    if isinstance(boundary, list):
        if old_auth in boundary:
            boundary[boundary.index(old_auth)] = str(auth_path)
        elif str(auth_path) not in boundary:
            boundary.append(str(auth_path))
        if str(lane_home) not in boundary:
            boundary.append(str(lane_home))
    environment["CODEX_HOME"] = str(lane_home)
    if "browser" in capabilities:
        environment["MAC_CHROMIUM_TMPDIR"] = plan["applied"]["browser_tmpdir"]
    else:
        environment.pop("MAC_CHROMIUM_TMPDIR", None)


def _git_output(cwd, *args):
    # Only object and index reads run here, never a working-tree scan: the lane can write the
    # repository's config and attributes, so filters, fsmonitor hooks and a lazy fetch through a
    # configured transport command must not fire.
    environment = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    result = subprocess.run(
        ["git", "-C", str(cwd), "-c", "core.fsmonitor=false", "-c", "protocol.allow=never", *args],
        capture_output=True, text=True, timeout=30, env={**environment, "GIT_NO_LAZY_FETCH": "1"},
    )
    if result.returncode != 0:
        raise ValueError(result.stderr.strip() or f"git {args[0]} failed")
    return result.stdout


def instructions_writable(plan):
    """Whether this Codex writer was granted its worktree's otherwise protected .agents."""
    return (plan.get("adapter") == "codex" and plan.get("mode") == "worktree_write"
            and str(Path(plan["cwd"], INSTRUCTION_DIR)) in plan["applied"]["add_dirs"])


def _instruction_path(path):
    # A tree may spell the directory in any case, and a case-insensitive checkout writes it to
    # .agents. The spelling is kept, so a variant is a new path rather than shadowing the original.
    # An entry named .agents itself (a link, file or submodule) counts as well.
    return path if unicodedata.normalize("NFC", path.partition("/")[0]).casefold() == INSTRUCTION_DIR else None


def _instruction_entries(cwd, listing, *, index=False):
    """Map each .agents path to (mode, object id) from ls-tree or ls-files --stage output."""
    entries, conflicts = {}, set()
    for record in _git_output(cwd, *listing).split("\0"):
        meta, _, path = record.partition("\t")
        key = _instruction_path(path)
        if not key:
            continue
        fields = meta.split()
        if index and fields[2] != "0":
            conflicts.add(key)
        entries[key] = (fields[0], fields[1] if index else fields[2])
    return entries, conflicts


def instruction_disk_state(cwd, object_format):
    """Hash every file under .agents, ignored or not, as Git would store it, without running Git.

    The walk works from directory descriptors and never follows a link, so a surviving lane
    process cannot redirect it by swapping a directory or file for a link, FIFO or device.
    """
    root = Path(cwd, INSTRUCTION_DIR)
    if not os.path.lexists(root):
        return {}  # Git removes the directory with its last file, as a merge may.
    if root.is_symlink() or not root.is_dir():
        raise ValueError(f"{INSTRUCTION_DIR} is no longer a directory")
    entries, total = {}, 0
    # fwalk silently skips a directory swapped for a link before it descends, so every real
    # directory seen must also be visited.
    expected, visited = {root}, set()

    def unreadable(error):
        raise error  # A directory the check cannot list could hide a change.

    for directory, subdirectories, files, directory_fd in os.fwalk(root, onerror=unreadable):
        visited.add(Path(directory))
        for name in [*files, *subdirectories]:
            status = os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
            if stat.S_ISDIR(status.st_mode):
                expected.add(Path(directory, name))
                continue
            digest = hashlib.new(object_format)
            if stat.S_ISLNK(status.st_mode):
                mode, data = "120000", os.fsencode(os.readlink(name, dir_fd=directory_fd))
                digest.update(b"blob %d\0" % len(data) + data)
            elif stat.S_ISREG(status.st_mode):
                mode = "100755" if status.st_mode & 0o100 else "100644"
                descriptor = os.open(name, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW, dir_fd=directory_fd)
                with os.fdopen(descriptor, "rb") as handle:
                    opened = os.fstat(handle.fileno())
                    if not stat.S_ISREG(opened.st_mode):
                        raise ValueError(f"{name} changed type during the check")
                    digest.update(b"blob %d\0" % opened.st_size)
                    read = 0
                    while chunk := handle.read(1 << 20):
                        read += len(chunk)
                        if total + read > INSTRUCTION_BYTES_LIMIT:
                            raise ValueError(f"{INSTRUCTION_DIR} exceeds {INSTRUCTION_BYTES_LIMIT} bytes")
                        digest.update(chunk)
                    if read != opened.st_size:
                        raise ValueError(f"{name} changed size during the check")
                    total += read
            else:
                # Never open a FIFO or device: it could block the check. Git cannot store one.
                relative = Path(directory, name).relative_to(root).as_posix()
                raise ValueError(f"{INSTRUCTION_DIR}/{relative} is not a file or link")
            relative = Path(directory, name).relative_to(root).as_posix()
            entries[f"{INSTRUCTION_DIR}/{relative}"] = (mode, digest.hexdigest())
    if expected - visited:
        raise ValueError(f"{INSTRUCTION_DIR} changed during the check")
    return entries


def instruction_snapshot(cwd):
    head = _git_output(cwd, "rev-parse", "--verify", "HEAD^{commit}").strip()
    object_format = _git_output(cwd, "rev-parse", "--show-object-format").strip()
    snapshot = {"head": head, "object_format": object_format,
                "tree": _instruction_entries(cwd, ("ls-tree", "-r", "-z", "--full-tree", head))[0],
                "disk": instruction_disk_state(cwd, object_format),
                "index": _instruction_entries(cwd, ("ls-files", "--stage", "-z"), index=True)[0]}
    # Store starting files Git does not already hold, so a quarantine can put them back.
    known = {entry[1] for view in ("tree", "index") for entry in snapshot[view].values()}
    for path, entry in snapshot["disk"].items():
        if entry[1] not in known and instruction_quarantine._store_blob(cwd, path) != entry:
            raise ValueError(f"{path} changed during the check")
    return snapshot


def _integration_refs(cwd):
    """The primary checkout's branch and its upstream, where set."""
    primary = _git_output(cwd, "worktree", "list", "--porcelain").split("\n\n")[0].splitlines()
    branch = next((line[len("branch "):] for line in primary if line.startswith("branch ")), "")
    upstream = _git_output(cwd, "for-each-ref", "--format=%(upstream)", branch).strip() if branch else ""
    return [ref for ref in (branch, upstream) if ref]


def authored_instruction_changes(cwd, start):
    """.agents paths the lane changed to content from neither its start nor the integration branch.

    HEAD, the index and the files on disk must each match, path by path, the attempt's starting
    state or the primary checkout's branch or its upstream, so a rebase or merge from that branch
    passes and changes the branch already carried are not re-reported. A lane can move local refs,
    so this catches an ordinary edit, not a forged integration branch.
    """
    trees = [_instruction_entries(cwd, ("ls-tree", "-r", "-z", "--full-tree", start["head"]))[0]]
    for ref in _integration_refs(cwd):
        try:
            trees.append(_instruction_entries(cwd, ("ls-tree", "-r", "-z", "--full-tree", ref))[0])
        except ValueError:
            pass  # An unset or unfetched ref cannot vouch for anything.
    baselines = [start["disk"], start["index"], *trees]
    index, conflicts = _instruction_entries(cwd, ("ls-files", "--stage", "-z"), index=True)
    views = [_instruction_entries(cwd, ("ls-tree", "-r", "-z", "--full-tree", "HEAD"))[0], index,
             instruction_disk_state(cwd, start["object_format"])]
    changed = set()
    for view in views:
        for key in set(view).union(*baselines):
            if all(view.get(key) != baseline.get(key) for baseline in baselines):
                changed.add(key)
    # No branch tracks a starting untracked file, so only the lane can have removed one.
    changed.update(key for key in start["disk"].keys() - start["tree"].keys() - start["index"].keys()
                   if key not in views[2])
    # A path the lane took in from an integration branch must keep that branch's version, so a
    # merge that discards it (git merge -s ours) is caught too.
    restore = instruction_quarantine._restore_views(cwd, start)
    for name, view in zip(("head", "index", "disk"), views):
        changed.update(key for key in restore["integrated"] if view.get(key) != restore[name].get(key))
    return sorted(key + (" (unresolved conflict)" if key in conflicts else "") for key in changed | conflicts)


def tool_cache_environment(plan, cache, inherited):
    """Point tool caches at the attempt cache, overriding inherited locations a sandbox denies.

    A read-only lane must still run a targeted test, so tools that would write a cache beside
    the sources it may not change are pointed at the cache or told to skip it.
    """
    environment = {"UV_CACHE_DIR": str(cache / "uv"), "npm_config_cache": str(cache / "npm")}
    if plan["mode"] == "read_only":
        addopts = inherited.get("PYTEST_ADDOPTS", "")
        environment.update(
            PYTEST_ADDOPTS=(addopts + " -p no:cacheprovider").strip(),
            PYTHONPYCACHEPREFIX=str(cache / "pycache"),
            RUFF_CACHE_DIR=str(cache / "ruff"),
            MYPY_CACHE_DIR=str(cache / "mypy"),
        )
    return environment


def _check_model(plan, env):
    """Check the cached CLI listing first, refreshing once on a miss."""
    if not plan["model"] or plan["model"].casefold() == "auto":
        return "skipped", "Provider chooses the model; no pinned slug to check"
    try:
        try:
            from . import exec_routing
        except ImportError:
            import exec_routing
        router = exec_routing._model_route_module()
        deadline = time.monotonic() + min(20, plan["timeout_seconds"])
        for refresh in (False, True):
            listing, code = router.model_route_probe.probe_capabilities(
                plan["adapter"], plan["argv"][0], listing_only=True, refresh=refresh,
                deadline=deadline, env=env, cache_failures=False,
            )
            if listing.get("status") == "unsupported_adapter":
                return "unsupported", "Adapter has no model listing"
            models = [model for model in listing.get("models", []) if isinstance(model, str) and model]
            if code or listing.get("refresh_failed") or not models:
                return "unavailable", listing.get("message") or "Model list unavailable"
            catalog = router.load_catalog()
            model_ids = {router.model_route_live._live_base(plan["adapter"], model, catalog).casefold() for model in models}
            if router.model_route_live._live_base(plan["adapter"], plan["model"], catalog).casefold() in model_ids:
                return "passed", ""
            if not listing.get("cache_hit"):
                break
        nearest = difflib.get_close_matches(plan["model"], models, n=3, cutoff=0)
        return "invalid", f"Unknown model {plan['model']!r}; nearest valid models: {', '.join(nearest)}"
    except Exception as exc:  # noqa: BLE001 - advisory discovery must never prevent a launch
        return "unavailable", type(exc).__name__


def execute(plan, output_path, **options):
    """Run exactly one attempt. The caller owns fallback, resume, and run publication."""
    tmp_fd = provider_confinement._open_attempt_tmp(plan["run_dir"])
    try:
        return _run_attempt(plan, output_path, tmp_fd, **options)
    finally:
        os.close(tmp_fd)


def attempt_environment(plan, env):
    """Prepare the provider environment from the owner environment."""
    control_environment = dict(os.environ if env is None else env)
    environment = dict(control_environment)
    for key in list(environment):
        if (key == "AGENT_FABRIC_STATE_DIRECTORY" and plan["adapter"] == "codex"
                and plan["mode"] == "worktree_write"):
            continue  # Writer coordination uses the same store whose database was granted.
        if key.startswith(
            ("GIT_", "PROVENANT_RUN_", "PROVENANT_PREFLIGHT_")
        ) or key in {
            "PROVENANT_FABRIC_PHASES",
            "PROVENANT_NO_OS_CONFINEMENT",
            "AGENT_FABRIC_STATE_DIRECTORY",
            "AGENT_FABRIC_SEAT",
            "AGENT_FABRIC_CLIENT_LABEL",
            "AGENT_FABRIC_LABEL",
            "AGENT_FABRIC_PRODUCT_ROOT",
        }:
            environment.pop(key, None)
    environment.update(
        PROVENANT_ROUTE=plan["route_label"],
        PROVENANT_RUN_ID=plan.get("run_id", ""),
        PROVENANT_CHAIR=plan.get("chair", ""),
        PWD=plan["cwd"],
    )
    environment.pop("PROVENANT_PARENT_ATTEMPT", None)
    environment.pop("PROVENANT_PARENT_RUN_TOKEN", None)
    environment.pop("PROVENANT_PARENT_REMOTE", None)
    environment.pop("PROVENANT_HOST_LOCAL_ONLY", None)
    if plan.get("attempt_identity"):
        environment["PROVENANT_PARENT_ATTEMPT"] = json.dumps(plan["attempt_identity"])
        parent_token = plan.get("_lineage_token") or control_environment.get("PROVENANT_RUN_TOKEN")
        if parent_token:
            environment["PROVENANT_PARENT_RUN_TOKEN"] = parent_token
    attempt_marker = uuid.uuid4().hex
    environment["PROVENANT_ATTEMPT_MARKER"] = attempt_marker
    return environment


def _run_attempt(
    plan,
    output_path,
    tmp_fd,
    *,
    events_path=None,
    stderr_path=None,
    on_start=None,
    on_progress=None,
    cancelled=None,
    env=None,
):
    """Run exactly one attempt. The caller owns fallback, resume, and run publication."""
    import selectors
    import threading

    adapter_config = profile(plan["adapter"])
    output_path = Path(output_path)
    events_path = Path(events_path or output_path.parent / "events.jsonl")
    stderr_path = Path(stderr_path or output_path.parent / "stderr.log")
    control_environment = dict(os.environ if env is None else env)
    environment = attempt_environment(plan, env)
    attempt_marker = environment["PROVENANT_ATTEMPT_MARKER"]
    instruction_start = None
    created_instructions = None
    if instructions_writable(plan):
        try:
            Path(plan["cwd"], INSTRUCTION_DIR).mkdir()
            created_instructions = Path(plan["cwd"], INSTRUCTION_DIR)
        except OSError:
            pass  # Present already; the snapshot fails anything but a real directory.
    if plan.get("instruction_policy"):
        try:
            instruction_start = instruction_snapshot(plan["cwd"])
        except (OSError, ValueError, subprocess.SubprocessError) as exc:
            instruction_start = exc
        # Running a skill's script must not leave bytecode behind for the check to report.
        environment["PYTHONDONTWRITEBYTECODE"] = "1"
    if plan["adapter"] == "claude" and "claude_config_dir" in plan:
        if plan["claude_config_dir"] is None:
            environment.pop("CLAUDE_CONFIG_DIR", None)
        else:
            environment["CLAUDE_CONFIG_DIR"] = plan["claude_config_dir"]
    environment["CLAUDE_CODE_DISABLE_WORKFLOWS"] = "1"
    private_tmp = Path(plan["run_dir"]) / "tmp"
    os.close(provider_confinement._private_dir(tmp_fd, "children"))
    private_cache = private_tmp / "cache"
    os.close(provider_confinement._private_dir(tmp_fd, "cache"))
    # Tool caches follow XDG_CACHE_HOME; Corepack is set too, so an inherited COREPACK_HOME
    # outside the writable roots cannot leak into child pnpm or yarn calls. zsh ignores TMPDIR
    # and writes here-documents under TMPPREFIX (default /tmp/zsh), outside a confined lane's writes.
    environment.update(TMPDIR=str(private_tmp), TMP=str(private_tmp), TEMP=str(private_tmp),
                       TMPPREFIX=str(private_tmp / "zsh"),
                       XDG_CACHE_HOME=str(private_cache),
                       COREPACK_HOME=str(private_cache / "node" / "corepack"))
    # Corepack downloads a pinned pnpm or yarn into COREPACK_HOME; no one is present to confirm the prompt.
    environment.setdefault("COREPACK_ENABLE_DOWNLOAD_PROMPT", "0")
    environment.update(tool_cache_environment(plan, private_cache, environment))
    if plan["mode"] == "worktree_write":
        # Inherited GIT_ variables were dropped above, so this list is the whole command-line scope.
        environment["GIT_CONFIG_COUNT"] = str(len(WRITER_GIT_CONFIG))
        for index, (key, value) in enumerate(WRITER_GIT_CONFIG):
            environment[f"GIT_CONFIG_KEY_{index}"] = key
            environment[f"GIT_CONFIG_VALUE_{index}"] = value
    if agy_home.needs_ps_shim(plan):
        shim_dir = str(agy_home.stage_ps_shim(tmp_fd, private_tmp))
        environment["PATH"] = shim_dir + os.pathsep + environment.get("PATH", os.defpath)
    if plan["adapter"] == "claude":
        private_claude_tmp = private_tmp / "claude"
        os.close(provider_confinement._private_dir(tmp_fd, "claude"))
        environment["CLAUDE_TMPDIR"] = str(private_claude_tmp)
        # Otherwise Claude Code opens /tmp/claude-<uid>, which the read-only profile denies.
        environment["CLAUDE_CODE_TMPDIR"] = str(private_claude_tmp)
    route = plan["route"]
    if (
        plan["adapter"] == "claude"
        and route.get("endpoint_base_url")
        and route.get("endpoint_token_env")
    ):
        environment.update(
            ANTHROPIC_BASE_URL=route["endpoint_base_url"],
            ANTHROPIC_AUTH_TOKEN=environment.get(route["endpoint_token_env"], ""),
            ANTHROPIC_API_KEY="",
        )
    adapter_environment = getattr(adapter_config, "environment", None)
    if adapter_environment:
        environment.update(adapter_environment(plan))
    warning_text = "\n".join(plan["warnings"])
    if warning_text:
        print(warning_text, file=sys.stderr, flush=True)
    if events_path.exists() or stderr_path.exists():
        suffix = uuid.uuid4().hex[:6]
        events_path = events_path.with_name(events_path.name + "." + suffix)
        stderr_path = stderr_path.with_name(stderr_path.name + "." + suffix)
    raw = BoundedCapture(events_path)
    diagnostics = BoundedCapture(stderr_path)
    started_at, started = now(), time.monotonic()
    last_progress, last_progress_at = started, started_at
    resolution, _ = harness_profile.lenient(cwd=Path(plan["cwd"]), environ=control_environment)
    silence_seconds = plan.get("silence_seconds", resolution.knobs["lanes.silence_seconds"] if resolution else 300)
    last_output, next_watch = started, started + silence_seconds
    launched, produced = started, False
    process = None
    descendants = None
    subreaper = False
    reaped = []
    child_cancellations = []
    stopped = False
    forced, cancel_signal = None, None
    preflight_error = None
    model_check, model_check_detail = "skipped", "Provider not launched"
    provider_exit_before_cleanup = None
    terminal_grace_break = False
    old_handlers = {}
    observer = provider_observer.LiveObserver(plan)

    def handle_signal(signum, frame):
        nonlocal cancel_signal
        cancel_signal = signum

    if threading.current_thread() is threading.main_thread():
        old_handlers = {
            sig: signal.getsignal(sig)
            for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)
        }
        for sig in old_handlers:
            signal.signal(sig, handle_signal)
    input_file = None
    workspace = None
    agy_staged_home, agy_home_warnings = None, []
    browser_tmp, browser_tmp_identity, browser_tmp_warnings = None, None, []
    selector = selectors.DefaultSelector()
    try:
        stall_watchdog.register_capture(output_path.parent, events_path, stderr_path)
        if "browser" in plan["applied"].get("capabilities", []):
            browser_tmp = provider_confinement._fabric_hosts_owner().browser_temporary_directory(plan["run_dir"], create=True)
            browser_tmp_identity = browser_tmp.lstat()
            plan["applied"]["browser_tmpdir"] = str(browser_tmp)
        _prepare_codex_capability_home(plan, environment)
        _prepare_codex_state_dir(plan)
        agy_staged_home = agy_home.stage_agy_home(plan, tmp_fd, private_tmp)
        if agy_staged_home:
            environment["HOME"] = str(agy_staged_home.path)
        if plan["adapter"] != "codex" and "browser" in plan["applied"].get("capabilities", []):
            environment["MAC_CHROMIUM_TMPDIR"] = str(browser_tmp)
        if plan["stdin_policy"] == "prompt":
            input_file = provider_confinement._unlinked_file(tmp_fd)
            input_file.write(plan["prompt"].encode())
            input_file.seek(0)
            stdin = input_file
        else:
            stdin = (
                subprocess.PIPE
                if plan["stdin_policy"] == "pipe"
                else subprocess.DEVNULL
            )
        cancelled_before_check = bool(cancelled and cancelled())
        copilot_disabled = plan["adapter"] == "copilot" and environment.get("CF_DISPATCH_ENABLE_COPILOT") != "1"
        command = list(plan["argv"])
        command[0] = shutil.which(command[0], path=environment.get("PATH")) or command[0]
        if not (plan.get("cooldown_blocked") or cancelled_before_check or cancel_signal or copilot_disabled):
            model_check, model_check_detail = _check_model({**plan, "argv": command}, environment)
        if plan.get("cooldown_blocked"):
            forced = "usage_limited"
            diagnostics.write(b"all alias candidates cooling\n")
        elif cancelled_before_check or (cancelled and cancelled()):
            forced = "cancelled"
        elif cancel_signal:
            forced = "interrupted"
        elif copilot_disabled:
            forced = "rejected"
            diagnostics.write(b"copilot requires CF_DISPATCH_ENABLE_COPILOT=1\n")
        else:
            if model_check == "invalid":
                raise CapabilityError("model_unavailable", model_check_detail)
            command = provider_confinement.confinement_command(plan, command, environment.get("PATH"))
            subreaper = _enable_subreaper()
            process = subprocess.Popen(
                command,
                cwd=plan["cwd"],
                env=environment,
                stdin=stdin,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
            )
            launched = time.monotonic()
            descendants = _Descendants(process, attempt_marker)
            descendants.sample()
            if on_start:
                on_start(process)
            for stream, name in (
                (process.stdout, "stdout"),
                (process.stderr, "stderr"),
            ):
                os.set_blocking(stream.fileno(), False)
                selector.register(stream, selectors.EVENT_READ, name)
            excluded = {
                path.resolve() for path in (output_path, events_path, stderr_path)
            }
            workspace = (
                WorkspaceProgress(plan["cwd"], excluded)
                if plan["mode"] == "worktree_write"
                else None
            )
            cpu, next_sample = (), started + 1
            next_tree_sample = started + 1.0
            while True:
                for key, mask in selector.select(0.05):
                    data = os.read(key.fileobj.fileno(), 65536)
                    if not data:
                        selector.unregister(key.fileobj)
                        continue
                    if key.data == "stdout":
                        produced = True
                        raw.write(data)
                        observer.feed_stdout(data)
                    else:
                        diagnostics.write(data)
                    observer.feed_stderr(data, key.data)
                    last_output = time.monotonic()
                    next_watch = last_output + silence_seconds
                    last_progress = last_output
                    last_progress_at = now()
                    if on_progress:
                        on_progress(last_progress_at)
                current = time.monotonic()
                if current >= next_tree_sample:
                    descendants.sample()
                    next_tree_sample = current + 1.0
                exited = process.poll() is not None
                if exited and not selector.get_map():
                    break
                if exited and not stopped:
                    provider_exit_before_cleanup = process.returncode
                    reaped.extend(descendants.stop(normal=True))  # descendants may hold output pipes
                    stopped = True
                if cancelled and cancelled():
                    forced = "cancelled"
                    break
                if cancel_signal:
                    forced = "interrupted"
                    break
                if plan["adapter"] == "opencode" and "usage_limited" in observer.semantic_failures:
                    observer.live_failure = observer.semantic_failures["usage_limited"]
                if observer.live_failure:
                    forced = observer.live_failure["status"]
                    break
                if (
                    observer.terminal_at is not None
                    and current - observer.terminal_at >= plan["grace_seconds"]
                ):
                    # Completion is evidenced by the event, even when the CLI hangs.
                    terminal_grace_break = True
                    break
                if current - started >= plan["timeout_seconds"]:
                    forced = "timed_out"
                    break
                # Only provider stdout counts: a banner on stderr is not a started turn.
                if not produced and current - launched >= plan.get("startup_seconds", STARTUP_SECONDS):
                    forced = "startup_timeout"
                    break
                if current >= next_sample:
                    next_sample = current + 1
                    changed = workspace.probe() if workspace is not None else False
                    new_cpu = _cpu_stamp(process.pid)
                    if changed or (cpu and new_cpu and new_cpu != cpu):
                        last_progress = current
                        last_progress_at = now()
                        if on_progress:
                            on_progress(last_progress_at)
                    cpu = new_cpu
                if current >= next_watch and not exited:
                    # poll() observes this child directly; silence alone never authorises a signal.
                    try:
                        stall_watchdog.write_observation(output_path.parent, plan["cwd"], process.pid,
                                                         True, current - last_output)
                    except (OSError, ValueError) as exc:
                        logging.warning("stall observation unavailable: %s", exc)
                    next_watch = current + silence_seconds
    except FileNotFoundError as exc:
        forced = "tool_missing"
        diagnostics.write(str(exc).encode())
    except ValueError as exc:
        forced = "model_unavailable" if model_check == "invalid" else "rejected"
        if isinstance(exc, CapabilityError):
            preflight_error = exc.code
        diagnostics.write(str(exc).encode())
    except OSError as exc:
        forced = "failed"
        diagnostics.write(str(exc).encode())
    finally:
        try:
            try:
                if workspace is not None:
                    workspace.close()
                if process:
                    if descendants and not stopped:
                        exited_at_stop = process.poll() is not None
                        provider_exit_before_cleanup = process.returncode if exited_at_stop else None
                        reaped.extend(descendants.stop(
                            normal=exited_at_stop,
                            terminal_grace=terminal_grace_break and not exited_at_stop,
                        ))
                    # Drain final bytes after the group exits, without an unbounded communicate.
                    for key in list(selector.get_map().values()):
                        while True:
                            try:
                                data = os.read(key.fileobj.fileno(), 65536)
                            except (BlockingIOError, OSError):
                                break
                            if not data:
                                break
                            (raw if key.data == "stdout" else diagnostics).write(data)
                            if key.data == "stdout":
                                observer.feed_stdout(data)
                            observer.feed_stderr(data, key.data)
                    for stream in (process.stdin, process.stdout, process.stderr):
                        if stream:
                            stream.close()
                selector.close()
                if input_file:
                    input_file.close()
                for sig, handler in old_handlers.items():
                    signal.signal(sig, handler)
            finally:
                try:
                    if subreaper:
                        _release_subreaper()
                    if agy_staged_home:
                        agy_home_warnings = agy_home.finish_agy_home(plan, tmp_fd, agy_staged_home)
                finally:
                    if browser_tmp is not None and browser_tmp_identity is not None:
                        # Cleanup trouble must not discard the lane's result; report it instead.
                        try:
                            current = browser_tmp.lstat()
                        except FileNotFoundError:
                            pass  # The lane already removed its temp directory.
                        except OSError as exc:
                            browser_tmp_warnings.append(f"browser temporary directory cleanup failed: {browser_tmp}: {exc}")
                        else:
                            if (current.st_dev, current.st_ino) != (browser_tmp_identity.st_dev, browser_tmp_identity.st_ino):
                                browser_tmp_warnings.append(f"browser temporary directory was replaced; left in place: {browser_tmp}")
                            else:
                                try:
                                    shutil.rmtree(browser_tmp)
                                except OSError as exc:
                                    browser_tmp_warnings.append(f"browser temporary directory cleanup failed: {browser_tmp}: {exc}")
        finally:
            if (forced in {"cancelled", "interrupted"} or cancel_signal) and plan.get("attempt_identity"):
                parent = plan["attempt_identity"]
                child_cancellations = run_lineage.cancel_children(
                    Path(parent["run_dir"]), parent["task_id"], parent["attempt"],
                    observed_owners=descendants.verified_owner_tokens if descendants else None,
                    shared_hosts=descendants.shared_hosts if descendants else frozenset())
    exit_code = process.returncode if process else None
    if cancel_signal and not forced:
        forced = "interrupted"  # EOF can precede the loop's signal check.
    termination_request = _termination_request(control_environment, process, exit_code, started_at, cancel_signal)
    observer.result()
    if forced == "timed_out":
        diagnostics.write(b"wall clock deadline exceeded\n")
    elif forced == "startup_timeout":
        diagnostics.write(
            f"no provider output within {plan.get('startup_seconds', STARTUP_SECONDS):g}s of launch; "
            "re-route to another model or adapter\n".encode()
        )
    stdout, stderr = (
        raw.text(),
        diagnostics.text(),
    )
    raw.close()
    diagnostics.close()
    # Read terminal semantics before attributing a signal death below. The raw
    # exit code remains in the receipt, including supervisor grace cleanup.
    parse_exit = exit_code if exit_code is not None else 1
    if observer.terminal_at is not None and not forced and parse_exit < 0:
        parse_exit = 0
    parsed = provider_output.parse_output(
        plan["adapter"],
        stdout,
        stderr,
        parse_exit,
    )
    parsed.update(observer.semantic)
    if raw.total > provider_observer.MAX_EVENTS_BYTES:
        # The complete stream is never buffered or reread. Preserve semantic
        # state observed before the diagnostic tail rolled over.
        if observer.terminal_text is not None or observer.text_chunks:
            text = observer.terminal_text if observer.terminal_text is not None else b"\n".join(observer.text_chunks).decode(errors="replace")
            text_event = getattr(adapter_config, "text_event", lambda text: {"type": "result", "result": text})
            recovered_event = text_event(text)
            recovered_events = [recovered_event]
            if plan["adapter"] == "codex":
                recovered_events = [{"type": "item.completed", "item": {"type": "agent_message", "text": text}}]
                if observer.terminal_at is not None:
                    recovered_events.append({"type": "turn.completed"})
            recovered = provider_output.parse_output(plan["adapter"], "\n".join(map(json.dumps, recovered_events)), stderr,
                                     parse_exit)
            for key in ("text", "status", "question", "signature", "excerpt"):
                parsed[key] = recovered[key]
        failures = dict(observer.semantic_failures)
        if observer.retry_failure:
            failures.setdefault(observer.retry_failure["status"], observer.retry_failure)
        if failures:
            # Quota-window classification has no independent rollover priority.
            priority = [signature for signature, _, adapters in provider_output.FAILURE_PATTERNS if signature != "quota_window"
                        and (not adapters or plan["adapter"] in adapters)]
            selected = next((failures[status] for status in priority if status in failures), next(iter(failures.values())))
            parsed.update(selected)
    if observer.live_failure and forced == observer.live_failure["status"]:
        parsed.update({key: observer.live_failure[key] for key in ("status", "signature", "excerpt", "reset_at", "retry_after")})
    valid_terminal = observer.terminal_at is not None and parsed["status"] in {"ok", "input_required"} and bool(parsed["text"].strip())
    if plan["adapter"] == "opencode" and parsed["status"] == "usage_limited":
        parsed["signature"] = "quota_exhausted"
        if not parsed["reset_at"] and re.search(r"monthly usage limit", parsed.get("failure_text") or parsed["excerpt"], re.I):
            # Billing anniversary is unknown: re-probe after a day. The next attempt is the probe;
            # success clears the entry and failure re-arms it.
            parsed["retry_after"] = 24 * 3600
    if forced:
        parsed["status"] = forced
        parsed["signature"] = {
            "startup_timeout": "startup_watchdog",
            "timed_out": "wall_clock",
            "cancelled": "cancel_requested",
            "interrupted": "supervisor_signal",
        }.get(forced, parsed["signature"])
        if preflight_error:
            parsed.update(signature=preflight_error, excerpt=stderr.strip())
    elif provider_exit_before_cleanup is not None and provider_exit_before_cleanup < 0:
        parsed["status"] = "interrupted" if valid_terminal else "failed"
        parsed["signature"] = "provider_signal"
        source = termination_request.get("termination_source", "sender unknown")
        parsed["excerpt"] = f"provider terminated by {_signal_name(-provider_exit_before_cleanup)}; {source}"
    if (termination_request.get("termination_source") == "cancel_requested"
            and parsed["status"] in {"interrupted", "failed"}):
        parsed["status"] = "cancelled"
        parsed["signature"] = "cancel_requested"
    if plan.get("cooldown_blocked"):
        parsed["reset_at"] = plan["cooldown_blocked"]["cooling_until"]
        parsed["signature"] = "cooldown_active"
    session = parsed["session_id"] or plan["resume_session"] or plan["session_id"]
    parsed["session_id"] = session
    observed, source = provider_output._observed_model(plan, parsed, environment)
    warnings = [*plan["warnings"], *agy_home_warnings, *browser_tmp_warnings]
    if model_check == "unavailable":
        warnings.append(model_check_detail)
    for capture in (raw, diagnostics):
        if capture.write_error is not None:
            warnings.append(f"{capture.path.name} capture write failed: {capture.write_error}")
    if parsed["signature"] == "exit_after_output":
        warnings.append(parsed["excerpt"])
    reported = {}
    if plan["adapter"] == "claude":
        # init.model is the request; Claude may answer with another model.
        answered = observer.meter.models["answered"]
        reported = {"init_model": observer.meter.models["init"], "answered_models": list(answered)}
        answering, answering_source = observer.meter.answering_model()
        if answering:
            observed, source = answering, answering_source
        if len(answered) > 1:
            warnings.append("claude answered as " + ", ".join(answered) + "; route records " + observed)
        elif answering and observer.meter.models["init"] and answering != observer.meter.models["init"]:
            warnings.append("claude answered as " + answering + "; init reported " + observer.meter.models["init"])
    if descendants and descendants.snapshot_unavailable:
        warnings.insert(0, "descendant census unavailable; process cleanup could not be verified")
    if reaped:
        warnings.insert(0, f"reaped {len(reaped)} leftover process(es)")
    if route.get("effort_substitution") and route["effort_substitution"] not in warnings:
        warnings.append(route["effort_substitution"])
    if observer.text_truncated or (raw.total > provider_observer.MAX_EVENTS_BYTES and observer.terminal_text is None and not observer.text_chunks):
        warnings.append("result truncated at capture limit; inspect provider session for complete output")
        if parsed["status"] in {"ok", "input_required"}:
            parsed.update(status="partial", question=None, signature="output_truncated")
    notes = list(route.get("notes") or [])
    observer.observe_model(observed)
    final_mismatch = bool(observed) and not provider_output._same_model(plan["adapter"], plan["model"], observed)
    substituted = bool(observer.model_changes) or final_mismatch
    if substituted:
        notes.append("mismatch: resolved " + plan["model"] + "; observed " + observed if final_mismatch
                     else "model changed during execution; final model " + observed)
        warnings.append(notes[-1])
        if plan["adapter"] == "agy" and plan.get("requested_model"):
            parsed["status"] = "model_unavailable"
            parsed["signature"] = "pinned_model_substitution"
    identity = "observed" if observed else "resolved" if plan["model"] else "unknown"
    family = route.get("model_family") or route.get("family") or "unknown"
    if substituted:
        try:
            try:
                from . import exec_routing
            except ImportError:
                import exec_routing
            observed_families = exec_routing.model_families(observed)
        except Exception:
            observed_families = ()
        if len(observed_families) == 1:
            family = observed_families[0]
            notes.append(f"observed family {family} inferred from catalogue")
        else:
            family = "unknown"
            notes.append("observed model family unverified after substitution")
    model = observed or plan["model"]
    # Applied effort is what was sent; with none sent, only a provider-reported
    # value may fill it (Codex writes one to its rollout), never a guessed default.
    effort, effort_source = plan["effort"], None
    if not effort and plan["adapter"] == "codex":
        effort = context_usage.codex_rollout_effort(session, environment) or ""
        effort_source = "codex:rollout.turn_context" if effort else None
    line = (
        f"Route: {plan['adapter']}/{model}"
        + ("@" + effort if effort else "")
        + f" ({family}; {identity})"
    )
    provenance = {
        "requested": {
            "adapter": plan["adapter"],
            "alias": route.get("alias"),
            "model": plan.get("requested_model"),
            "effort": plan.get("requested_effort"),
        },
        "resolved_model": plan["model"],
        "observed_model": observed,
        "observed_source": source,
        **reported,
        "identity": identity,
        "provider": route.get("endpoint_provider") or plan["adapter"],
        "transport": plan["adapter"],
        **({"account": plan["account"]} if "account" in plan else {}),
        "family": family,
        "effort_requested": plan.get("requested_effort"),
        "effort_applied": effort,
        "effort_observed_source": effort_source,
        "cli_version": route.get("cli_version"),
        "fallback_from": plan.get("fallback_from"),
        "harness_rev": harness_revision(),
        "notes": notes,
        "line": line,
    }
    status = parsed["status"]
    quarantine = None
    instruction_changes = []
    if instruction_start is not None:
        try:
            if isinstance(instruction_start, Exception):
                raise instruction_start
            instruction_changes = authored_instruction_changes(plan["cwd"], instruction_start)
        except (OSError, ValueError, subprocess.SubprocessError) as exc:
            instruction_changes = [f"{INSTRUCTION_DIR} (unverifiable: {exc})"]
    # A signal death must retain actual instruction edits as failures rather
    # than repairing a partial attempt and disguising its changed instructions.
    instruction_policy = ("deny" if status == "interrupted" or parsed["signature"] == "provider_signal" or
                          (status == "cancelled" and termination_request)
                          else plan.get("instruction_policy", "quarantine"))
    # Assurance failure blocks landing, independently of a usable provider result.
    if instruction_changes and instruction_policy == "allow" and instruction_quarantine.quarantinable(instruction_changes):
        warnings.append("lane changed " + INSTRUCTION_DIR + "/ (allowed by policy): " + ", ".join(instruction_changes))
        instruction_changes = []
    if instruction_changes:
        unverifiable = any("(unverifiable:" in change for change in instruction_changes)
        signature = "protected_instructions_unverifiable" if unverifiable else "protected_instructions_changed"
        quarantine = {"reason": signature + ": " + ", ".join(instruction_changes),
                      "patches": [], "restored": False, "patch_error": None}
        patch = Path(plan["run_dir"], "protected.patch")
        patches = []
        try:
            # Retain an explicit unavailable marker even if safe diff capture fails.
            with tempfile.TemporaryDirectory() as marker_dir:
                marker = Path(marker_dir, "protected.patch")
                marker.write_text("# Instruction diff unavailable: " + quarantine["reason"] + "\n")
                install(str(marker), str(patch))
            quarantine["patches"] = [str(patch)]
            if not instruction_quarantine.quarantinable(instruction_changes) or not isinstance(instruction_start, dict):
                raise ValueError("no safe instruction diff can be captured; inspect the retained worktree")
            patches = instruction_quarantine.quarantine_instruction_changes(
                plan["cwd"], instruction_start, instruction_changes, Path(plan["run_dir"]),
                restore_changes=instruction_policy == "quarantine", published=patches)
            quarantine["patches"] = list(map(str, patches))
            if instruction_policy == "quarantine":
                remaining = authored_instruction_changes(plan["cwd"], instruction_start)
                if remaining:
                    raise ValueError("still changed after restore: " + ", ".join(remaining))
                quarantine["restored"] = True
                warnings.append("lane changed " + INSTRUCTION_DIR + "/ beyond its start and integration branch: "
                                + ", ".join(instruction_changes) + f"; quarantined to {', '.join(map(str, patches))} "
                                "and removed from the lane; review before landing")
                instruction_changes = []
        except (OSError, ValueError, subprocess.SubprocessError) as exc:
            quarantine["patch_error"] = str(exc)
            warnings.append(f"quarantine of {INSTRUCTION_DIR}/ changes failed: {exc}")
        if patches:
            quarantine["patches"] = list(map(str, patches))
        if instruction_changes:  # Edits remain on the lane: fail, as without quarantine.
            status = "failed"
        elif status == "ok":
            status = "quarantined"
        parsed["signature"] = signature
    if created_instructions is not None:
        try:
            created_instructions.rmdir()  # Only the empty directory this attempt added.
        except OSError:
            pass
    if instruction_changes:
        warnings.append(("instruction assurance unavailable: " if unverifiable else
                         "lane changed " + INSTRUCTION_DIR + "/ beyond its start and integration branch: ")
                        + ", ".join(instruction_changes))
    if (plan["adapter"] == "agy" and status not in {"ok", "input_required", "partial", "quarantined"}
            and not (quarantine and parsed["status"] == "ok")):
        parsed["text"] = ""
        stderr = "agy dispatch failed: status=" + status + "\n" + stderr
        if parsed["excerpt"]:
            parsed["excerpt"] = "provider error: " + parsed["excerpt"]
    output = (
        parsed["text"]
        if (status in {"ok", "input_required", "partial", "quarantined"}
            or quarantine and parsed["status"] == "ok"
            or plan["adapter"] == "opencode" and observer.terminal_at is not None and parsed["text"].strip())
        else (parsed["text"] + "\n" if parsed["text"] else "")
        + stderr
        + ("\n" + parsed["excerpt"] if parsed["excerpt"] else "")
    )
    digest_value = ""
    output_value = ""
    try:
        with provider_confinement._unlinked_file(tmp_fd) as staged:
            staged.write(output.encode())
            staged.flush()
            digest_value, device, inode = install(staged.fileno(), str(output_path))
            verify(str(output_path), digest_value, device, inode)
        output_value = str(output_path)
    except (OSError, CustodyError):
        status = "output_identity_invalid" if digest_value else "output_write_error"
        digest_value = ""
    cross = bool(
        not substituted
        and plan.get("orchestrator_family")
        and family not in {"unknown", "generic-open", "open-weight"}
        and family != plan["orchestrator_family"]
    )
    guarantee = plan["applied"]["guarantee"]
    fix = (
        "check the Fabric product wrapper, supported Node runtime and confinement grants"
        if parsed["signature"] == "fabric_mcp_unavailable"
        else
        "run `kiro-cli whoami` outside the sandbox"
        if status == "auth_required" and parsed["signature"] == "auth_expired"
        else
        provider_output._variant_fix(parsed["failure_text"])
        if status == "rejected" and parsed["signature"] == "invalid_input"
        else
        "OpenCode rejected the free-tier request; use a paid opencode-go model or report this"
        if status == "model_unavailable"
        and plan["adapter"] == "opencode"
        and re.search(
            r"FreeTierError|free tier can only be used from within OpenCode",
            parsed.get("failure_text", stderr),
            re.I,
        )
        else model_check_detail if model_check == "invalid" else provider_output._model_unavailable_fix(plan)
        if status == "model_unavailable"
        else {
            "auth_required": "authenticate the provider CLI",
            "permission_blocked": "check the requested sandbox and directory grants",
            "tool_missing": "install the provider CLI",
            "stalled": "inspect events or try another model",
            "startup_timeout": "re-route to another model or adapter",
            "usage_limited": "wait for reset or choose another model",
            "rate_limited": "retry after the recorded cooldown",
        }.get(status)
    )
    if quarantine and status == "failed" and instruction_changes:
        fix = ("instruction edits remain on the lane branch; restore " + INSTRUCTION_DIR
               + "/ to the start commit before landing; patches: "
               + (", ".join(quarantine["patches"]) or "unavailable")
               + ("; " + quarantine["patch_error"] if quarantine["patch_error"] else ""))
    elif quarantine:
        fix = ("not landable; review quarantine: " + quarantine["reason"] + "; patches: "
               + (", ".join(quarantine["patches"]) or "unavailable")
               + ("; " + quarantine["patch_error"] if quarantine["patch_error"] else ""))
    record = {
        **{
            key: route.get(key, "")
            for key in (
                "requested_effort",
                "effort_source",
                "effort_capability_source",
                "effort_substitution",
                "substitution",
                "fallback_model",
                "catalog_model",
                "model_selection",
                "identity_source",
                "policy_override",
            )
        },
        "tool": plan["adapter"],
        "adapter": plan["adapter"],
        "adapter_gate": "direct-cli",
        **({"account": plan["account"]} if "account" in plan else {}),
        "execution_intent": plan["intent"],
        "model": plan["model"],
        "requested_model": plan.get("requested_model") or plan["model"],
        "resolved_model": plan["model"],
        "model_check": model_check,
        "model_check_detail": model_check_detail,
        "model_changes": observer.model_changes,
        "effort": plan["effort"],
        "status": status,
        **({"result_partial": True} if status not in {"ok", "input_required", "quarantined"} and not valid_terminal and parsed["text"].strip() else {}),
        **({"error": parsed["signature"]} if status == "rejected" and parsed["signature"] in {
            "invalid_input", "fabric_mcp_unavailable"} else {}),
        **({"error": parsed["signature"]} if instruction_changes else {}),
        **({"landable": False, "quarantine": quarantine} if quarantine else {}),
        "reason": parsed["excerpt"],
        "exit": 0
        if status in {"ok", "input_required", "quarantined"}
        and (observer.terminal_at is not None or parsed["signature"] == "exit_after_output")
        else exit_code,
        "output_path": output_value,
        "output_digest": digest_value,
        "read_only_guarantee": guarantee if plan["mode"] == "read_only" else "none",
        "provider_sandbox": plan["agy_sandbox"]
        if plan["adapter"] == "agy"
        else plan["applied"]["sandbox"],
        "provider_network": plan["applied"]["network"],
        "access_mode": plan["mode"],
        "worktree": plan["worktree"] or "",
        "original_prompt_file": plan.get("original_prompt_file"),
        "orchestrator_family": plan.get("orchestrator_family", ""),
        "provider_family": family,
        "model_family": family,
        "endpoint_provider": provenance["provider"],
        "route_alias": route.get("alias", ""),
        "reviewer_id": plan.get("reviewer_id", ""),
        "risk_tier": plan.get("risk_tier", ""),
        "model_override_tier": plan.get("model_override_tier", ""),
        "cross_family": cross,
        "certification_eligible": plan["intent"] == "assurance"
        and status == "ok"
        and not substituted
        and cross
        and plan["mode"] == "read_only"
        and guarantee == "enforced",
        "session_id": session,
        "provenance": provenance,
        "applied": plan["applied"],
        "context": context_usage.with_codex_rollout(observer.meter.result(), session, environment)
        if plan["adapter"] == "codex"
        else observer.meter.result(),
        "usage_source": context_usage.usage_source(observer.meter.result()),
        "error_class": provider_output.error_class(status, parsed["signature"]),
        "warnings": warnings,
        "reaped": reaped,
        **({"children": child_cancellations} if child_cancellations else {}),
        **({"spared": len(descendants.spared_at_stop)} if descendants and descendants.spared_at_stop else {}),
        "question": parsed["question"],
        "retryable": status
        in {"usage_limited", "rate_limited", "model_unavailable", "stalled", "startup_timeout"},
        "reset_at": parsed["reset_at"],
        "retry_after": parsed["retry_after"],
        "fix": fix,
        "evidence": {
            "exit": exit_code,
            "signal": -exit_code if exit_code and exit_code < 0 else None,
            "signal_name": _signal_name(-exit_code) if exit_code and exit_code < 0 else None,
            "termination_source": (
                "external_signal" if provider_exit_before_cleanup is not None and provider_exit_before_cleanup < 0
                else "supervisor_signal" if cancel_signal
                else forced if forced
                else "terminal_grace" if terminal_grace_break
                else "provider_exit"
            ),
            "signal_sender_pid": None,  # waitpid exposes the signal, not its sender.
            "signal_requester_pid": None,
            "supervisor_signal": cancel_signal,
            **termination_request,
            "signature": parsed["signature"],
            "excerpt": parsed["excerpt"][:200],
        },
        "pgid": process.pid if process else None,
        "started_at": started_at,
        "ended_at": now(),
        "last_progress_at": last_progress_at,
        "auth_or_quota_error": status
        in {"usage_limited", "rate_limited", "auth_required"},
        "paths": {
            "result": output_value or None,
            "stderr": str(stderr_path),
            "events": str(events_path),
            "receipt": None,
        },
    }
    return record


def observe_main(argv):
    """Read lane line envelopes and request only semantic terminal/stop actions."""
    parser = argparse.ArgumentParser(description="Observe captured provider lines")
    parser.add_argument("--adapter", required=True)
    parser.add_argument("--model", default="")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--max-bytes", type=int, default=provider_observer.MAX_EVENTS_BYTES)
    args = parser.parse_args(argv)
    provider_observer.MAX_EVENTS_BYTES = args.max_bytes
    observer = provider_observer.LiveObserver({"adapter": args.adapter, "model": args.model})
    terminal_at, stop = None, None
    for envelope in sys.stdin:
        message = json.loads(envelope)
        data = message["line"].encode()
        if not data.endswith(b"\n"):
            data += b"\n"
        if message["s"] == "out":
            observer.feed_stdout(data)
            observer.feed_stderr(data, "stdout")
        elif message["s"] == "err":
            observer.feed_stderr(data, "stderr")
        else:
            raise ValueError("unknown observer stream")
        if observer.terminal_at is not None and observer.terminal_at != terminal_at:
            print(json.dumps({"terminal": True}), flush=True)
        terminal_at = observer.terminal_at
        failure = observer.live_failure
        if args.adapter == "opencode" and "usage_limited" in observer.semantic_failures:
            failure = observer.semantic_failures["usage_limited"]
            observer.live_failure = failure
        if failure and failure["status"] != stop:
            stop = failure["status"]
            if stop == "stalled":
                raise ValueError("silence cannot request a stop")
            print(json.dumps({"stop": stop}), flush=True)
    result = observer.result()
    with tempfile.TemporaryFile() as staged:
        staged.write((json.dumps(result) + "\n").encode())
        staged.flush()
        install(staged.fileno(), str(args.out.absolute()))
    return 0


def parser():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--route-file", type=Path, required=True)
    p.add_argument("--adapter", required=True)
    p.add_argument("--account")
    p.add_argument("--prompt-file", type=Path, required=True)
    p.add_argument("--original-prompt-file", type=Path)
    p.add_argument("--out", type=Path, required=True)
    p.add_argument("--run-dir", type=Path)
    p.add_argument("--plan-only", action="store_true")
    p.add_argument("--mode", default="read_only")
    p.add_argument("--cwd", type=Path)
    p.add_argument("--workspace-root", type=Path)
    p.add_argument("--read-root", action="append", default=[])
    p.add_argument("--worktree")
    p.add_argument("--sandbox")
    p.add_argument("--network", choices=["true", "false"])
    p.add_argument("--capabilities", type=parse_capabilities_argument)
    p.add_argument("--add-dir", action="append", default=[])
    p.add_argument("--timeout-seconds", type=float)
    p.add_argument("--intent", default="ordinary")
    p.add_argument("--orchestrator-family", default="")
    p.add_argument("--reviewer-id", default="")
    p.add_argument("--risk-tier", default="")
    p.add_argument("--model-override-tier", default="")
    p.add_argument("--requested-model")
    p.add_argument("--requested-effort")
    p.add_argument("--resume-session")
    p.add_argument("--context-ceiling", type=float)
    p.add_argument("--no-preface", action="store_true")
    p.add_argument("--cleanup-dir", type=Path)
    p.add_argument("--cleanup-prompt", action="store_true")
    return p


def main():
    if sys.argv[1:2] == ["observe"]:
        return observe_main(sys.argv[2:])
    args = parser().parse_args()
    writer_lease = None
    try:
        workspace_root = Path(args.workspace_root or Path.cwd()).expanduser().resolve()
        selected_cwd = Path(args.cwd or workspace_root).expanduser().resolve()
        if not args.worktree and not any(
                selected_cwd.is_relative_to(root) for root in provider_confinement.read_boundary(workspace_root, args.read_root)):
            raise ValueError("cwd must be inside the workspace")
        route = json.loads(args.route_file.read_text())
        original_prompt_file = args.original_prompt_file or args.prompt_file
        provider_confinement.check_protected_inputs(route, workspace_root, selected_cwd, worktree=args.worktree,
                               prompt_file=original_prompt_file, add_dirs=args.add_dir,
                               read_roots=args.read_root)
        if original_prompt_file != args.prompt_file:
            provider_confinement.check_protected_inputs(route, workspace_root, selected_cwd, worktree=args.worktree,
                                   prompt_file=args.prompt_file, add_dirs=args.add_dir,
                                   read_roots=args.read_root)
        plan = build_plan(
            args.adapter,
            route,
            args.prompt_file.read_text(),
            mode=args.mode,
            cwd=args.cwd,
            workspace_root=workspace_root,
            read_roots=args.read_root,
            worktree=args.worktree,
            sandbox=args.sandbox,
            network=None if args.network is None else args.network == "true",
            add_dirs=args.add_dir,
            capabilities=args.capabilities,
            timeout_seconds=args.timeout_seconds,
            intent=args.intent,
            preface=not args.no_preface,
            resume_session=args.resume_session,
            context_ceiling=args.context_ceiling,
            account=args.account,
            orchestrator_family=args.orchestrator_family,
            reviewer_id=args.reviewer_id,
            risk_tier=args.risk_tier,
            model_override_tier=args.model_override_tier,
            requested_model=args.requested_model,
            requested_effort=args.requested_effort,
            prompt_file=args.prompt_file,
            original_prompt_file=original_prompt_file,
            run_dir=args.run_dir or args.out.parent,
            run_id=os.environ.get("PROVENANT_RUN_ID", ""),
            chair=os.environ.get("PROVENANT_CHAIR", ""),
        )
        plan["output_path"] = str(args.out.absolute())
        if args.plan_only:
            print(json.dumps(plan))
            return 0
        if args.mode == "worktree_write":
            # Direct CLI calls own the same registered-worktree lease as Fabric.
            import writer_lease as writer_lease_owner

            writer_lease = writer_lease_owner.acquire_worktree_lease(
                writer_lease_owner.resolve_writer_worktree(Path(args.worktree))
            )
        record = execute(
            plan,
            args.out,
            events_path=Path(str(args.out) + ".raw.jsonl"),
            stderr_path=Path(str(args.out) + ".stderr.log"),
        )
        print(json.dumps(record))
        return 0 if record["status"] in {"ok", "input_required"} else 1
    except (OSError, ValueError) as exc:
        status = (
            "output_write_error"
            if isinstance(exc, (CustodyError, OSError))
            else "rejected"
        )
        print(
            json.dumps(
                {
                    "status": status,
                    **({"error": exc.code} if isinstance(exc, CapabilityError) else {}),
                    "fix": str(exc),
                    "output_path": "",
                    "output_digest": "",
                    "certification_eligible": False,
                }
            )
        )
        return 2
    finally:
        if writer_lease is not None:
            import writer_lease as writer_lease_owner

            writer_lease_owner.release_worktree_lease(writer_lease)
        if args.cleanup_prompt:
            args.prompt_file.unlink(missing_ok=True)
        if args.cleanup_dir:
            shutil.rmtree(args.cleanup_dir, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
