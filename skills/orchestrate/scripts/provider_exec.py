#!/usr/bin/env python3
"""One provider descendant supervisor, shared by direct and Fabric owners."""

from __future__ import annotations

import argparse
from collections import deque
import ctypes
import ctypes.util
import json
import math
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import uuid
from datetime import UTC, datetime, timedelta
from dataclasses import dataclass

sys.path.insert(0, str(Path(__file__).resolve().parent))
from adapters import profile
from output_custody import install, verify, CustodyError


def now():
    return datetime.now(UTC).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def credential_path(path):
    candidate = Path(path).expanduser().resolve()
    home = Path.home().resolve()
    stores = [home / name for name in (
        '.ssh', '.aws', '.azure', '.gnupg', '.codex', '.claude',
        '.gemini', '.cursor', '.kiro', '.docker', '.kube', '.netrc', '.npmrc',
        '.config/gh', '.config/gcloud', '.config/claude', '.config/codex',
        '.config/openai', '.config/opencode', '.local/share/opencode', 'Library/Keychains',
        'Library/Application Support',
    )]
    if any(candidate.is_relative_to(store) or store.is_relative_to(candidate) for store in stores):
        return True
    parts = [part.lower() for part in candidate.parts]
    return (
        bool(set(parts) & {".ssh", ".aws", ".azure", ".gnupg", ".codex", ".claude",
                           ".gemini", ".cursor", ".kiro", ".docker", ".kube", ".netrc", ".npmrc"})
        or any(
            part
            in {
                ".env",
                ".env.local",
                ".env.production",
                "credentials.json",
                "auth.json",
                "auth.db",
                "token.json",
                "application_default_credentials.json",
            }
            for part in parts
        )
        or any(
            part == ".config"
            and index + 1 < len(parts)
            and parts[index + 1] in {"gcloud", "gh", "claude", "codex", "openai", "opencode"}
            for index, part in enumerate(parts)
        )
        or any(
            part in {".codex", ".claude"}
            and index + 1 < len(parts)
            and parts[index + 1] in {"auth.json", ".credentials.json"}
            for index, part in enumerate(parts)
        )
    )


def build_plan(
    adapter,
    route,
    prompt,
    *,
    cwd=None,
    mode="read_only",
    worktree=None,
    sandbox=None,
    network=None,
    add_dirs=(),
    timeout_seconds=None,
    idle_seconds=None,
    preface=True,
    run_id="",
    chair="",
    resume_session=None,
    session_id=None,
    requested_model=None,
    requested_effort=None,
    intent="ordinary",
    **metadata,
):
    config = profile(adapter)
    selected_cwd = Path(worktree or cwd or Path.cwd()).expanduser().resolve()
    if not selected_cwd.is_dir():
        raise ValueError("cwd must be a readable directory")
    cwd = str(selected_cwd)
    sandbox = sandbox or (
        "workspace-write" if mode == "worktree_write" else "read-only"
    )
    if mode == "read_only" and sandbox != "read-only":
        raise ValueError("write sandbox on a read-only run is forbidden")
    if sandbox not in {"read-only", "workspace-write", "full"}:
        raise ValueError("invalid sandbox")
    if network is not None and type(network) is not bool:
        raise ValueError("network must be a boolean")
    directories = list(
        dict.fromkeys(str(Path(p).expanduser().resolve()) for p in add_dirs)
    )
    warnings = list(route.get("notes") or [])
    safe_directories = []
    for directory in directories:
        if credential_path(directory) or Path.home().resolve().is_relative_to(Path(directory)):
            warnings.append("credential or authentication add-dir dropped: " + directory)
        else:
            safe_directories.append(directory)
    directories = safe_directories
    if any(not Path(p).is_dir() for p in directories):
        raise ValueError("add-dir must be a readable directory")
    if mode == "worktree_write" and Path(cwd, ".git").is_file():
        common = subprocess.run(
            [
                "git",
                "-C",
                cwd,
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
            ],
            capture_output=True,
            text=True,
            timeout=5,
        )
        if common.returncode == 0 and common.stdout.strip() not in directories:
            directories.append(common.stdout.strip())
    model = route.get("resolved_model") or route.get("model") or ""
    effort = route.get("effort_applied", route.get("effort")) or ""
    if effort == "default":
        effort = ""
    if effort and config.EFFORT_FLAG is None:
        warnings.append(f"{adapter} does not expose effort control; requested {effort}")
        effort = ""
    route_label = adapter + "/" + model + ("@" + effort if effort else "")
    if "\x00" in prompt:
        raise ValueError("prompt contains NUL")
    if preface:
        prompt = (
            f"You are {route_label} via Fabric. Attribute work to exactly this route; never guess a model name.\n\n"
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
    if adapter in {"agy", "kiro"} or (
        mode == "worktree_write" and guarantee != "enforced"
    ):
        warnings.append(f"{adapter} {mode} guarantee={guarantee}")
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
    applied_sandbox = (
        sandbox
        if adapter == "codex"
        else "read-only"
        if mode == "read_only" and adapter in {"claude", "cursor"}
        else None
    )
    if sandbox == "full" and adapter != "codex":
        warnings.append("sandbox control unsupported by " + adapter)
    if adapter in {"cursor", "kiro", "copilot", "opencode"} and directories:
        warnings.append("additional directories unsupported by " + adapter)
        directories = []
    timeout = float(timeout_seconds or (10800 if mode == "worktree_write" else 3600))
    idle = float(
        idle_seconds
        or os.environ.get("CF_DISPATCH_IDLE_SECONDS")
        or (config.IDLE_WRITE if mode == "worktree_write" else config.IDLE_READ)
    )
    if not all(math.isfinite(value) and value > 0 for value in (timeout, idle)):
        raise ValueError("timeouts must be finite positive numbers")
    boundary = f"Workspace root: {cwd}\nResolve relative paths against this root. " + (
        "Write, run commands and commit only inside this owned worktree. Do not push or change other checkouts."
        if mode == "worktree_write"
        else "Do not modify files or run commands that mutate state. Use file-reading tools only."
    )
    plan = {
        "schema": "fabric.exec-plan.v1",
        "adapter": adapter,
        "route": route,
        "model": model,
        "effort": effort,
        "prompt": prompt,
        "network_requested": network,
        "cwd": cwd,
        "mode": mode,
        "worktree": str(worktree) if worktree else None,
        "timeout_seconds": timeout,
        "idle_seconds": idle,
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
            "guarantee": guarantee,
        },
        "agy_sandbox": intent == "assurance"
        or os.environ.get("CF_DISPATCH_AGY_SANDBOX", "0") == "1",
        **metadata,
    }
    plan["argv"] = config.argv(plan)
    if config.PROMPT_TRANSPORT == "argv":
        ceiling = int(os.environ.get("CF_DISPATCH_ARGV_PROMPT_MAX_BYTES", "65536"))
        if any(len(arg.encode()) > ceiling for arg in plan["argv"]):
            raise ValueError(
                "prompt_too_large: argv prompt exceeds the single-argument ceiling"
            )
    return plan


