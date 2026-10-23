"""Minimal stdio MCP client and transcript normaliser for the cross-language judge."""

from __future__ import annotations

import json
import os
from pathlib import Path
import re
import select
import shlex
import subprocess
import time
from typing import Any

from tests.support.front_door import front_door_binary


ROOT = Path(__file__).resolve().parents[2]
ENVIRONMENT_KEYS = (
    "AGENT_FABRIC_SEAT", "AGENT_FABRIC_STATE_DIRECTORY", "AGENT_FABRIC_INSTANCE_ROOT",
    "AGENT_FABRIC_PRODUCT_ROOT", "AGENT_FABRIC_LABEL",
)


def server_command() -> list[str]:
    override = os.environ.get("PROVENANT_MCP_SERVER_CMD")
    return shlex.split(override) if override else [str(front_door_binary())]


class McpProcess:
    def __init__(self, home: Path, project: Path, state: Path, extra_env: dict[str, str] | None = None):
        env = os.environ.copy()
        env.update({
            "HOME": str(home),
            "XDG_CONFIG_HOME": str(home / ".config"),
            "XDG_STATE_HOME": str(home / ".local/state"),
            "AGENT_FABRIC_SEAT": "codex",
            "AGENT_FABRIC_STATE_DIRECTORY": str(state),
            "AGENT_FABRIC_INSTANCE_ROOT": str(home / ".agents"),
            "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT),
        })
        env.update(extra_env or {})
        self.process = subprocess.Popen(
            server_command(), cwd=project, env=env,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            bufsize=0,
        )
        self.sequence = 0
        self.requests: list[dict[str, Any]] = []
        self.output = bytearray()
        self.initialization = self.request("initialize", {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": {"name": "provenant-mcp-judge", "version": "1"},
        })
        self.notify("notifications/initialized")

    def request(self, method: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        self.sequence += 1
        request = {"jsonrpc": "2.0", "id": self.sequence, "method": method}
        if params is not None:
            request["params"] = params
        self.requests.append(request)
        assert self.process.stdin and self.process.stdout
        self.process.stdin.write((json.dumps(request, separators=(",", ":")) + "\n").encode())
        self.process.stdin.flush()
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            newline = self.output.find(b"\n")
            if newline >= 0:
                line = bytes(self.output[:newline])
                del self.output[:newline + 1]
                reply = json.loads(line)
                if reply.get("id") == self.sequence:
                    return reply
                continue
            remaining = deadline - time.monotonic()
            readable, _, _ = select.select([self.process.stdout], [], [], remaining)
            if not readable:
                break
            chunk = os.read(self.process.stdout.fileno(), 65536)
            if not chunk:
                error = self.process.stderr.read() if self.process.stderr else ""
                raise AssertionError(f"MCP server exited before replying: {error[-2000:]}")
            self.output.extend(chunk)
        raise AssertionError(f"MCP request timed out: {method}")

    def notify(self, method: str, params: dict[str, Any] | None = None) -> None:
        notification = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            notification["params"] = params
        assert self.process.stdin
        self.process.stdin.write((json.dumps(notification, separators=(",", ":")) + "\n").encode())
        self.process.stdin.flush()

    def call(self, name: str, arguments: dict[str, Any]) -> dict[str, Any]:
        return self.request("tools/call", {"name": name, "arguments": arguments})

    def close(self) -> None:
        if self.process.poll() is None:
            if self.process.stdin:
                self.process.stdin.close()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.terminate()
                self.process.wait(timeout=5)
        if self.process.stderr:
            self.process.stderr.close()


def resolve_refs(value: Any, captures: dict[str, Any]) -> Any:
    if isinstance(value, dict) and set(value) == {"$ref"}:
        return captures[value["$ref"]]
    if isinstance(value, dict):
        return {key: resolve_refs(item, captures) for key, item in value.items()}
    if isinstance(value, list):
        return [resolve_refs(item, captures) for item in value]
    return value


def capture_path(value: Any, path: str) -> Any:
    for part in path.split("."):
        value = value[int(part)] if isinstance(value, list) else value[part]
    return value


_ISO_TIME = re.compile(r"\b\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?Z\b")
_UUID = re.compile(r"\b[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\b", re.I)
_EPOCH_MS = re.compile(r"\b1\d{12}\b")
_SHA = re.compile(r"\b(?:[0-9a-f]{40}|[0-9a-f]{64})\b", re.I)


def normalize(value: Any, root: Path) -> Any:
    if isinstance(value, dict):
        result = {}
        for key, item in value.items():
            if key in {"pid", "pgid", "owner_pid", "host_pid", "provider_pid"} and item is not None:
                result[key] = "<PID>"
            elif key in {"run_token", "token", "claim_token"} and item is not None:
                result[key] = "<TOKEN>"
            elif key.endswith("AtMs") and item is not None:
                result[key] = "<TIME_MS>"
            elif key == "id" and "jsonrpc" in value:
                result[key] = "<RPC-ID>"
            else:
                result[key] = normalize(item, root)
        return result
    if isinstance(value, list):
        items = [normalize(item, root) for item in value]
        if items and all(isinstance(item, dict) and isinstance(item.get("name"), str) for item in items):
            return sorted(items, key=lambda item: item["name"])
        return items
    if isinstance(value, str):
        rooted = value.replace(str(root.resolve()), "<ROOT>").replace(str(root), "<ROOT>")
        return _SHA.sub("<SHA>", _UUID.sub("<UUID>", _EPOCH_MS.sub("<TIME_MS>", _ISO_TIME.sub("<TIME>", rooted))))
    return value


def seed_run(project: Path) -> None:
    run = project / ".agent-run/runs/20261023-0547-dispatch-fixture"
    result = run / "tasks/fixture-task/attempt-001/result.md"
    result.parent.mkdir(parents=True, exist_ok=True)
    result.write_text("fixture result\n", encoding="utf-8")
    (run / "tasks/fixture-task/attempt-001/attempt.json").write_text(json.dumps({
        "schema": "fabric.attempt.v1", "run_id": "fixture-run", "task_id": "fixture-task",
        "attempt": 1, "state": "terminal", "status": "ok", "mode": "read_only",
        "started_at": "2026-01-01T00:00:00.000Z", "ended_at": "2026-01-01T00:00:01.000Z",
        "paths": {"result": "tasks/fixture-task/attempt-001/result.md"},
        "provenance": {"requested": {"adapter": "fixture"}, "resolved_model": "fixture"},
    }), encoding="utf-8")
    (run / "dispatch-status.json").write_text(json.dumps({
        "schema": "fabric.status.v1", "id": "fixture-run", "task_id": "fixture-task",
        "task_ids": ["fixture-task"], "status": "completed", "kind": "dispatch",
        "started_at": "2026-01-01T00:00:00.000Z", "finished_at": "2026-01-01T00:00:01.000Z",
        "routes": [{"adapter": "fixture", "resolved_model": "fixture"}],
    }), encoding="utf-8")


def seed_hosts(home: Path, peers: dict[str, dict[str, str]] | None = None) -> Path:
    config = home / ".agents/.agent-fabric/hosts.json"
    config.parent.mkdir(parents=True, exist_ok=True)
    config.write_text(json.dumps({"schema_version": 1, "local_host": "judge-local", "peers": peers or {}}), encoding="utf-8")
    return config


def seed_fetch_fixture(home: Path, project: Path, state: Path) -> dict[str, str]:
    """Create a synthetic writer result and local fake peer; never runs real SSH."""
    import base64
    import hashlib
    import sqlite3

    seed_hosts(home, {"judge-peer": {"ssh_destination": "fixture-peer"}})
    git = ["git", "-C", str(project)]
    subprocess.run(["git", "init", "-q", "-b", "main", str(project)], check=True)
    (project / "fixture-source.txt").write_text("base\n", encoding="utf-8")
    subprocess.run([*git, "add", "fixture-source.txt"], check=True)
    git_env = {**os.environ, "GIT_CONFIG_GLOBAL": os.devnull}
    subprocess.run([*git, "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "base"], check=True, env=git_env)
    base = subprocess.check_output([*git, "rev-parse", "HEAD"], text=True).strip()
    (project / "fixture-source.txt").write_text("verified result\n", encoding="utf-8")
    subprocess.run([*git, "add", "fixture-source.txt"], check=True)
    subprocess.run([*git, "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "result"], check=True, env=git_env)
    head = subprocess.check_output([*git, "rev-parse", "HEAD"], text=True).strip()

    host_root = home / ".agents/.agent-fabric"
    project_id = project.relative_to(home).as_posix()
    code_dir = host_root / "code" / hashlib.sha256(project_id.encode()).hexdigest() / "writer-once-0"
    code_dir.mkdir(parents=True)
    metadata = {"base_revision": base, "branch": "feat/writer", "worktree": "feat-writer"}
    (code_dir / "outgoing.json").write_text(json.dumps({"host": "judge-peer", "operation_id": "writer-once", "metadata": metadata}), encoding="utf-8")
    bindings = [{"transfer_id": "writer-once-0", "input_operation_id": "writer-once", "task_id": "fixture-task", **metadata}]
    with sqlite3.connect(host_root / "hosts-state.sqlite3") as db:
        db.execute("CREATE TABLE writer_history (id TEXT PRIMARY KEY, project TEXT, host TEXT, bindings TEXT, result TEXT)")
        db.execute("INSERT INTO writer_history VALUES (?, ?, ?, ?, ?)", (
            "writer-once", project_id, "judge-peer", json.dumps(bindings),
            json.dumps({"id": "writer-once", "task_id": "fixture-task"}),
        ))

    result_id = hashlib.sha256(f"writer-once:writer-once-0:{head}".encode()).hexdigest()
    ref = f"refs/provenant/hosts/judge-peer/{result_id}/result"
    subprocess.run([*git, "update-ref", ref, head], check=True)
    bundle = state / "fetch-result.bundle"
    subprocess.run([*git, "bundle", "create", str(bundle), ref, f"^{base}"], check=True)
    raw = bundle.read_bytes()
    checksum = hashlib.sha256(raw).hexdigest()
    receipt = {
        "project_path": project_id, "operation_id": "writer-once", "input_operation_id": "writer-once",
        "transfer_id": "writer-once-0", "base_revision": base, "head_revision": head,
        "branch": "feat/writer", "worktree": "feat-writer", "worktree_path": str(project / ".worktrees/feat-writer"),
        "host": "judge-peer", "clean": True, "result_id": result_id,
    }
    fake_ssh = home / "bin/ssh"
    fake_ssh.parent.mkdir(parents=True)
    fake_ssh.write_text("""#!/usr/bin/env python3
import base64, json, os, sys
request = json.load(sys.stdin)
verb, params = request['verb'], request['params']
if verb == 'hello':
    result = {'protocol_version': 1, 'revision': None, 'host': 'judge-peer'}
elif verb == 'git-result':
    result = {'verification': json.loads(os.environ['MCP_JUDGE_FETCH_RECEIPT']),
              'size': os.path.getsize(os.environ['MCP_JUDGE_FETCH_BUNDLE']),
              'sha256': os.environ['MCP_JUDGE_FETCH_SHA256']}
else:
    offset = params['input']['offset']
    with open(os.environ['MCP_JUDGE_FETCH_BUNDLE'], 'rb') as bundle:
        bundle.seek(offset)
        data = bundle.read(512 * 1024)
    result = {'offset': offset, 'data': base64.b64encode(data).decode()}
print(json.dumps({'protocol_version': 1, 'ok': True, 'result': result}))
""", encoding="utf-8")
    fake_ssh.chmod(0o755)
    return {
        "PATH": f"{fake_ssh.parent}{os.pathsep}{os.environ.get('PATH', '')}",
        "AGENT_FABRIC_SSH_PROGRAM": str(fake_ssh),
        "MCP_JUDGE_FETCH_BUNDLE": str(bundle),
        "MCP_JUDGE_FETCH_SHA256": checksum,
        "MCP_JUDGE_FETCH_RECEIPT": json.dumps(receipt),
    }
