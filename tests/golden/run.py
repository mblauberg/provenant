#!/usr/bin/env python3
"""Record and compare CLI-boundary cases. Uses only the Python standard library."""

from __future__ import annotations

import argparse
import difflib
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import sqlite3
import socket
import subprocess
import sys
import tempfile
from typing import Any

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
CASES = HERE / "cases"

# Each entry has a representative boundary invocation. The detail case gives
# the public usage text; the second case exercises owner validation (or a
# harmless successful query for commands with a stable read-only path).
VERBS: dict[str, list[list[str]]] = {
    "help": [["help"], ["help", "bogus"]],
    "root": [["root"], ["root", "extra"]],
    "route": [["route", "--help"], ["route", "--bad-golden-flag"]],
    "routes": [["routes", "--help"], ["routes", "--bad-golden-flag"]],
    "capabilities": [["capabilities", "--help"], ["capabilities", "--bad-golden-flag"]],
    "worktree": [["worktree", "--help"], ["worktree", "--bad-golden-flag"]],
    "clean": [["clean", "--help"], ["clean", "--bad-golden-flag"]],
    "reap": [["reap", "--help"], ["reap", "--bad-golden-flag"]],
    "lint": [["lint", "--help"], ["lint", "--bad-golden-flag"]],
    "check": [["check", "--help"], ["check", "--bad-golden-flag"]],
    "gate": [["gate", "--help"]],
    "fabric": [["fabric", "--help"], ["fabric", "--bad-golden-flag"],
                ["fabric", "dispatch", "--help"], ["fabric", "dispatch", "--plan"]],
    "lease": [["lease", "--help"], ["lease", "invalid"]],
    "hosts": [["hosts", "--help"], ["hosts", "invalid-action"]],
    "lanes": [["lanes", "--help"], ["lanes", "--bad-golden-flag"]],
    "events": [["events", "--help"], ["events", "--bad-golden-flag"]],
    "graph": [["graph", "--help"], ["graph", "--bad-golden-flag"]],
    "telemetry": [["telemetry", "--help"], ["telemetry", "invalid-action"]],
    "stats": [["stats", "--help"], ["stats", "--bad-golden-flag"]],
    "retro": [["retro", "--help"], ["retro", "--bad-golden-flag"]],
    "quota": [["quota", "--help"], ["quota", "--bad-golden-flag"]],
    "profile": [["profile", "--help"], ["profile", "invalid-action"]],
    "digest": [["digest", "--help"], ["digest", "missing.txt", "question"]],
    "classify": [["classify", "--help"], ["classify", "--labels", "a,b", "missing.json"]],
    "extract": [["extract", "--help"], ["extract", "missing.json", "--schema", "missing-schema.json"]],
    "dispatch": [["dispatch", "--help"], ["dispatch", "--plan"]],
    "batch": [["batch", "--help"], ["batch", "--plan"]],
    "run": [["run", "--help"], ["run", "show", "missing-run"]],
    "pr": [["pr", "--help"], ["pr", "checks", "not-a-pr"]],
}

ALIASES: dict[str, list[list[str]]] = {
    "usage": [["--help"], ["bogus"]],
    "peer": [["peer", "--help"]],
    "events": [["events", "--help"]],
    "hosts": [["hosts", "--help"]],
    "routes": [["help", "routes"]],
}


def binary_path() -> Path | str | None:
    explicit = os.environ.get("PROVENANT_GOLDEN_BIN")
    if explicit:
        return Path(explicit).expanduser().resolve()
    target = Path(os.environ.get("CARGO_TARGET_DIR", "tools/front-door/target"))
    if not target.is_absolute():
        target = REPO / target
    release = target / "release" / "provenant"
    if release.is_file():
        return release
    on_path = shutil.which("provenant")
    return Path(on_path).resolve() if on_path else None