SIGNATURES = (
    (
        "permission_blocked",
        r"permission denied|permission blocked|not allowed to|tool required the .+ permission|denied_actions",
    ),
    (
        "usage_limited",
        r"you.ve hit your (?:usage|session|weekly) limit|usage limit|session limit|weekly limit|individual quota reached|insufficient_quota|quota exceeded|RESOURCE_EXHAUSTED|exhausted your capacity",
    ),
    (
        "auth_required",
        r"authentication required|please sign in|please(?: run)? login|not logged in|not authenticated|unauthenticated|unauthorized|login expired|\b401\b",
    ),
    ("rate_limited", r"rate.?limit|too many requests|overloaded|\b(?:429|529)\b"),
    (
        "model_unavailable",
        r"model[^\n]*(?:unavailable|not available|not found|unsupported|does not exist)|unknown model",
    ),
    ("permission_blocked", r"\b403\b|forbidden"),
)


def _objects(value):
    if isinstance(value, dict):
        yield value
        for item in value.values():
            yield from _objects(item)
    elif isinstance(value, list):
        for item in value:
            yield from _objects(item)


def reset_time(text, events, at=None):
    at = at or datetime.now().astimezone()
    for event in events:
        for item in _objects(event):
            epoch = item.get("resetsAt")
            if isinstance(epoch, (int, float)) and not isinstance(epoch, bool):
                try:
                    return (
                        datetime.fromtimestamp(epoch, UTC)
                        .isoformat()
                        .replace("+00:00", "Z")
                    )
                except (ValueError, OverflowError, OSError):
                    pass
    hours = re.search(r"Resets in\s+(\d+)h(?:\s*(\d+)m)?", text, re.I)
    if hours:
        return (
            (at + timedelta(hours=int(hours[1]), minutes=int(hours[2] or 0)))
            .astimezone(UTC)
            .isoformat()
            .replace("+00:00", "Z")
        )
    local = re.search(r"try again at\s+(\d{1,2}):(\d{2})\s*(AM|PM)", text, re.I)
    if local:
        hour = int(local[1]) % 12 + (12 if local[3].upper() == "PM" else 0)
        candidate = at.replace(hour=hour, minute=int(local[2]), second=0, microsecond=0)
        if candidate <= at:
            candidate += timedelta(days=1)
        return candidate.astimezone(UTC).isoformat().replace("+00:00", "Z")
    return None


