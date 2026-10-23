#!/usr/bin/env python3
"""Record and compare CLI-boundary cases. Uses only the Python standard library."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from datetime import datetime, timezone
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
MANIFEST = HERE / "manifest.json"

# Classes are explicit fixture contracts, independent of argument spelling.
VERBS: dict[str, list[tuple[str, list[str]]]] = {
    "help": [
        ('help', ['help']),
        ('help', ['help', 'bogus']),
    ],
    "root": [
        ('machine', ['root']),
        ('help', ['root', 'extra']),
    ],
    "route": [
        ('help', ['route', '--help']),
        ('help', ['route', '--bad-golden-flag']),
    ],
    "routes": [
        ('help', ['routes', '--help']),
        ('help', ['routes', '--bad-golden-flag']),
    ],
    "capabilities": [
        ('help', ['capabilities', '--help']),
        ('help', ['capabilities', '--bad-golden-flag']),
    ],
    "worktree": [
        ('help', ['worktree', '--help']),
        ('help', ['worktree', '--bad-golden-flag']),
    ],
    "clean": [
        ('help', ['clean', '--help']),
        ('help', ['clean', '--bad-golden-flag']),
    ],
    "reap": [
        ('help', ['reap', '--help']),
        ('help', ['reap', '--bad-golden-flag']),
    ],
    "lint": [
        ('help', ['lint', '--help']),
        ('help', ['lint', '--bad-golden-flag']),
    ],
    "check": [
        ('help', ['check', '--help']),
        ('help', ['check', '--bad-golden-flag']),
    ],
    "gate": [
        ('help', ['gate', '--help']),
    ],
    "fabric": [
        ('help', ['fabric', '--help']),
        ('help', ['fabric', '--bad-golden-flag']),
        ('help', ['fabric', 'dispatch', '--help']),
        ('help', ['fabric', 'dispatch', '--bad-golden-flag']),
    ],
    "lease": [
        ('help', ['lease', '--help']),
        ('help', ['lease', 'invalid']),
    ],
    "hosts": [
        ('help', ['hosts', '--help']),
        ('help', ['hosts', 'invalid-action']),
    ],
    "lanes": [
        ('help', ['lanes', '--help']),
        ('help', ['lanes', '--bad-golden-flag']),
    ],
    "events": [
        ('help', ['events', '--help']),
        ('help', ['events', '--bad-golden-flag']),
    ],
    "graph": [
        ('help', ['graph', '--help']),
        ('help', ['graph', '--bad-golden-flag']),
    ],
    "quota": [
        ('help', ['quota', '--help']),
        ('help', ['quota', '--bad-golden-flag']),
    ],
    "profile": [
        ('help', ['profile', '--help']),
        ('help', ['profile', 'invalid-action']),
    ],
    "digest": [
        ('help', ['digest', '--help']),
        ('machine', ['digest', 'missing.txt', 'question']),
    ],
    "classify": [
        ('help', ['classify', '--help']),
        ('machine', ['classify', '--labels', 'a,b', 'missing.json']),
    ],
    "extract": [
        ('help', ['extract', '--help']),
        ('machine', ['extract', 'missing.json', '--schema', 'missing-schema.json']),
    ],
    "dispatch": [
        ('help', ['dispatch', '--help']),
        ('help', ['dispatch', '--plan']),
    ],
    "batch": [
        ('help', ['batch', '--help']),
        ('help', ['batch', '--plan']),
    ],
    "run": [
        ('help', ['run', '--help']),
        ('machine', ['run', 'show', 'missing-run']),
    ],
    "pr": [
        ('help', ['pr', '--help']),
        ('machine', ['pr', 'checks', 'not-a-pr']),
    ],
}

ALIASES: dict[str, list[tuple[str, list[str]]]] = {
    "usage": [
        ('help', ['--help']),
        ('help', ['bogus']),
    ],
    "peer": [
        ('help', ['peer', '--help']),
    ],
}


def binary_path() -> Path | None:
    explicit = os.environ.get("PROVENANT_GOLDEN_BIN")
    if explicit:
        return Path(explicit).expanduser().resolve()
    target = Path(os.environ.get("CARGO_TARGET_DIR", "tools/front-door/target"))
    if not target.is_absolute():
        target = REPO / target
    release = target / "release" / "provenant"
    return release.resolve() if release.is_file() else None


def suite_binary() -> Path | None:
    """The binary for end-to-end tests, only when a job names it explicitly.

    Parallel test shards share the checkout, so the checkout guard cannot
    tell a golden child's writes from another test's.
    """
    return binary_path() if os.environ.get("PROVENANT_GOLDEN_BIN") else None


def normalize(value: str, replacements: list[tuple[str, str]]) -> str:
    for source, label in sorted(replacements, key=lambda item: len(item[0]), reverse=True):
        value = value.replace(source, label)
    value = re.sub(r"\b20\d\d-\d\d-\d\d[T ]\d\d:\d\d:\d\d(?:\.\d+)?(?:Z|[+-]\d\d:?\d\d)?\b", "<TIMESTAMP>", value)
    value = re.sub(r"\b\d{13}\b", "<EPOCH_MS>", value)
    value = re.sub(r"\b(?:duration|elapsed|took|waited)[=: ]+\d+(?:\.\d+)?\s*(?:ms|s|sec|seconds)?\b", "<DURATION>", value, flags=re.I)
    value = re.sub(r'(\"(?:pid|owner_pid|parent_pid)\"\s*:\s*|(?<![\w-])pid=)\d+\b', r'\1<PID>', value)
    value = re.sub(r"\b[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\b", "<UUID:8-4-4-4-12>", value, flags=re.I)

    def shaped(match: re.Match[str]) -> str:
        token = match.group()
        shape = ''.join('x' if c.isalnum() else c for c in token)
        return f"<{shape}>"

    value = re.sub(r"\b(?:run|task|attempt)[-_]\d{6,}[-_a-zA-Z0-9]*\b|\b\d{8}-\d{4}-(?:dispatch|batch)-[A-Za-z0-9_.-]+\b|\bmcp-[a-zA-Z0-9]{6,}\b", shaped, value)
    value = re.sub(r"\b(?:[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}|sk-[A-Za-z0-9_-]{12,})\b", shaped, value)
    hostname = socket.gethostname()
    if hostname:
        value = re.sub(r"(?<![\w./-])" + re.escape(hostname) + r"(?![\w./-])", "<HOST>", value)
    value = re.sub(r"(?<![\w./-])(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?\.)+local(?![\w./-])", "<HOST>", value)
    return value


def path_spellings(path: Path, label: str) -> list[tuple[str, str]]:
    resolved = str(path.resolve())
    spellings = {str(path), resolved}
    if resolved.startswith("/private/var/"):
        spellings.add(resolved.removeprefix("/private"))
    elif resolved.startswith("/var/"):
        spellings.add("/private" + resolved)
    return [(spelling, label) for spelling in spellings]


def checkout_status(excluded: tuple[Path, ...] = ()) -> bytes:
    exclusions = [f":(exclude,literal){path.resolve().relative_to(REPO)}"
                  for path in excluded if path.resolve().is_relative_to(REPO)]
    status = subprocess.check_output([
        shutil.which("git") or "git", "--no-optional-locks", "-C", str(REPO),
        "status", "--porcelain", "-z", "--ignored", "--untracked-files=all", "--", ".", *exclusions,
    ])
    # Status alone misses edits to files already dirty, untracked or ignored.
    stamps = []
    for entry in status.split(b"\0"):
        if len(entry) < 4:
            continue
        path = REPO / os.fsdecode(entry[3:])
        try:
            stat = path.lstat()
        except FileNotFoundError:
            continue
        stamps.append((str(path), stat.st_size, stat.st_mtime_ns, stat.st_ctime_ns))
    return status + json.dumps(stamps).encode()


def runtime_manifest() -> dict[str, str]:
    return {"python": f"{sys.version_info.major}.{sys.version_info.minor}"}


def check_manifest() -> None:
    expected = json.loads(MANIFEST.read_text())
    if expected != runtime_manifest():
        raise ValueError(
            f"golden runtime mismatch: recorded {expected}, current {runtime_manifest()}; "
            f"re-record with Python {expected['python']} (CI uses 3.14) and review the manifest and case diffs"
        )


def help_structure(text: str, verb: str | None = None) -> dict[str, list[str]]:
    """Read explicit command sections and positional brace choices only."""
    flags = set(re.findall(r"(?<![\w-])--?[A-Za-z][\w-]*", text))
    commands: set[str] = set()
    lines = text.splitlines()
    in_commands = False
    prefix = None
    for index, line in enumerate(lines):
        # Fabric uses a standalone signature as its command-section heading.
        heading = re.fullmatch(r"fabric (?:<command>|([a-z][\w-]*))", line)
        if heading:
            in_commands = True
            prefix = heading[1]
            continue
        if re.fullmatch(r"commands: ?|commands|subcommands: ?|subcommands", line.strip(), re.I):
            in_commands = True
            prefix = None
            continue
        if line and not line[0].isspace():
            in_commands = False
            prefix = None
        if in_commands:
            match = re.match(r"^ {2,4}([a-z][\w|-]*)(?:[ \t]+|$)(.*)", line)
            if match:
                name, rest = match.groups()
                if name == (prefix or verb):
                    child = re.match(r"([a-z][\w|-]*)(?:\s|$)", rest)
                    if child:
                        commands.update(child[1].split("|"))
                elif prefix is None:
                    commands.update(name.split("|"))
        if not re.match(r"\s*usage:", line, re.I):
            continue
        usage = re.split(r"usage:", line, maxsplit=1, flags=re.I)[1]
        for continuation in lines[index + 1:]:
            if not continuation.startswith(" ") or not continuation.strip():
                break
            usage += " " + continuation.strip()
        # Remove nested optional groups, including option value choices.
        while re.search(r"\[[^\[\]]*\]", usage):
            usage = re.sub(r"\[[^\[\]]*\]", " ", usage)
        for match in re.finditer(r"\{([a-z][a-z0-9_,|-]*)\}", usage):
            before = usage[:match.start()].split()
            if before and before[-1].startswith("-"):
                continue
            commands.update(re.split(r"[,|]", match[1]))
    if verb:
        commands.discard(verb)
    return {"subcommands": sorted(commands), "flags": sorted(flags)}


def comparison_fields(result: dict[str, Any]) -> dict[str, Any]:
    if result["class"] == "help":
        return {"exit_code": result["exit_code"],
                "help": help_structure(result["stdout"] + "\n" + result["stderr"],
                                       verb=result.get("argv", [None])[0])}
    fields = {key: result[key] for key in ("exit_code", "stdout", "stderr")}
    files = {row["path"]: {key: val for key, val in row.items() if key != "path"} for row in result["files"]}
    fields["files.paths"] = sorted(files)
    fields.update({f"files.{path}": content for path, content in files.items()})
    return fields


class Fixture:
    def __init__(self, binary: Path | str, checkout_excluded: tuple[Path, ...] = ()):
        self.checkout_excluded = checkout_excluded
        self.tmp = tempfile.TemporaryDirectory(prefix="provenant-golden-")
        self.root = Path(self.tmp.name).resolve()
        self.binary = str(binary)
        self.home = self.root / "home"
        self.bin = self.root / "fake-bin"
        self.project = self.root / "fixture-project"
        self.runs = self.project / ".agent-run" / "runs"
        self.roots = [self.home, self.root / "xdg-cache", self.root / "xdg-config",
                      self.root / "xdg-data", self.root / "fabric-state", self.root / "fabric-instance",
                      self.root / "codex", self.root / "claude", self.root / "gh", self.project]
        for directory in self.roots + [self.bin, self.runs / "fixture-run"]:
            directory.mkdir(parents=True, exist_ok=True)
        (self.runs / "fixture-run" / "RUN.json").write_text('{"run_id":"fixture-run","status":"complete"}\n')
        self.make_fake_clis()
        self.git = shutil.which("git")
        if not self.git:
            raise RuntimeError("golden fixtures require git")
        # Every PATH entry is pinned here; no host PATH directories leak in.
        for name, target in {"git": self.git, "python": sys.executable, "python3": sys.executable,
                             "bash": shutil.which("bash"), "dirname": shutil.which("dirname")}.items():
            if not target:
                self.close()
                raise RuntimeError(f"golden fixtures require {name}")
            (self.bin / name).symlink_to(target)
        self.env = {"PATH": str(self.bin)}
        if os.name == "nt" and "SystemRoot" in os.environ:
            self.env["SystemRoot"] = os.environ["SystemRoot"]
        self.env.update({
            "HOME": str(self.home), "XDG_CACHE_HOME": str(self.root / "xdg-cache"),
            "XDG_CONFIG_HOME": str(self.root / "xdg-config"), "XDG_DATA_HOME": str(self.root / "xdg-data"),
            "AGENT_FABRIC_STATE_ROOT": str(self.root / "fabric-state"),
            "AGENT_FABRIC_INSTANCE_ROOT": str(self.root / "fabric-instance"),
            "CODEX_HOME": str(self.root / "codex"), "CLAUDE_CONFIG_DIR": str(self.root / "claude"),
            "AGENT_FABRIC_PRODUCT_ROOT": str(REPO), "AGENT_FABRIC_PROJECT_ROOT": str(self.project),
            "AGENTS_HOME": str(self.home / ".agents"),
            "TZ": "UTC", "LC_ALL": "C", "COLUMNS": "80", "NO_COLOR": "1", "TERM": "dumb",
            "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": str(self.home / ".gitconfig"),
            "PYTHONHASHSEED": "0", "PYTHONDONTWRITEBYTECODE": "1", "PYTHONNOUSERSITE": "1",
            "HARNESS_PYTHON": sys.executable, "GIT_TERMINAL_PROMPT": "0",
            "GH_CONFIG_DIR": str(self.root / "gh"), "GH_TOKEN": "",
            "GIT_AUTHOR_DATE": "2020-01-01T00:00:00Z", "GIT_COMMITTER_DATE": "2020-01-01T00:00:00Z",
            "TMPDIR": str(self.root), "TMP": str(self.root), "TEMP": str(self.root),
            # Stale-checkout warnings depend on the host checkout; tests/test_stale.py owns them.
            "PROVENANT_STALE_CHECKED": "1",
        })
        # Fabric's real query paths need its installed Node runtime, pinned once.
        node = shutil.which("node")
        if node:
            self.env["FABRIC_NODE"] = str(Path(node).resolve())
        self.replacements = (path_spellings(self.root, "<FIXTURE>") + path_spellings(REPO, "<CHECKOUT>")
                             + [(sys.executable, "<PYTHON>")])
        self.git_run("init", "-q", "-b", "main")
        self.git_run("config", "user.name", "Golden Fixture")
        self.git_run("config", "user.email", "golden@example.invalid")
        self.git_run("config", "core.autocrlf", "false")
        (self.project / ".gitignore").write_text(".agent-run/\n.worktrees/\n")
        self.git_run("add", ".gitignore")
        self.git_run("-c", "commit.gpgsign=false", "commit", "-qm", "fixture")

    def git_run(self, *args: str) -> None:
        subprocess.run([self.git, "-C", str(self.project), *args], env=self.env,
                       check=True, capture_output=True)

    def make_fake_clis(self) -> None:
        for name in ("claude", "codex", "gemini", "opencode", "cursor"):
            path = self.bin / name
            path.write_text("#!/bin/sh\nprintf '%s\\n' 'golden fake provider'\n")
            path.chmod(0o755)

        gh = self.bin / "gh"
        gh.write_text("#!/bin/sh\nprintf '%s\\n' 'golden: gh unavailable (offline fixture)' >&2\nexit 1\n")
        gh.chmod(0o755)

    def snapshot(self) -> dict[str, str]:
        found: dict[str, str] = {}
        for base in self.roots:
            for path in base.rglob("*"):
                # Git's binary index/stat caches are implementation metadata, not
                # portable output files. Worktree .git pointer files remain visible.
                if ".git" in path.relative_to(self.root).parts[:-1]:
                    continue
                if path.is_file() and not path.is_symlink():
                    rel = "<FIXTURE>/" + str(path.relative_to(self.root))
                    data = path.read_bytes()
                    if path.suffix in {".sqlite", ".sqlite3", ".db"}:
                        try:
                            with sqlite3.connect(f"file:{path}?mode=ro", uri=True) as db:
                                schema = db.execute("SELECT type, name, tbl_name, sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY type, name, tbl_name, sql").fetchall()
                                tables = {}
                                for (name,) in db.execute("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"):
                                    quoted = '"' + name.replace('"', '""') + '"'
                                    columns = len(db.execute(f"SELECT * FROM {quoted} LIMIT 0").description)
                                    order = ", ".join(str(i) for i in range(1, columns + 1))
                                    tables[name] = db.execute(f"SELECT * FROM {quoted} ORDER BY {order}").fetchall()
                                dump = json.dumps({"schema": schema, "rows": tables}, indent=2, sort_keys=True,
                                                  default=lambda blob: {"blob_hex": blob.hex()}) + "\n"
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
        checkout_before = checkout_status(self.checkout_excluded)
        before = self.snapshot()
        try:
            completed = subprocess.run([self.binary, *argv], cwd=self.project, env=self.env,
                                       input=stdin.encode(), stdout=subprocess.PIPE,
                                       stderr=subprocess.PIPE, timeout=30)
        finally:
            if checkout_status(self.checkout_excluded) != checkout_before:
                raise AssertionError("golden child changed AGENT_FABRIC_PRODUCT_ROOT checkout")
        after = self.snapshot()
        changed = []
        for path in sorted(before.keys() | after.keys()):
            if before.get(path) != after.get(path):
                changed.append({"path": path, "content": after[path]} if path in after else {"path": path, "deleted": True})
        return {
            "argv": argv, "stdin": stdin,
            "exit_code": completed.returncode,
            "stdout": normalize(completed.stdout.decode("utf-8", "surrogateescape"), self.replacements),
            "stderr": normalize(completed.stderr.decode("utf-8", "surrogateescape"), self.replacements),
            "files": changed,
        }

    def close(self) -> None:
        self.tmp.cleanup()


@dataclass(frozen=True)
class Case:
    name: str
    verb: str
    argv: list[str]
    kind: str
    setup: tuple[tuple[str, ...], ...] = ()
    stdin: str = ""


CREATE_WORKTREE = ("worktree", "create", "golden", "--detach", "HEAD", "--no-node-modules")
SUCCESS_CASES = [
    Case("worktree-create", "worktree", list(CREATE_WORKTREE), "machine"),
    Case("worktree-list", "worktree", ["worktree", "list"], "machine", (CREATE_WORKTREE,)),
    Case("worktree-remove", "worktree", ["worktree", "remove", "golden"], "machine", (CREATE_WORKTREE,)),
    Case("route-resolve", "route", ["route", "resolve", "--help"], "help"),
    Case("routes-catalog", "routes", ["routes", "--json"], "machine"),
    Case("lint-branch", "lint", ["lint", "branch", "feat/cli-golden-fixture"], "machine"),
    Case("events-query", "events", ["events"], "machine"),
    Case("fabric-dispatch-list", "fabric", ["fabric", "dispatch", "list", "--json"], "machine"),
]


def cases_for(verb: str) -> list[Case]:
    rows = []
    for label, mapping in (("case", VERBS), ("alias", ALIASES)):
        for index, (kind, argv) in enumerate(mapping.get(verb, []), 1):
            name = f"{verb}-{label}-{index:02d}"
            if argv == ["fabric", "dispatch", "--bad-golden-flag"]:
                name = "fabric-dispatch-invalid-option"
            rows.append(Case(name, verb, argv, kind))
    return rows + [case for case in SUCCESS_CASES if case.verb == verb]


def all_cases() -> list[Case]:
    return [case for verb in sorted(set(VERBS) | set(ALIASES)) for case in cases_for(verb)]


def run_case(binary: Path | str, case: Case, env: dict[str, str] | None = None,
             checkout_excluded: tuple[Path, ...] = ()) -> dict[str, Any]:
    # Selectors may choose product lanes, never configure interpreters or loaders.
    invalid = {key for key in env or {} if not key.startswith(("PROVENANT_", "AGENT_FABRIC_"))}
    if invalid:
        raise ValueError(f"implementation selector name(s) not allowed: {', '.join(sorted(invalid))}")
    if case.verb in {"fabric", "lease", "lanes", "events", "graph"} and not shutil.which("node"):
        raise RuntimeError("golden Fabric cases require node on the host PATH")
    fixture = Fixture(binary, checkout_excluded)
    try:
        if env:
            reserved = env.keys() & (fixture.env.keys() | {"FABRIC_NODE"})
            if reserved:
                raise ValueError(f"implementation selector cannot override fixture variable(s): {', '.join(sorted(reserved))}")
            fixture.env.update(env)
        if case.name == "events-query":
            attempt = fixture.runs / "20200101-0000-dispatch-golden-000000" / "tasks" / "golden" / "attempt-001"
            attempt.mkdir(parents=True)
            # Events includes terminal attempts from the last day only.
            stamp = datetime.now(timezone.utc).isoformat()
            (attempt / "attempt.json").write_text(json.dumps({
                "schema": "fabric.attempt.v1", "run_id": "fixture-run", "task_id": "golden",
                "attempt": 1, "state": "terminal", "status": "ok", "paths": {},
                "started_at": stamp, "ended_at": stamp,
            }) + "\n")
        for argv in case.setup:
            setup = fixture.run(list(argv))
            if setup["exit_code"]:
                raise RuntimeError(f"{case.name} setup failed: {setup}")
        result = fixture.run(case.argv, case.stdin)
        result["class"] = case.kind
        return result
    finally:
        fixture.close()


def record(binary: Path | str, selected: str | None) -> None:
    generated = all_cases()
    if selected and selected not in {case.verb for case in generated}:
        raise ValueError(f"unknown verb: {selected}")
    selected_cases = [case for case in generated if not selected or case.verb == selected]
    # Help-only updates neither require nor bless a different machine runtime.
    if selected and MANIFEST.exists() and any(case.kind == "machine" for case in selected_cases):
        check_manifest()
    for case in generated:
        if selected and case.verb != selected:
            continue
        result = run_case(binary, case)
        if case in SUCCESS_CASES and result["exit_code"] != 0:
            raise ValueError(f"{case.name}: expected a real success path, got {result}")
        destination = CASES / case.verb / f"{case.name}.json"
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    expected = {CASES / case.verb / f"{case.name}.json" for case in generated}
    for path in CASES.glob("*/*.json"):
        if path not in expected:
            path.unlink()
    if not selected or not MANIFEST.exists():
        MANIFEST.write_text(json.dumps(runtime_manifest(), indent=2, sort_keys=True) + "\n")


def compare(binary: Path | str) -> int:
    try:
        generated = all_cases()
        if any(case.kind == "machine" for case in generated):
            check_manifest()
        expected_paths = {CASES / case.verb / f"{case.name}.json" for case in generated}
        actual_paths = set(CASES.glob("*/*.json"))
        if expected_paths != actual_paths:
            raise ValueError(f"golden case-file set differs: missing={sorted(str(p.relative_to(HERE)) for p in expected_paths - actual_paths)}, orphan={sorted(str(p.relative_to(HERE)) for p in actual_paths - expected_paths)}; re-record")
        help_case = Case("coverage", "help", ["help"], "help")
        help_result = run_case(binary, help_case)
        commands = set(help_structure(help_result["stdout"])["subcommands"])
        covered = {case.argv[0] for case in generated}
        if help_result["exit_code"] or not commands or commands - covered:
            raise ValueError(f"provenant help command coverage failed: missing={sorted(commands - covered)}")
        failed = 0
        for case in generated:
            path = CASES / case.verb / f"{case.name}.json"
            expected = json.loads(path.read_text())
            actual = run_case(binary, case)
            # Input metadata is part of the recorded contract too.
            want = {key: expected[key] for key in ("argv", "stdin", "class")}
            got = {key: actual[key] for key in ("argv", "stdin", "class")}
            want.update(comparison_fields(expected))
            got.update(comparison_fields(actual))
            if got != want:
                failed += 1
                print(f"\n{path.relative_to(HERE)}: mismatch")
                print("".join(difflib.unified_diff(
                    (json.dumps(want, indent=2, sort_keys=True) + "\n").splitlines(True),
                    (json.dumps(got, indent=2, sort_keys=True) + "\n").splitlines(True),
                    fromfile="expected", tofile="actual")), end="")
        if failed:
            print(f"{failed} golden case(s) differed", file=sys.stderr)
            return 1
        print(f"{len(generated)} golden cases passed")
        return 0
    except (ValueError, OSError, AssertionError) as exc:
        print(str(exc), file=sys.stderr)
        return 1


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
    if binary is None or not binary.is_file():
        reason = "no CLI binary found (set PROVENANT_GOLDEN_BIN or build tools/front-door)"
        if os.environ.get("PROVENANT_GOLDEN_ALLOW_SKIP") == "1" and os.environ.get("PROVENANT_GOLDEN_REQUIRED") != "1":
            print(f"SKIP: {reason}")
            return 0
        print(reason, file=sys.stderr)
        return 1
    if args.record:
        record(binary, args.verb)
        print(f"recorded golden cases for {args.verb or 'all verbs'}")
        return 0
    return compare(binary)


if __name__ == "__main__":
    raise SystemExit(main())