def normalize(value: str, replacements: list[tuple[str, str]]) -> str:
    for source, label in sorted(replacements, key=lambda item: len(item[0]), reverse=True):
        value = value.replace(source, label)
    value = re.sub(r"\b20\d\d-\d\d-\d\d[T ]\d\d:\d\d:\d\d(?:\.\d+)?(?:Z|[+-]\d\d:?\d\d)?\b", "<TIMESTAMP>", value)
    value = re.sub(r"\b\d{13}\b", "<EPOCH_MS>", value)
    value = re.sub(r"\b(?:duration|elapsed|took|waited)[=: ]+\d+(?:\.\d+)?\s*(?:ms|s|sec|seconds)?\b", "<DURATION>", value, flags=re.I)
    value = re.sub(r"\bpid\b[\"']?\s*[:= ]\s*[\"']?\d+\b", "pid=<PID>", value, flags=re.I)
    value = re.sub(r"\b[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\b", "<UUID>", value, flags=re.I)
    value = re.sub(r"\b(?:run|task|attempt)[-_]?[0-9]{6,}[-_a-zA-Z0-9]*\b", "<RUN_ID>", value)
    value = re.sub(r"\b(?:[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}|sk-[A-Za-z0-9_-]{12,})\b", "<TOKEN>", value)
    hostname = socket.gethostname()
    if hostname:
        value = value.replace(hostname, "<HOST>")
    value = re.sub(r"\b[\w.-]+\.local\b", "<HOST>", value)
    return value


class Fixture:
    def __init__(self, binary: Path | str):
        self.tmp = tempfile.TemporaryDirectory(prefix="provenant-golden-")
        self.root = Path(self.tmp.name)
        self.binary = str(binary)
        self.home = self.root / "home"
        self.bin = self.root / "fake-bin"
        self.project = self.root / "fixture-project"
        self.runs = self.project / ".agent-run" / "runs"
        self.roots = [self.home, self.root / "xdg-cache", self.root / "xdg-config",
                      self.root / "xdg-data", self.root / "fabric-state", self.root / "fabric-instance",
                      self.root / "codex", self.root / "claude", self.project]
        for directory in self.roots + [self.bin, self.runs / "fixture-run"]:
            directory.mkdir(parents=True, exist_ok=True)
        (self.runs / "fixture-run" / "RUN.json").write_text('{"run_id":"fixture-run","status":"complete"}\n')
        subprocess.run(["git", "init", "-q", str(self.project)], check=True)
        subprocess.run(["git", "-C", str(self.project), "config", "user.name", "Golden Fixture"], check=True)
        subprocess.run(["git", "-C", str(self.project), "config", "user.email", "golden@example.invalid"], check=True)
        self.make_fake_clis()
        inherited_path = os.environ.get("PATH", "/usr/local/bin:/usr/bin:/bin")
        self.env = {"PATH": str(self.bin) + os.pathsep + inherited_path}
        if os.name == "nt" and "SystemRoot" in os.environ:
            self.env["SystemRoot"] = os.environ["SystemRoot"]
        self.env.update({
            "HOME": str(self.home), "XDG_CACHE_HOME": str(self.root / "xdg-cache"),
            "XDG_CONFIG_HOME": str(self.root / "xdg-config"), "XDG_DATA_HOME": str(self.root / "xdg-data"),
            "AGENT_FABRIC_STATE_ROOT": str(self.root / "fabric-state"),
            "AGENT_FABRIC_INSTANCE_ROOT": str(self.root / "fabric-instance"),
            "CODEX_HOME": str(self.root / "codex"), "CLAUDE_CONFIG_DIR": str(self.root / "claude"),
            "AGENT_FABRIC_PRODUCT_ROOT": str(REPO), "AGENT_FABRIC_PROJECT_ROOT": str(self.project),
            "AGENTS_HOME": str(self.home / ".agents"), "PATH": str(self.bin) + os.pathsep + os.environ.get("PATH", ""),
            "TZ": "UTC", "LC_ALL": "C", "COLUMNS": "80", "NO_COLOR": "1", "TERM": "dumb",
            "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": str(self.home / ".gitconfig"),
            "PYTHONHASHSEED": "0",
            "TMPDIR": str(self.root), "TMP": str(self.root), "TEMP": str(self.root),
        })
        self.replacements = [(str(self.root), "<FIXTURE>"), (str(REPO), "<CHECKOUT>")]

    def make_fake_clis(self) -> None:
        for name in ("claude", "codex", "gemini", "opencode", "cursor"):
            path = self.bin / name
            path.write_text("#!/bin/sh\nprintf '%s\\n' 'golden fake provider'\n")
            path.chmod(0o755)

    def snapshot(self) -> dict[str, str]:
        found: dict[str, str] = {}
        for base in self.roots:
            for path in base.rglob("*"):
                if path.is_file() and not path.is_symlink():
                    rel = "<FIXTURE>" + str(path.relative_to(self.root))
                    data = path.read_bytes()
                    if path.suffix in {".sqlite", ".sqlite3", ".db"}:
                        try:
                            with sqlite3.connect(f"file:{path}?mode=ro", uri=True) as db:
                                dump = "\n".join(db.iterdump()) + "\n"
                            found[rel] = normalize(dump, self.replacements)
                            continue
                        except sqlite3.Error:
                            pass
                    try:
                        content = normalize(data.decode("utf-8"), self.replacements)
                        found[rel] = content
                    except UnicodeDecodeError:
                        found[rel] = "sha256:" + hashlib.sha256(data).hexdigest()
        return found

    def run(self, argv: list[str], stdin: str = "") -> dict[str, Any]:
        before = self.snapshot()
        completed = subprocess.run([self.binary, *argv], cwd=self.project, env=self.env,
                                   input=stdin.encode(), stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, timeout=30)
        after = self.snapshot()
        changed = []
        for path in sorted(before.keys() | after.keys()):
            if before.get(path) != after.get(path):
                changed.append({"path": path, "content": after[path]} if path in after else {"path": path, "deleted": True})
        return {
            "argv": argv, "stdin": stdin,
            "exit_code": completed.returncode,
            "stdout": normalize(completed.stdout.decode("utf-8", "replace"), self.replacements),
            "stderr": normalize(completed.stderr.decode("utf-8", "replace"), self.replacements),
            "files": changed,
        }

    def close(self) -> None:
        self.tmp.cleanup()