def parse_output(adapter, stdout, stderr="", exit_code=0, *, at=None):
    """Interpret structured failures before diagnostic signatures; never classify answer prose."""
    config = profile(adapter)
    result = {
        "status": "failed",
        "text": "",
        "session_id": None,
        "observed_model": None,
        "reset_at": None,
        "retry_after": None,
        "signature": None,
        "excerpt": "",
        "terminal": False,
        "question": None,
    }
    events, plain, parts, errors = [], [], [], []
    retry_errors = []
    duplicate_json = False

    def no_duplicates(pairs):
        nonlocal duplicate_json
        value = {}
        for key, item in pairs:
            if key in value:
                duplicate_json = True
            value[key] = item
        return value

    terminal_text = None
    partial = False
    try:
        whole = json.loads(stdout, object_pairs_hook=no_duplicates)
        if isinstance(whole, dict):
            events = [whole]
            if "type" not in whole and not (adapter == "agy" and "status" in whole):
                plain = [stdout]
        else:
            plain = []
    except ValueError:
        for line in stdout.splitlines(keepends=True):
            try:
                event = json.loads(line, object_pairs_hook=no_duplicates)
            except ValueError:
                plain.append(line)
                continue
            if isinstance(event, dict):
                events.append(event)
    if adapter == "agy" and (
        duplicate_json
        or "\ufffd" in stdout
        or (plain and any("status" in event for event in events))
    ):
        errors.append("invalid agy envelope")
    for event in events:
        kind = event.get("type") or ""
        if not kind and isinstance(event.get("event"), str):
            kind = event["event"]  # agy names the event kind `event`
        if adapter == "kiro" and event.get("jsonrpc") == "2.0":
            for item in _objects(event):
                if item.get("sessionUpdate") == "agent_message_chunk":
                    content = item.get("content", {})
                    if isinstance(content, dict) and isinstance(
                        content.get("text"), str
                    ):
                        parts.append(content["text"])
                if item.get("category") == "model" and isinstance(
                    item.get("currentValue"), str
                ):
                    result["observed_model"] = item["currentValue"]
            if isinstance(event.get("result"), dict) and event["result"].get(
                "stopReason"
            ):
                result["terminal"] = True
            if event.get("error"):
                errors.append(json.dumps(event["error"]))
        # kiro-cli --agent-engine v2 wraps the same ACP updates in typed events.
        if adapter == "kiro" and kind in {"sessionUpdate", "runFinished"}:
            data = event.get("data") if isinstance(event.get("data"), dict) else {}
            update = data.get("update") if isinstance(data.get("update"), dict) else {}
            content = update.get("content") if isinstance(update.get("content"), dict) else {}
            if update.get("sessionUpdate") == "agent_message_chunk" and isinstance(content.get("text"), str):
                parts.append(content["text"])
            if kind == "runFinished":
                result["terminal"] = True
                if isinstance(data.get("finalText"), str):
                    terminal_text = data["finalText"]
                if str(data.get("status", "success")).lower() != "success":
                    errors.append(json.dumps(data, ensure_ascii=False)[:400])
        for item in _objects(event):
            for key in config.SESSION_KEYS:
                if isinstance(item.get(key), str):
                    result["session_id"] = item[key]
            if isinstance(item.get("retry_after"), (int, float)):
                result["retry_after"] = item["retry_after"]
        if adapter in {"claude", "cursor", "agy", "kiro"}:
            if (
                kind
                in {
                    "init",
                    "system",
                    "session_start",
                    "session.created",
                    "message_start",
                }
            ):
                for item in _objects(event):
                    if isinstance(item.get("model"), str):
                        result["observed_model"] = item["model"]
        error_event = kind in {
            "error",
            "turn.failed",
            "api_retry",
            "AGY_ERROR",
        } or bool(event.get("is_error"))
        if kind == "rate_limit_event":
            # Allowed utilization notices are evidence, not failures.
            error_event = any(
                item.get("status") in {"rejected", "limited", "exceeded"}
                for item in _objects(event)
            )
            if error_event:
                errors.append("usage limit " + json.dumps(event))
        if error_event:
            error_text = json.dumps(event, ensure_ascii=False)
            if not (adapter == "codex" and "reconnecting" in error_text.lower()):
                (retry_errors if kind == "api_retry" else errors).append(error_text)
        if event.get("denied_actions") is not None and not isinstance(
            event["denied_actions"], list
        ):
            errors.append("invalid agy envelope: malformed denied actions field")
        if isinstance(event.get("denied_actions"), list) and event.get(
            "denied_actions"
        ):
            errors.append("denied_actions " + json.dumps(event["denied_actions"]))
        if kind == "result":
            if event.get("is_error") is False:
                retry_errors.clear()
            result["terminal"] = True
            text = event.get("result", event.get("text", event.get("response")))
            if isinstance(text, str):
                terminal_text = text
        if kind in {"turn.completed", "turn.failed", "session.completed", "done"}:
            result["terminal"] = True
        if kind == "assistant":
            content = (
                event.get("message", {}).get("content", [])
                if isinstance(event.get("message"), dict)
                else []
            )
            if isinstance(content, str):
                parts.append(content)
            elif isinstance(content, list):
                parts.extend(
                    item["text"]
                    for item in content
                    if isinstance(item, dict)
                    and item.get("type") == "text"
                    and isinstance(item.get("text"), str)
                )
        if kind == "item.completed" and isinstance(event.get("item"), dict):
            item = event["item"]
            if item.get("type") == "agent_message" and isinstance(
                item.get("text"), str
            ):
                parts.append(item["text"])
        if kind in {"text", "text_delta", "assistant_message"}:
            text = event.get("text")
            if isinstance(event.get("part"), dict):
                text = event["part"].get("text", text)
            if isinstance(text, str):
                parts.append(text)
        if (
            kind == "step_finish"
            and isinstance(event.get("part"), dict)
            and event["part"].get("reason") == "stop"
        ):
            result["terminal"] = True
        # agy prints its envelope flat or nested under a `result` event.
        envelope = (
            event["result"]
            if adapter == "agy" and isinstance(event.get("result"), dict)
            else event
        )
        if adapter == "agy" and "status" in envelope and "response" in envelope:
            provider_status = envelope.get("status")
            response = envelope.get("response")
            error = envelope.get("error")
            if not isinstance(provider_status, str) or not isinstance(response, str):
                errors.append("invalid agy envelope")
                continue
            result["terminal"] = True
            terminal_text = response
            if provider_status.upper() == "SUCCESS" and exit_code != 0 and not error:
                errors.append("provider exited " + str(exit_code) + " despite SUCCESS")
            if provider_status.upper() != "SUCCESS" or error:
                if "timeout" in str(error).lower() or provider_status.upper() in {
                    "TIMEOUT",
                    "PARTIAL",
                }:
                    partial = bool(response.strip())
                    if not partial:
                        errors.append("print-timeout")
                else:
                    errors.append(
                        str(
                            error
                            or "provider returned a non-success status without an error message"
                        )
                    )
    if adapter == "agy":
        match = re.search(r'(?:using model|model[=:])\s*["\']?([\w./-]+)', stderr, re.I)
        if match and not result["observed_model"]:
            result["observed_model"] = match[1]
        if "AGY_ERROR" in stderr:
            errors.append(stderr)
        if re.search("print.?timeout", stderr, re.I):
            partial = bool(terminal_text or parts)
    result["text"] = (
        terminal_text
        if terminal_text is not None
        else "\n".join(parts)
        if parts
        else "".join(plain)
    )
    errors.extend(retry_errors)
    failure_text = (
        "\n".join(errors)
        if errors
        else stderr
    )
    # Permission denials in diagnostics invalidate a claimed success (Agy does this).
    denial = adapter == "agy" and re.search(SIGNATURES[0][1], stderr, re.I)
    if denial:
        errors.insert(0, stderr)
        failure_text = stderr + "\n" + failure_text
    status = None
    if errors or exit_code != 0 or denial:
        for name, pattern in (*config.SIGNATURES, *SIGNATURES):
            if re.search(pattern, failure_text, re.I):
                status = name
                result["signature"] = name
                break
        status = status or (
            "timed_out" if "print-timeout" in failure_text else "failed"
        )
        result["excerpt"] = failure_text.strip()[:200]
    elif partial:
        status = "partial"
        result["signature"] = "print_timeout"
    elif result["text"].strip():
        status = "ok"
    else:
        status = "failed"
        result["signature"] = "empty_output"
        result["excerpt"] = ("no assistant text: " + stdout.strip())[:200]
    if status == "ok":
        question = re.search(
            r"```(?:[^\n`]*\n)?\s*QUESTION:\s*((?:(?!```).)*?)\s*```\s*\Z", result["text"], re.S
        )
        if question and question[1].strip():
            result["question"] = question[1].strip()[:4096]
            status = "input_required"
    result["status"] = status
    result["reset_at"] = reset_time(failure_text, events, at)
    return result


MAX_EVENTS_BYTES = 20 * 1024 * 1024


class BoundedCapture:
    """Keep a fixed head and rolling tail on disk; never discard the diagnostics file."""

    def __init__(self, path, limit=None):
        limit = MAX_EVENTS_BYTES if limit is None else limit
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
        self.tail = deque()
        self.tail_size = 0

    def write(self, data):
        # Append to disk only until the cap. Keep subsequent tail chunks in a
        # deque: every byte is copied a bounded number of times, even for JSONL.
        room = max(0, self.limit - self.total)
        if room:
            self.file.write(data[:room])
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
        if self.total > self.limit:
            self.file.seek(self.limit // 2)
            self.file.write(b"".join(self.tail))
            self.file.truncate(self.limit)

    def text(self):
        self._flush_tail()
        self.file.seek(0)
        return self.file.read().decode("utf-8", errors="replace")

    def close(self):
        self._flush_tail()
        self.file.close()


@dataclass(frozen=True)
class _ProcessRow:
    pid: int
    ppid: int
    pgid: int
    started: str
    command: str
    zombie: bool = False

    @property
    def identity(self):
        return self.pid, self.started


class _DarwinBsdInfo(ctypes.Structure):
    _fields_ = [
        ("flags", ctypes.c_uint32), ("status", ctypes.c_uint32),
        ("xstatus", ctypes.c_uint32), ("pid", ctypes.c_uint32),
        ("ppid", ctypes.c_uint32), ("uid", ctypes.c_uint32),
        ("gid", ctypes.c_uint32), ("ruid", ctypes.c_uint32),
        ("rgid", ctypes.c_uint32), ("svuid", ctypes.c_uint32),
        ("svgid", ctypes.c_uint32), ("reserved", ctypes.c_uint32),
        ("comm", ctypes.c_char * 16), ("name", ctypes.c_char * 32),
        ("nfiles", ctypes.c_uint32), ("pgid", ctypes.c_uint32),
        ("pjobc", ctypes.c_uint32), ("tdev", ctypes.c_uint32),
        ("tpgid", ctypes.c_uint32), ("nice", ctypes.c_int32),
        ("start_sec", ctypes.c_uint64), ("start_usec", ctypes.c_uint64),
    ]


def _process_snapshot():
    if sys.platform == "darwin":
        library = ctypes.util.find_library("proc")
        if not library:
            return {}
        try:
            libproc = ctypes.CDLL(library, use_errno=True)
        except OSError:
            return {}
        libproc.proc_listallpids.argtypes = (ctypes.c_void_p, ctypes.c_int)
        libproc.proc_listallpids.restype = ctypes.c_int
        libproc.proc_pidinfo.argtypes = (
            ctypes.c_int, ctypes.c_int, ctypes.c_uint64, ctypes.c_void_p, ctypes.c_int,
        )
        libproc.proc_pidinfo.restype = ctypes.c_int
        count = libproc.proc_listallpids(None, 0)
        if count <= 0:
            return {}
        pids = (ctypes.c_int * (count + 128))()
        count = libproc.proc_listallpids(pids, ctypes.sizeof(pids))
        rows = {}
        for pid in pids[:max(0, count)]:
            if pid <= 0:
                continue
            info = _DarwinBsdInfo()
            if libproc.proc_pidinfo(pid, 3, 0, ctypes.byref(info), ctypes.sizeof(info)) != ctypes.sizeof(info):
                continue
            rows[pid] = _ProcessRow(
                pid, info.ppid, info.pgid,
                f"{info.start_sec}.{info.start_usec:06d}",
                info.name.split(b"\0", 1)[0].decode(errors="replace")
                or info.comm.split(b"\0", 1)[0].decode(errors="replace"),
                info.status == 5,
            )
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
            try:
                stat = Path(entry.path, "stat").read_text()
            except OSError:
                continue
            end = stat.rfind(") ")
            if end < 0:
                continue
            fields = stat[end + 2:].split()
            if len(fields) < 20:
                continue
            pid = int(entry.name)
            rows[pid] = _ProcessRow(
                pid, int(fields[1]), int(fields[2]), fields[19],
                stat[stat.find("(") + 1:end], fields[0] == "Z",
            )
    return rows


def _has_attempt_marker(pid, marker):
    expected = ("PROVENANT_ATTEMPT_MARKER=" + marker).encode()
    if sys.platform.startswith("linux"):
        try:
            return expected in Path(f"/proc/{pid}/environ").read_bytes().split(b"\0")
        except OSError:
            return False
    if sys.platform != "darwin":
        return False
    libc = ctypes.CDLL(None, use_errno=True)
    mib = (ctypes.c_int * 3)(1, 49, pid)  # CTL_KERN, KERN_PROCARGS2, pid
    size = ctypes.c_size_t()
    if libc.sysctl(mib, 3, None, ctypes.byref(size), None, 0) != 0 or size.value > 1024 * 1024:
        return False
    data = ctypes.create_string_buffer(size.value)
    if libc.sysctl(mib, 3, data, ctypes.byref(size), None, 0) != 0:
        return False
    # KERN_PROCARGS2 stores argc, executable path, argv, then NUL-separated env.
    raw = data.raw[:size.value]
    if len(raw) < ctypes.sizeof(ctypes.c_int):
        return False
    argc = ctypes.c_int.from_buffer_copy(raw).value
    if not 0 <= argc <= 65536:
        return False
    offset = raw.find(b"\0", ctypes.sizeof(ctypes.c_int))
    if offset < 0:
        return False
    while offset < len(raw) and raw[offset] == 0:
        offset += 1
    for _ in range(argc):
        offset = raw.find(b"\0", offset)
        if offset < 0:
            return False
        offset += 1
    return expected in raw[offset:].split(b"\0")


class _Descendants:
    def __init__(self, process, marker):
        self.process = process
        self.marker = marker
        self.spawned_at = time.time() - 1
        self.spawned_ticks = None
        if sys.platform.startswith("linux"):
            try:
                ticks_per_second = os.sysconf("SC_CLK_TCK")
                uptime = float(Path("/proc/uptime").read_text().split()[0])
                self.spawned_ticks = int((uptime - 1) * ticks_per_second)
            except (OSError, ValueError):
                pass
        self.root = None
        self.tracked = {}
        self.snapshot_unavailable = False

    def sample(self, include_reparented=False):
        rows = _process_snapshot()
        if os.getpid() not in rows:
            self.snapshot_unavailable = True
        root = rows.get(self.process.pid)
        if self.root is None and root is not None and self.process.poll() is None:
            self.root = root.identity
        parents = {self.root} if self.root else set()
        parents.update(self.tracked)
        discovered = {}
        while True:
            newly = {
                row.identity: row for row in rows.values()
                if row.identity not in parents
                and (parent := rows.get(row.ppid)) is not None
                and parent.identity in parents
            }
            if not newly:
                break
            discovered.update(newly)
            parents.update(newly)
        self.tracked.update(discovered)
        if include_reparented:
            for row in rows.values():
                if (row.ppid not in {1, os.getpid()} or row.pid == self.process.pid
                    or row.identity in self.tracked or row.zombie):
                    continue
                if sys.platform == "darwin" and float(row.started) < self.spawned_at:
                    continue
                if sys.platform.startswith("linux") and (
                    self.spawned_ticks is None or int(row.started) < self.spawned_ticks
                ):
                    continue
                if _has_attempt_marker(row.pid, self.marker):
                    self.tracked[row.identity] = row
        return rows

    def live(self, rows):
        return {
            identity: row for identity in (set(self.tracked) | ({self.root} if self.root else set()))
            if (row := rows.get(identity[0])) is not None
            and row.identity == identity and not row.zombie
        }

    def signal(self, signum):
        rows = _process_snapshot()
        live = self.live(rows)
        groups = {row.pgid for row in live.values() if row.pgid > 0 and row.pgid != os.getpgrp()}
        if self.process.poll() is None:
            groups.add(self.process.pid)  # Popen still proves the direct child.
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
        if self.process.poll() is None and (
            signum != signal.SIGTERM or self.process.pid not in signalled_groups
        ):
            try:
                os.kill(self.process.pid, signum)
            except (ProcessLookupError, PermissionError):
                pass

    def stop(self, grace=0.5):
        rows = self.sample(include_reparented=True)
        leftovers = {
            identity: {"pid": row.pid, "command": row.command[:80]}
            for identity, row in self.live(rows).items() if identity != self.root
        }
        self.signal(signal.SIGTERM)
        deadline = time.monotonic() + grace
        while time.monotonic() < deadline:
            self.process.poll()
            rows = self.sample(include_reparented=True)
            leftovers.update({
                identity: {"pid": row.pid, "command": row.command[:80]}
                for identity, row in self.live(rows).items() if identity != self.root
            })
            if not self.live(rows):
                break
            time.sleep(0.05)
        rows = self.sample(include_reparented=True)
        leftovers.update({
            identity: {"pid": row.pid, "command": row.command[:80]}
            for identity, row in self.live(rows).items() if identity != self.root
        })
        self.signal(signal.SIGKILL)
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
        return list(leftovers.values())


def _enable_subreaper():
    if not sys.platform.startswith("linux"):
        return
    try:
        libc = ctypes.CDLL(None, use_errno=True)
        libc.prctl(36, 1, 0, 0, 0)  # PR_SET_CHILD_SUBREAPER
    except (AttributeError, OSError):
        pass


def _workspace_stamp(cwd, excluded=()):
    modified, size, count = 0, 0, 0
    for root, dirs, files in os.walk(cwd):
        dirs[:] = [
            d
            for d in dirs
            if d
            not in {
                ".git",
                ".agent-run",
                ".worktrees",
                "node_modules",
                ".venv",
                "__pycache__",
            }
            and not Path(root, d).is_symlink()
        ]
        for name in files:
            path = Path(root, name)
            if path in excluded:
                continue
            try:
                metadata = path.lstat()
                modified += metadata.st_mtime_ns
                size += metadata.st_size
            except OSError:
                continue
            count += 1
    return modified, size, count


def _cpu_stamp(pgid):
    try:
        rows = subprocess.run(
            ["/bin/ps", "-axo", "pgid=,time="],
            capture_output=True,
            text=True,
            timeout=0.5,
        ).stdout
        return tuple(
            line.split(None, 1)[1]
            for line in rows.splitlines()
            if len(line.split(None, 1)) == 2 and line.split(None, 1)[0] == str(pgid)
        )
    except (OSError, subprocess.SubprocessError):
        return ()


def _same_model(adapter, resolved, observed):
    """Aliases and display names name the same model: opus is claude-opus-5-5,
    and Cursor reports grok-4.7 as "Grok 4.7 256K High Fast"."""
    if not resolved or not observed or resolved == observed:
        return True
    norm = lambda value: re.sub(r"[\s_]+", "-", str(value).strip().lower())
    # Only a display name (spaces, e.g. Cursor) may extend the id with
    # descriptors; an id-shaped suffix such as -thinking is a different model.
    if " " in str(observed).strip() and norm(observed).startswith(norm(resolved) + "-"):
        return True
    try:
        try:
            from .exec_routing import registered_model
        except ImportError:
            from exec_routing import registered_model
        return registered_model(adapter, resolved) == registered_model(adapter, observed)
    except Exception:  # A malformed catalogue must not fail a finished attempt.
        return False


def _observed_model(plan, parsed, env):
    adapter, session = plan["adapter"], parsed["session_id"]
    if adapter == "codex" and session and re.fullmatch(r"[\w-]{1,128}", session):
        root = Path(env.get("CODEX_HOME") or Path.home() / ".codex") / "sessions"
        # File names contain thread_id; never inspect auth/config stores.
        for path in sorted(root.glob("**/*" + session + "*.jsonl"), reverse=True):
            try:
                observed = None
                with path.open() as stream:
                    for line in stream:
                        try:
                            event = json.loads(line)
                        except ValueError:
                            continue
                        if (
                            isinstance(event, dict)
                            and event.get("type") == "turn_context"
                        ):
                            payload = event.get("payload", {})
                            if isinstance(payload, dict) and isinstance(
                                payload.get("model"), str
                            ):
                                observed = payload["model"]
                if observed:
                    return observed, profile(adapter).MODEL_SOURCE
            except OSError:
                continue
    elif adapter == "opencode" and session:
        try:
            exported = subprocess.run(
                [profile(adapter).CLI, "export", session],
                capture_output=True,
                text=True,
                timeout=5,
                env=env,
                cwd=plan["cwd"],
                stdin=subprocess.DEVNULL,
            )
            value = json.loads(exported.stdout)
            observed = [
                (item.get("providerID"), item["modelID"])
                for item in _objects(value)
                if isinstance(item.get("modelID"), str)
            ]
            if observed:
                provider, model = observed[-1]
                return (
                    provider + "/" + model if provider and "/" not in model else model
                ), profile(adapter).MODEL_SOURCE
        except (OSError, ValueError, subprocess.SubprocessError):
            pass
    if parsed["observed_model"]:
        return parsed["observed_model"], profile(adapter).MODEL_SOURCE
    return None, None


def execute(
    plan,
    output_path,
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

    output_path = Path(output_path)
    events_path = Path(events_path or output_path.parent / "events.jsonl")
    stderr_path = Path(stderr_path or output_path.parent / "stderr.log")
    environment = dict(os.environ if env is None else env)
    for key in list(environment):
        if key.startswith(
            ("GIT_", "PROVENANT_RUN_", "PROVENANT_PREFLIGHT_")
        ) or key in {
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
    )
    attempt_marker = uuid.uuid4().hex
    environment["PROVENANT_ATTEMPT_MARKER"] = attempt_marker
    environment["CLAUDE_CODE_DISABLE_WORKFLOWS"] = "1"
    # Linux CI and service shells may have no TMPDIR; providers expect one.
    environment.setdefault("TMPDIR", tempfile.gettempdir())
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
    if plan["adapter"] == "opencode":
        permission = {
            "edit": {"*": "allow" if plan["mode"] == "worktree_write" else "deny"},
            "bash": {"*": "allow" if plan["mode"] == "worktree_write" else "deny"},
            "question": "deny",
            "external_directory": {"*": "deny"},
            "webfetch": "deny" if plan.get("network_requested") is False else "allow",
        }
        # No shell-pattern allowlist: "git diff; write" must remain denied on reads.
        environment["OPENCODE_CONFIG_CONTENT"] = json.dumps({"permission": permission})
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
    process = None
    descendants = None
    reaped = []
    stopped = False
    forced, terminal_at, cancel_signal = None, None, False
    pending = b""
    dropping_line = False
    semantic = {}
    text_chunks, text_size = deque(), 0
    semantic_failures = {}
    retry_failure = None
    terminal_text = None
    text_truncated = False
    old_handlers = {}

    def consume(data):
        nonlocal pending, terminal_at, text_size, retry_failure, terminal_text, text_truncated, dropping_line
        if dropping_line:
            if b"\n" not in data:
                return
            data = data.split(b"\n", 1)[1]
            dropping_line = False
        pending += data
        while b"\n" in pending:
            line, pending = pending.split(b"\n", 1)
            try:
                event = json.loads(line)
            except (ValueError, UnicodeDecodeError):
                continue
            if not isinstance(event, dict):
                continue
            parsed_line = parse_output(plan["adapter"], line.decode(errors="replace"))
            for key in ("session_id", "observed_model", "reset_at", "retry_after"):
                if parsed_line.get(key) is not None:
                    semantic[key] = parsed_line[key]
            if parsed_line["terminal"] and terminal_at is None:
                terminal_at = time.monotonic()
            if parsed_line["status"] not in {"ok", "input_required"} and parsed_line["signature"] != "empty_output":
                failure = {key: parsed_line[key] for key in ("status", "signature", "excerpt", "reset_at", "retry_after")}
                if event.get("type") == "api_retry":
                    retry_failure = failure
                else:
                    semantic_failures[parsed_line["status"]] = failure
            elif event.get("type") == "result" and event.get("is_error") is False:
                retry_failure = None
            if parsed_line["text"]:
                encoded = parsed_line["text"].encode()
                text_truncated |= len(encoded) > MAX_EVENTS_BYTES
                chunk = encoded[:MAX_EVENTS_BYTES]
                if event.get("type") == "result" or (plan["adapter"] == "agy" and "status" in event):
                    terminal_text = chunk.decode(errors="replace")
                else:
                    text_chunks.append(chunk)
                    text_size += len(chunk)
                    while text_size > MAX_EVENTS_BYTES:
                        text_truncated = True
                        text_size -= len(text_chunks.popleft())
        if len(pending) > MAX_EVENTS_BYTES:
            # Do not parse a clipped JSON suffix or certify preliminary text
            # when the final result event exceeded the semantic buffer.
            pending = b""
            dropping_line = True
            text_truncated = True


    def handle_signal(signum, frame):
        nonlocal cancel_signal
        cancel_signal = True

    if threading.current_thread() is threading.main_thread():
        old_handlers = {
            sig: signal.getsignal(sig)
            for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)
        }
        for sig in old_handlers:
            signal.signal(sig, handle_signal)
    input_file = None
    selector = selectors.DefaultSelector()
    try:
        if plan["stdin_policy"] == "prompt":
            input_file = tempfile.TemporaryFile()
            input_file.write(plan["prompt"].encode())
            input_file.seek(0)
            stdin = input_file
        else:
            stdin = (
                subprocess.PIPE
                if plan["stdin_policy"] == "pipe"
                else subprocess.DEVNULL
            )
        if plan.get("cooldown_blocked"):
            forced = "usage_limited"
            diagnostics.write(b"all alias candidates cooling\n")
        elif cancelled and cancelled():
            forced = "cancelled"
        elif (
            plan["adapter"] == "copilot"
            and environment.get("CF_DISPATCH_ENABLE_COPILOT") != "1"
        ):
            forced = "rejected"
            diagnostics.write(b"copilot requires CF_DISPATCH_ENABLE_COPILOT=1\n")
        else:
            command = list(plan["argv"])
            command[0] = (
                shutil.which(command[0], path=environment.get("PATH")) or command[0]
            )
            _enable_subreaper()
            process = subprocess.Popen(
                command,
                cwd=plan["cwd"],
                env=environment,
                stdin=stdin,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
            )
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
            warned = False
            excluded = {
                path.resolve() for path in (output_path, events_path, stderr_path)
            }
            stamp = (
                _workspace_stamp(plan["cwd"], excluded)
                if plan["mode"] == "worktree_write"
                else None
            )
            cpu, next_sample = (), started + 1
            next_tree_sample = started + 0.25
            while True:
                for key, mask in selector.select(0.05):
                    data = os.read(key.fileobj.fileno(), 65536)
                    if not data:
                        selector.unregister(key.fileobj)
                        continue
                    if key.data == "stdout":
                        raw.write(data)
                        consume(data)
                    else:
                        diagnostics.write(data)
                    last_progress = time.monotonic()
                    last_progress_at = now()
                    warned = False
                    if on_progress:
                        on_progress(last_progress_at)
                current = time.monotonic()
                if current >= next_tree_sample:
                    descendants.sample()
                    next_tree_sample = current + 0.25
                exited = process.poll() is not None
                if exited and not selector.get_map():
                    break
                if exited:
                    reaped = descendants.stop()  # descendants may hold output pipes
                    stopped = True
                if cancel_signal or (cancelled and cancelled()):
                    forced = "cancelled"
                    break
                if (
                    terminal_at is not None
                    and current - terminal_at >= plan["grace_seconds"]
                ):
                    # Completion is evidenced by the event, even when the CLI hangs.
                    break
                if current - started >= plan["timeout_seconds"]:
                    forced = "timed_out"
                    break
                if current >= next_sample:
                    next_sample = current + 1
                    new_stamp = (
                        _workspace_stamp(plan["cwd"], excluded)
                        if plan["mode"] == "worktree_write"
                        else None
                    )
                    new_cpu = _cpu_stamp(process.pid)
                    if new_stamp != stamp or (cpu and new_cpu and new_cpu != cpu):
                        last_progress = current
                        last_progress_at = now()
                        warned = False
                        if on_progress:
                            on_progress(last_progress_at)
                    stamp, cpu = new_stamp, new_cpu
                idle = current - last_progress
                if idle >= plan["idle_seconds"]:
                    forced = "stalled"
                    break
                if idle >= plan["idle_seconds"] / 2 and not warned:
                    raw.write(
                        (
                            json.dumps(
                                {
                                    "type": "fabric.idle_warning",
                                    "silent_seconds": round(idle, 3),
                                }
                            )
                            + "\n"
                        ).encode()
                    )
                    warned = True
    except FileNotFoundError as exc:
        forced = "tool_missing"
        diagnostics.write(str(exc).encode())
    except OSError as exc:
        forced = "failed"
        diagnostics.write(str(exc).encode())
    finally:
        if process:
            if descendants and not stopped:
                reaped = descendants.stop()
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
                        consume(data)
            for stream in (process.stdin, process.stdout, process.stderr):
                if stream:
                    stream.close()
        selector.close()
        if input_file:
            input_file.close()
        for sig, handler in old_handlers.items():
            signal.signal(sig, handler)
    exit_code = process.returncode if process else None
    if pending:
        consume(b"\n")
    if forced == "stalled":
        diagnostics.write(
            f"idle for {plan['idle_seconds']:g}s; try another model\n".encode()
        )
    elif forced == "timed_out":
        diagnostics.write(b"wall clock deadline exceeded\n")
    stdout, stderr = (
        raw.text(),
        diagnostics.text(),
    )
    raw.close()
    diagnostics.close()
    parsed = parse_output(
        plan["adapter"],
        stdout,
        stderr,
        0
        if terminal_at is not None
        and not forced
        and exit_code is not None
        and exit_code < 0
        else (exit_code if exit_code is not None else 1),
    )
    parsed.update(semantic)
    if raw.total > MAX_EVENTS_BYTES:
        # The complete stream is never buffered or reread. Preserve semantic
        # state observed before the diagnostic tail rolled over.
        if terminal_text is not None or text_chunks:
            text = terminal_text if terminal_text is not None else b"\n".join(text_chunks).decode(errors="replace")
            recovered = parse_output(plan["adapter"], json.dumps({"type": "result", "result": text}), stderr,
                                     0 if terminal_at is not None and exit_code is not None and exit_code < 0 else (exit_code or 0))
            for key in ("text", "status", "question", "signature", "excerpt"):
                parsed[key] = recovered[key]
        failures = dict(semantic_failures)
        if retry_failure:
            failures.setdefault(retry_failure["status"], retry_failure)
        if failures:
            priority = [status for status, _ in (*profile(plan["adapter"]).SIGNATURES, *SIGNATURES)]
            selected = next((failures[status] for status in priority if status in failures), next(iter(failures.values())))
            parsed.update(selected)
    if forced:
        parsed["status"] = forced
        parsed["signature"] = {
            "stalled": "idle_watchdog",
            "timed_out": "wall_clock",
            "cancelled": "cancel_requested",
        }.get(forced, parsed["signature"])
    if plan.get("cooldown_blocked"):
        parsed["reset_at"] = plan["cooldown_blocked"]["cooling_until"]
        parsed["signature"] = "cooldown_active"
    session = parsed["session_id"] or plan["resume_session"] or plan["session_id"]
    parsed["session_id"] = session
    observed, source = _observed_model(plan, parsed, environment)
    warnings = list(plan["warnings"])
    if descendants and descendants.snapshot_unavailable:
        warnings.insert(0, "descendant census unavailable; process cleanup could not be verified")
    if reaped:
        warnings.insert(0, f"reaped {len(reaped)} leftover process(es)")
    if route.get("effort_substitution") and route["effort_substitution"] not in warnings:
        warnings.append(route["effort_substitution"])
    if text_truncated or (raw.total > MAX_EVENTS_BYTES and terminal_text is None and not text_chunks):
        warnings.append("result truncated at capture limit; inspect provider session for complete output")
        if parsed["status"] in {"ok", "input_required"}:
            parsed.update(status="partial", question=None, signature="output_truncated")
    notes = list(route.get("notes") or [])
    substituted = bool(observed) and not _same_model(plan["adapter"], plan["model"], observed)
    if substituted:
        notes.append("mismatch: resolved " + plan["model"] + "; observed " + observed)
        warnings.append(notes[-1])
        if plan["adapter"] == "agy" and plan.get("requested_model"):
            parsed["status"] = "model_unavailable"
            parsed["signature"] = "pinned_model_substitution"
    identity = "observed" if observed else "resolved" if plan["model"] else "unknown"
    family = route.get("model_family") or route.get("family") or "unknown"
    if substituted:
        # Routing attributed the requested model, not the provider's substitution.
        # Do not certify a different family without model-router evidence for it.
        family = "unknown"
        notes.append("observed model family unverified after substitution")
    model = observed or plan["model"]
    line = (
        f"Route: {plan['adapter']}/{model}"
        + ("@" + plan["effort"] if plan["effort"] else "")
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
        "identity": identity,
        "provider": route.get("endpoint_provider") or plan["adapter"],
        "transport": plan["adapter"],
        "family": family,
        "effort_requested": plan.get("requested_effort"),
        "effort_applied": plan["effort"],
        "cli_version": route.get("cli_version"),
        "fallback_from": plan.get("fallback_from"),
        "notes": notes,
        "line": line,
    }
    status = parsed["status"]
    if plan["adapter"] == "agy" and status not in {"ok", "input_required", "partial"}:
        parsed["text"] = ""
        stderr = "agy dispatch failed: status=" + status + "\n" + stderr
        if parsed["excerpt"]:
            parsed["excerpt"] = "provider error: " + parsed["excerpt"]
    output = (
        parsed["text"]
        if status in {"ok", "input_required", "partial"}
        else (parsed["text"] + "\n" if parsed["text"] else "")
        + stderr
        + ("\n" + parsed["excerpt"] if parsed["excerpt"] else "")
    )
    digest_value = ""
    output_value = ""
    try:
        with tempfile.NamedTemporaryFile() as staged:
            staged.write(output.encode())
            staged.flush()
            digest_value, device, inode = install(staged.name, str(output_path))
            verify(str(output_path), digest_value, device, inode)
        output_value = str(output_path)
    except (OSError, CustodyError):
        status = "output_identity_invalid" if digest_value else "output_write_error"
        digest_value = ""
    cross = bool(
        plan.get("orchestrator_family")
        and family not in {"unknown", "generic-open", "open-weight"}
        and family != plan["orchestrator_family"]
    )
    guarantee = plan["applied"]["guarantee"]
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
        "execution_intent": plan["intent"],
        "model": plan["model"],
        "requested_model": plan.get("requested_model") or plan["model"],
        "resolved_model": plan["model"],
        "effort": plan["effort"],
        "status": status,
        "reason": parsed["excerpt"],
        "exit": 0
        if status in {"ok", "input_required"} and terminal_at is not None
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
        and cross
        and plan["mode"] == "read_only"
        and guarantee == "enforced",
        "session_id": session,
        "provenance": provenance,
        "applied": plan["applied"],
        "warnings": warnings,
        "reaped": reaped,
        "question": parsed["question"],
        "retryable": status
        in {"usage_limited", "rate_limited", "model_unavailable", "stalled"},
        "reset_at": parsed["reset_at"],
        "retry_after": parsed["retry_after"],
        "fix": {
            "auth_required": "authenticate the provider CLI",
            "model_unavailable": "choose another model",
            "permission_blocked": "check the requested sandbox and directory grants",
            "tool_missing": "install the provider CLI",
            "stalled": "inspect events or try another model",
            "usage_limited": "wait for reset or choose another model",
            "rate_limited": "retry after the recorded cooldown",
        }.get(status),
        "evidence": {
            "exit": exit_code,
            "signal": -exit_code if exit_code and exit_code < 0 else None,
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


def parser():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--route-file", type=Path, required=True)
    p.add_argument("--adapter", required=True)
    p.add_argument("--prompt-file", type=Path, required=True)
    p.add_argument("--out", type=Path, required=True)
    p.add_argument("--plan-only", action="store_true")
    p.add_argument("--mode", default="read_only")
    p.add_argument("--cwd", type=Path)
    p.add_argument("--worktree")
    p.add_argument("--sandbox")
    p.add_argument("--network", choices=["true", "false"])
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
    p.add_argument("--no-preface", action="store_true")
    p.add_argument("--cleanup-dir", type=Path)
    p.add_argument("--cleanup-prompt", action="store_true")
    return p


def main():
    args = parser().parse_args()
    writer_lease = None
    try:
        if args.cwd and not args.cwd.expanduser().resolve().is_relative_to(
            Path.cwd().resolve()
        ):
            raise ValueError("cwd must be inside the current workspace")
        plan = build_plan(
            args.adapter,
            json.loads(args.route_file.read_text()),
            args.prompt_file.read_text(),
            mode=args.mode,
            cwd=args.cwd,
            worktree=args.worktree,
            sandbox=args.sandbox,
            network=None if args.network is None else args.network == "true",
            add_dirs=args.add_dir,
            timeout_seconds=args.timeout_seconds,
            intent=args.intent,
            preface=not args.no_preface,
            resume_session=args.resume_session,
            orchestrator_family=args.orchestrator_family,
            reviewer_id=args.reviewer_id,
            risk_tier=args.risk_tier,
            model_override_tier=args.model_override_tier,
            requested_model=args.requested_model,
            requested_effort=args.requested_effort,
            run_id=os.environ.get("PROVENANT_RUN_ID", ""),
            chair=os.environ.get("PROVENANT_CHAIR", ""),
        )
        plan["output_path"] = str(args.out.absolute())
        if args.plan_only:
            print(json.dumps(plan))
            return 0
        if args.mode == "worktree_write":
            # Direct CLI calls own the same registered-worktree lease as Fabric.
            from dispatch_run import resolve_writer_worktree, acquire_worktree_lease

            writer_lease = acquire_worktree_lease(
                resolve_writer_worktree(Path(args.worktree))
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
            from dispatch_run import release_worktree_lease

            release_worktree_lease(writer_lease)
        if args.cleanup_prompt:
            args.prompt_file.unlink(missing_ok=True)
        if args.cleanup_dir:
            shutil.rmtree(args.cleanup_dir, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