def cases_for(verb: str) -> list[tuple[str, list[str]]]:
    rows = [(f"{verb}-case-{index:02d}", argv) for index, argv in enumerate(VERBS.get(verb, []), 1)]
    rows.extend((f"{verb}-alias-{index:02d}", argv) for index, argv in enumerate(ALIASES.get(verb, []), 1))
    return rows


def record(binary: Path | str, selected: str | None) -> None:
    verbs = [selected] if selected else sorted(set(VERBS) | set(ALIASES))
    for verb in verbs:
        for name, argv in cases_for(verb):
            fixture = Fixture(binary)
            try:
                result = fixture.run(argv)
                destination = CASES / verb / f"{name}.json"
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
            finally:
                fixture.close()


def compare(binary: Path | str) -> int:
    files = sorted(CASES.glob("*/*.json"))
    if not files:
        print("no golden cases found; run python tests/golden/run.py --record", file=sys.stderr)
        return 1
    failed = 0
    for path in files:
        fixture = Fixture(binary)
        try:
            expected = json.loads(path.read_text())
            actual = fixture.run(expected["argv"], expected["stdin"])
            got = json.dumps(actual, indent=2, sort_keys=True) + "\n"
            want = json.dumps(expected, indent=2, sort_keys=True) + "\n"
            if got != want:
                failed += 1
                print(f"\n{path.relative_to(HERE)}: mismatch")
                print("".join(difflib.unified_diff(want.splitlines(True), got.splitlines(True),
                                                  fromfile="expected", tofile="actual")), end="")
        finally:
            fixture.close()
    if failed:
        print(f"{failed} golden case(s) differed", file=sys.stderr)
        return 1
    print(f"{len(files)} golden cases passed")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--record", action="store_true")
    mode.add_argument("--compare", action="store_true")
    parser.add_argument("--verb", choices=sorted(set(VERBS) | set(ALIASES)))
    args = parser.parse_args()
    if args.verb and not args.record:
        parser.error("--verb is available only with --record")
    binary = binary_path()
    if binary is None or (isinstance(binary, Path) and not binary.is_file()):
        reason = "no CLI binary found (set PROVENANT_GOLDEN_BIN or build tools/front-door)"
        if os.environ.get("PROVENANT_GOLDEN_REQUIRED") == "1":
            print(reason, file=sys.stderr)
            return 1
        print(f"SKIP: {reason}")
        return 0
    if args.record:
        record(binary, args.verb)
        print(f"recorded golden cases for {args.verb or 'all verbs'}")
        return 0
    return compare(binary)


if __name__ == "__main__":
    raise SystemExit(main())
