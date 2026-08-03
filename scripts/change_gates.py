#!/usr/bin/env python3
"""Fail-closed red, revert-probe, and changed-line mutation gates.

The runner deliberately treats test-process failures as typed evidence.  A
non-zero exit caused by importing or collecting a test is not a right-reason
red, because it says nothing about the production behaviour under test.
"""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import io
import os
from dataclasses import dataclass
from enum import Enum
import re
import shlex
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
from pathlib import Path


class GateError(RuntimeError):
    """A gate cannot certify its required evidence."""


class FailureClass(str, Enum):
    PASS = "pass"
    ASSERTION = "assertion-failure"
    IMPORT = "import-error"
    COLLECTION = "collection-error"
    SETUP = "fixture-or-setup-error"
    UNKNOWN = "unclassified-failure"


@dataclass(frozen=True)
class CommandResult:
    command: str
    returncode: int
    output: str
    classification: FailureClass


@dataclass(frozen=True)
class DiffHunk:
    path: str
    header: tuple[str, ...]
    hunk_header: str
    body: tuple[str, ...]
    old_start: int
    old_count: int
    new_start: int
    new_count: int
    old_lines: tuple[str, ...]
    new_lines: tuple[str, ...]


@dataclass(frozen=True)
class Mutant:
    path: str
    line: int
    before: str
    after: str | None
    description: str


_IMPORT_DIAGNOSTICS = (
    re.compile(r"^\s*(?:E\s+)?(?:ImportError|ModuleNotFoundError)(?::|\b)", re.IGNORECASE),
    re.compile(
        r"^\s*(?:E\s+)?(?:Error:\s+)?(?:Cannot find module|Failed to (?:load url|resolve import)|ERR_MODULE_NOT_FOUND\b|does not provide an export named)\b",
        re.IGNORECASE,
    ),
)
_COLLECTION_DIAGNOSTICS = (
    re.compile(r"^\s*(?:E\s+)?ERROR\s+(?:collecting\b|during\s+collection\b)", re.IGNORECASE),
    re.compile(r"^\s*(?:E\s+)?(?:\d+\s+)?errors?\s+during\s+collection\b", re.IGNORECASE),
    re.compile(r"^\s*(?:E\s+)?(?:No test files found|No tests collected|Test file not found)\b", re.IGNORECASE),
)
_SETUP_DIAGNOSTICS = (
    re.compile(
        r"^\s*(?:E\s+)?ERROR\s+(?:at\s+)?(?:setup|teardown)\b|^\s*(?:E\s+)?(?:setup|teardown)\s+failed\b",
        re.IGNORECASE,
    ),
    re.compile(
        r"^\s*(?:E\s+)?(?:beforeAll|before_all|beforeEach|before_each|afterAll|after_all|afterEach|after_each)\b.*\b(?:failed|error)\b",
        re.IGNORECASE,
    ),
    re.compile(r"^\s*(?:E\s+)?fixture\s+['\"][^'\"]+['\"]\s+not found\b", re.IGNORECASE),
)
_RUNTIME_DIAGNOSTICS = (
    re.compile(r"^\s*(?:E\s+)?(?:TypeError|ReferenceError|SyntaxError)(?::|\b)", re.IGNORECASE),
    re.compile(
        r"^\s*(?:E\s+)?(?:Cannot read properties of .+|[A-Za-z_$][\w.$]*\s+is not a function\b)",
        re.IGNORECASE,
    ),
)
_ASSERTION_DIAGNOSTICS = (
    re.compile(r"^\s*(?:E\s+)?AssertionError(?:\s+\[[^\]]+\])?(?::|\b)", re.IGNORECASE),
    re.compile(r"^\s*(?:E\s+)?assert\b", re.IGNORECASE),
    re.compile(r"^\s*(?:E\s+)?Error:\s+expected\b", re.IGNORECASE),
)
_HUNK_RE = re.compile(
    r"^@@ -(?P<old_start>\d+)(?:,(?P<old_count>\d+))? "
    r"\+(?P<new_start>\d+)(?:,(?P<new_count>\d+))? @@"
)
_DIFF_FILE_RE = re.compile(r"^diff --git a/(.+) b/(.+)$")


def classify_failure(returncode: int, output: str) -> FailureClass:
    """Classify a test process from anchored runner diagnostics.

    Diagnostics are matched line-by-line in output order.  This keeps marker
    words printed inside an assertion message from overriding the assertion
    that actually failed, while an earlier setup, import, collection, or
    runtime diagnostic remains mechanical evidence.
    """

    if returncode == 0:
        return FailureClass.PASS
    diagnostics = (
        (FailureClass.IMPORT, _IMPORT_DIAGNOSTICS),
        (FailureClass.COLLECTION, _COLLECTION_DIAGNOSTICS),
        (FailureClass.SETUP, _SETUP_DIAGNOSTICS),
        (FailureClass.UNKNOWN, _RUNTIME_DIAGNOSTICS),
        (FailureClass.ASSERTION, _ASSERTION_DIAGNOSTICS),
    )
    for line in output.splitlines():
        for classification, patterns in diagnostics:
            if any(pattern.search(line) for pattern in patterns):
                return classification
    return FailureClass.UNKNOWN


def run_command(command: str, cwd: Path, test_path: str | None = None) -> CommandResult:
    arguments = shlex.split(command)
    if test_path:
        arguments = [test_path if argument == "{test}" else argument for argument in arguments]
    rendered = shlex.join(arguments)
    completed = subprocess.run(
        arguments,
        cwd=cwd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        env=os.environ.copy(),
        check=False,
    )
    output = completed.stdout or ""
    return CommandResult(
        command=rendered,
        returncode=completed.returncode,
        output=output,
        classification=classify_failure(completed.returncode, output),
    )


def parse_diff(diff_text: str) -> list[DiffHunk]:
    """Parse unified diff hunks while retaining enough data for one-at-a-time probes."""

    lines = diff_text.splitlines()
    blocks: list[tuple[str, list[str]]] = []
    current_path: str | None = None
    current: list[str] = []
    for line in lines:
        match = _DIFF_FILE_RE.match(line)
        if match:
            if current_path is not None:
                blocks.append((current_path, current))
            current_path = match.group(2)
            current = [line]
        elif current_path is not None:
            current.append(line)
    if current_path is not None:
        blocks.append((current_path, current))

    hunks: list[DiffHunk] = []
    for path, block in blocks:
        hunk_indexes = [index for index, line in enumerate(block) if line.startswith("@@ ")]
        if not hunk_indexes:
            continue
        header = tuple(block[: hunk_indexes[0]])
        for position, hunk_index in enumerate(hunk_indexes):
            end = hunk_indexes[position + 1] if position + 1 < len(hunk_indexes) else len(block)
            hunk_header = block[hunk_index]
            match = _HUNK_RE.match(hunk_header)
            if not match:
                raise GateError(f"cannot parse unified hunk header: {hunk_header}")
            body = tuple(
                line for line in block[hunk_index + 1 : end]
                if line != r"\ No newline at end of file"
            )
            old_lines = tuple(line[1:] for line in body if line.startswith((" ", "-")))
            new_lines = tuple(line[1:] for line in body if line.startswith((" ", "+")))
            hunks.append(
                DiffHunk(
                    path=path,
                    header=header,
                    hunk_header=hunk_header,
                    body=body,
                    old_start=int(match.group("old_start")),
                    old_count=int(match.group("old_count") or 1),
                    new_start=int(match.group("new_start")),
                    new_count=int(match.group("new_count") or 1),
                    old_lines=old_lines,
                    new_lines=new_lines,
                )
            )
    return hunks


def git_diff(source_root: Path, base: str) -> str:
    completed = subprocess.run(
        ["git", "-C", str(source_root), "diff", "--binary", base, "--"],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if completed.returncode != 0:
        raise GateError(f"unable to compute diff against {base}: {completed.stderr.strip()}")
    return completed.stdout


def is_test_path(path: str) -> bool:
    lowered = path.casefold()
    name = Path(path).name.casefold()
    return (
        "/tests/" in f"/{lowered}/"
        or name.startswith("test_")
        or name.endswith("_test.py")
        or ".test." in name
        or ".spec." in name
    )


def is_source_path(path: str) -> bool:
    return (
        Path(path).suffix.casefold() in {".py", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"}
        and "/dist/" not in f"/{path}/"
        and not is_test_path(path)
    )


def changed_test_paths(hunks: list[DiffHunk]) -> list[str]:
    return sorted({hunk.path for hunk in hunks if is_test_path(hunk.path)})


def production_hunks(hunks: list[DiffHunk]) -> list[DiffHunk]:
    return [hunk for hunk in hunks if is_source_path(hunk.path)]


def changed_line_numbers(hunk: DiffHunk) -> list[int]:
    line_number = hunk.new_start
    changed: list[int] = []
    for line in hunk.body:
        if line.startswith("+"):
            changed.append(line_number)
            line_number += 1
        elif line.startswith(" "):
            line_number += 1
    return changed


def _replace_once(line: str, old: str, new: str) -> str | None:
    index = line.find(old)
    if index < 0:
        return None
    return line[:index] + new + line[index + len(old) :]


def mutations_for_lines(path: str, lines: list[str], line_numbers: list[int]) -> list[Mutant]:
    """Generate all supported, line-local mutations; never mutate an unchanged line."""

    candidates: list[tuple[str, str | None, str]] = []
    replacements = (
        ("===", "!==", "strict equality flipped"),
        ("!==", "===", "strict inequality flipped"),
        ("==", "!=", "equality flipped"),
        ("!=", "==", "inequality flipped"),
        (">=", "<", "greater-than boundary flipped"),
        ("<=", ">", "less-than boundary flipped"),
        ("&&", "||", "boolean conjunction flipped"),
        ("||", "&&", "boolean disjunction flipped"),
        ("True", "False", "boolean literal flipped"),
        ("False", "True", "boolean literal flipped"),
        ("true", "false", "boolean literal flipped"),
        ("false", "true", "boolean literal flipped"),
    )
    for number in line_numbers:
        if number < 1 or number > len(lines):
            raise GateError(f"changed line {path}:{number} is absent from the source tree")
        before = lines[number - 1]
        for old, new, description in replacements:
            after = _replace_once(before, old, new)
            if after is not None and after != before:
                candidates.append((before, after, description))
        if before.strip() and not before.lstrip().startswith(("#", "//", "/*", "*")):
            candidates.append((before, None, "changed line deleted"))
        if "process.cwd()" in before:
            candidates.append((before, before.replace("process.cwd()", "undefined", 1), "cwd default removed"))
        call = re.search(r"\b(?:canonicalConfigPath|realpathSync)\(([^()]+)\)", before)
        if call:
            candidates.append((before, before[: call.start()] + call.group(1) + before[call.end() :], "path canonicalisation removed"))
    seen: set[tuple[int, str]] = set()
    result: list[Mutant] = []
    for number in line_numbers:
        before = lines[number - 1]
        for candidate_before, after, description in candidates:
            key = (number, "<delete>" if after is None else after)
            if candidate_before != before or key in seen:
                continue
            seen.add(key)
            result.append(Mutant(path, number, before, after, description))
    return result


def build_mutants(source_root: Path, hunks: list[DiffHunk]) -> list[Mutant]:
    mutants: list[Mutant] = []
    for hunk in production_hunks(hunks):
        path = source_root / hunk.path
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except OSError as exc:
            raise GateError(f"cannot read changed production file {hunk.path}: {exc}") from exc
        mutants.extend(mutations_for_lines(hunk.path, lines, changed_line_numbers(hunk)))
    return mutants


def _ensure_no_symlink_components(root: Path, path: Path) -> None:
    """Reject a destination whose write could traverse outside its scratch root."""

    try:
        relative_path = path.relative_to(root)
    except ValueError as exc:
        raise GateError(f"scratch destination escapes scratch tree: {path}") from exc
    if relative_path.is_absolute() or ".." in relative_path.parts:
        raise GateError(f"scratch destination escapes scratch tree: {path}")
    current = root
    for component in relative_path.parts:
        current /= component
        if current.is_symlink():
            raise GateError(f"scratch destination contains a symlink: {current}")


def _remove_scratch_path(root: Path, path: Path) -> None:
    _ensure_no_symlink_components(root, path)
    if path.is_symlink() or path.is_file():
        path.unlink()
    elif path.is_dir():
        shutil.rmtree(path)


def _remap_source_symlink(
    source: Path, target: Path, source_root: Path, destination_root: Path
) -> str:
    link = os.readlink(source)
    resolved = (source.parent / link).resolve(strict=False)
    try:
        relative_target = resolved.relative_to(source_root.resolve())
    except ValueError:
        return link
    return os.path.relpath(destination_root / relative_target, target.parent)


def _safe_copy_entry(
    source: Path,
    target: Path,
    source_root: Path,
    destination_root: Path,
    ignored_names: frozenset[str] = frozenset(),
) -> None:
    """Copy one tree entry without following source workspace links."""

    _ensure_no_symlink_components(destination_root, target)
    metadata = source.lstat()
    if stat.S_ISLNK(metadata.st_mode):
        link = _remap_source_symlink(source, target, source_root, destination_root)
        target.parent.mkdir(parents=True, exist_ok=True)
        _ensure_no_symlink_components(destination_root, target)
        target.symlink_to(link, target_is_directory=(source.parent / os.readlink(source)).is_dir())
        return
    if stat.S_ISDIR(metadata.st_mode):
        target.mkdir(parents=True, exist_ok=True)
        for child in source.iterdir():
            if child.name in ignored_names:
                continue
            _safe_copy_entry(child, target / child.name, source_root, destination_root, ignored_names)
        return
    if stat.S_ISREG(metadata.st_mode):
        target.parent.mkdir(parents=True, exist_ok=True)
        _ensure_no_symlink_components(destination_root, target)
        shutil.copy2(source, target, follow_symlinks=False)
        return
    raise GateError(f"unsupported source filesystem entry: {source}")


def _copy_tree(source_root: Path, destination: Path) -> None:
    ignored_names = frozenset({".git", ".pytest_cache", ".mypy_cache", ".ruff_cache", "node_modules", ".venv"})
    destination.mkdir(parents=True, exist_ok=True)
    for child in source_root.iterdir():
        if child.name in ignored_names:
            continue
        _safe_copy_entry(child, destination / child.name, source_root, destination, ignored_names)
    _provision_install_artifacts(source_root, destination)


def _provision_install_artifacts(source_root: Path, destination: Path) -> None:
    source = source_root / "node_modules"
    if source.exists() or source.is_symlink():
        target = destination / "node_modules"
        _remove_scratch_path(destination, target)
        _safe_copy_entry(source, target, source_root, destination)

    source = source_root / ".venv"
    if source.exists() or source.is_symlink():
        target = destination / ".venv"
        _remove_scratch_path(destination, target)
        _ensure_no_symlink_components(destination, target)
        target.symlink_to(source, target_is_directory=source.is_dir())


def _safe_extract_archive(archive: tarfile.TarFile, destination: Path) -> None:
    for member in archive:
        member_path = Path(member.name)
        if member_path.is_absolute() or ".." in member_path.parts:
            raise GateError(f"archive member escapes scratch tree: {member.name}")
        target = destination.joinpath(*member_path.parts)
        _ensure_no_symlink_components(destination, target)
        if member.isdir():
            target.mkdir(parents=True, exist_ok=True)
        elif member.issym():
            target.parent.mkdir(parents=True, exist_ok=True)
            _ensure_no_symlink_components(destination, target)
            target.symlink_to(member.linkname)
        elif member.isfile():
            source = archive.extractfile(member)
            if source is None:
                raise GateError(f"archive member has no file payload: {member.name}")
            target.parent.mkdir(parents=True, exist_ok=True)
            _ensure_no_symlink_components(destination, target)
            with target.open("wb") as handle:
                shutil.copyfileobj(source, handle)
            target.chmod(member.mode & 0o7777)
        else:
            raise GateError(f"unsupported archive member: {member.name}")


def _materialise_base(source_root: Path, base: str, destination: Path, tests: list[str]) -> None:
    for child in destination.iterdir():
        if child.is_symlink() or child.is_file():
            child.unlink()
        elif child.is_dir():
            shutil.rmtree(child)
    archive = subprocess.run(
        ["git", "-C", str(source_root), "archive", base],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if archive.returncode != 0:
        raise GateError(f"unable to materialise merge base {base}: {archive.stderr.decode().strip()}")
    with tarfile.open(fileobj=io.BytesIO(archive.stdout), mode="r:") as tar:
        _safe_extract_archive(tar, destination)

    git_commands = (
        (["init", "--quiet"], "initialise the materialised base repository"),
        (["add", "-A"], "stage the materialised base repository"),
        (
            [
                "-c",
                "user.name=Provenant change gates",
                "-c",
                "user.email=change-gates@example.invalid",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                f"materialise merge base {base}",
            ],
            "commit the materialised base repository",
        ),
    )
    for arguments, operation in git_commands:
        completed = subprocess.run(
            ["git", "-C", str(destination), *arguments],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        if completed.returncode != 0:
            detail = completed.stderr.strip() or completed.stdout.strip()
            raise GateError(f"unable to {operation}: {detail}")
    update_ref = subprocess.run(
        ["git", "-C", str(destination), "update-ref", "refs/remotes/origin/main", "HEAD"],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if update_ref.returncode != 0:
        detail = update_ref.stderr.strip() or update_ref.stdout.strip()
        raise GateError(f"unable to expose materialised base as origin/main: {detail}")

    # Changed tests run in the base tree but must not become part of origin/main.
    for relative in tests:
        source = source_root / relative
        target = destination / relative
        if source.is_symlink() or not source.is_file():
            raise GateError(f"changed test is unavailable in the source tree: {relative}")
        _ensure_no_symlink_components(source_root, source)
        _ensure_no_symlink_components(destination, target)
        target.parent.mkdir(parents=True, exist_ok=True)
        _ensure_no_symlink_components(destination, target)
        shutil.copy2(source, target, follow_symlinks=False)

    _provision_install_artifacts(source_root, destination)

    source_attestation = source_root / Path("runtime/agent-fabric/.npm-ci-attestation")
    if source_attestation.is_file() and not source_attestation.is_symlink():
        target_attestation = destination / Path("runtime/agent-fabric/.npm-ci-attestation")
        _ensure_no_symlink_components(source_root, source_attestation)
        _ensure_no_symlink_components(destination, target_attestation)
        target_attestation.parent.mkdir(parents=True, exist_ok=True)
        _ensure_no_symlink_components(destination, target_attestation)
        shutil.copy2(source_attestation, target_attestation, follow_symlinks=False)


@contextmanager
def _temporary_tree(source_root: Path, scratch_root: Path):
    scratch_root.mkdir(parents=True, exist_ok=True)
    destination = Path(tempfile.mkdtemp(prefix="gate-", dir=scratch_root))
    try:
        _copy_tree(source_root, destination)
        yield destination
    finally:
        # The scratch tree is disposable and git can leave `.git` busy inside it
        # once the base has been materialised. CI hit exactly that, and the
        # OSError escaped before the gate had printed anything, so the run
        # reported a crash with no verdict at all. Failing to delete a scratch
        # copy says nothing about the change under test.
        try:
            shutil.rmtree(destination)
        except OSError as error:
            print(f"gate scratch tree left behind at {destination}: {error}")


def _targets(commands: list[str] | dict[str, str], tests: list[str]) -> list[str | None]:
    if not tests:
        return [None]
    command_values = commands.values() if isinstance(commands, dict) else commands
    if not any("{test}" in command for command in command_values):
        raise GateError("--test targets require at least one command containing {test}")
    return tests


def _uses_ts_command(commands: list[str] | dict[str, str], tests: list[str]) -> bool:
    if not isinstance(commands, dict):
        return False
    return any(
        target is None or Path(target).suffix.casefold() != ".py"
        for target in _targets(commands, tests)
    )


def _provision_protocol_build(
    commands: list[str] | dict[str, str], cwd: Path, tests: list[str]
) -> None:
    if not _uses_ts_command(commands, tests):
        return
    build_script = cwd / "scripts" / "agent-fabric-protocol-build"
    if not build_script.is_file():
        raise GateError(f"materialised TS tree is missing {build_script}")
    environment = os.environ.copy()
    environment["AGENTS_HOME"] = str(cwd)
    try:
        completed = subprocess.run(
            [str(build_script)],
            cwd=cwd,
            env=environment,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            check=False,
        )
    except OSError as exc:
        raise GateError(f"unable to run protocol build in materialised tree: {exc}") from exc
    if completed.returncode != 0:
        detail = (completed.stdout or "").strip()
        suffix = f": {detail}" if detail else ""
        raise GateError(
            f"protocol build failed in materialised TS tree "
            f"(returncode={completed.returncode}){suffix}"
        )


def _refresh_materialised_npm_attestation(cwd: Path) -> None:
    writer = cwd / "runtime" / "agent-fabric" / "scripts" / "write-npm-ci-attestation.mjs"
    attestation = cwd / "runtime" / "agent-fabric" / ".npm-ci-attestation"
    _ensure_no_symlink_components(cwd, writer)
    _ensure_no_symlink_components(cwd, attestation)
    if not writer.is_file():
        return
    try:
        completed = subprocess.run(
            ["node", str(writer), str(cwd)],
            cwd=cwd,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            check=False,
        )
    except OSError as exc:
        raise GateError(f"unable to refresh npm install attestation in materialised tree: {exc}") from exc
    if completed.returncode != 0:
        detail = (completed.stdout or "").strip()
        suffix = f": {detail}" if detail else ""
        raise GateError(
            f"npm install attestation refresh failed in materialised tree "
            f"(returncode={completed.returncode}){suffix}"
        )


@contextmanager
def _current_npm_attestation_library(source_root: Path, destination: Path):
    """Expose the current verifier to a materialised protocol preflight only."""

    source = source_root / Path("runtime/agent-fabric/scripts/lib/npm-install-attestation.mjs")
    target = destination / Path("runtime/agent-fabric/scripts/lib/npm-install-attestation.mjs")
    _ensure_no_symlink_components(source_root, source)
    _ensure_no_symlink_components(destination, target)
    if source.is_symlink() or not source.is_file() or target.is_symlink() or not target.is_file():
        yield
        return

    original = target.read_bytes()
    original_stat = target.stat()
    _ensure_no_symlink_components(destination, target)
    shutil.copy2(source, target, follow_symlinks=False)
    try:
        yield
    finally:
        _ensure_no_symlink_components(destination, target)
        target.write_bytes(original)
        os.utime(
            target,
            ns=(original_stat.st_atime_ns, original_stat.st_mtime_ns),
            follow_symlinks=False,
        )


def _run_suite(
    commands: list[str] | dict[str, str],
    cwd: Path,
    tests: list[str],
    protocol_build_source_root: Path | None = None,
) -> list[CommandResult]:
    if _uses_ts_command(commands, tests):
        if protocol_build_source_root is None:
            _refresh_materialised_npm_attestation(cwd)
            _provision_protocol_build(commands, cwd, tests)
        else:
            with _current_npm_attestation_library(protocol_build_source_root, cwd):
                _refresh_materialised_npm_attestation(cwd)
                _provision_protocol_build(commands, cwd, tests)

    def commands_for_target(target: str | None) -> list[str]:
        if not isinstance(commands, dict):
            return commands
        language = "py" if target and Path(target).suffix.casefold() == ".py" else "ts"
        return [commands[language]]

    return [
        run_command(command, cwd, target)
        for target in _targets(commands, tests)
        for command in commands_for_target(target)
    ]


def _print_output(result: CommandResult) -> None:
    print(
        f"COMMAND classification={result.classification.value} returncode={result.returncode}: "
        f"{result.command}"
    )
    if result.output:
        print(result.output.rstrip())


def target_existed_at_base(base_root: Path, target: str) -> bool:
    """Check the merge-base tree, not the current test copy, for a target."""

    completed = subprocess.run(
        ["git", "-C", str(base_root), "cat-file", "-e", f"origin/main:{target}"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    return completed.returncode == 0


def right_reason_red_evidence(result: CommandResult, target_existed: bool) -> str | None:
    """Return the accepted evidence kind, or ``None`` for a defective red."""

    del target_existed
    if result.returncode == 0:
        return None
    if result.classification is FailureClass.ASSERTION:
        return "assertion"
    return None


def _all_assertion_failures(results: list[CommandResult]) -> bool:
    return bool(results) and all(result.classification is FailureClass.ASSERTION for result in results)


def gate_right_reason_red(
    source_root: Path,
    base: str,
    commands: list[str],
    tests: list[str],
    scratch_root: Path,
) -> int:
    with _temporary_tree(source_root, scratch_root) as directory:
        base_root = Path(directory)
        _materialise_base(source_root, base, base_root, tests)
        results = _run_suite(
            commands,
            base_root,
            tests,
            protocol_build_source_root=source_root,
        )
        targets = _targets(commands, tests)
        evidence = [
            right_reason_red_evidence(result, target_existed_at_base(base_root, target))
            for result, target in zip(results, targets, strict=True)
        ]
    for result in results:
        _print_output(result)
    # Name every target and its verdict. The aggregate line below blocks the
    # merge, and on its own it says only that something was rejected, leaving a
    # reader to rerun the gate by hand and bisect the targets to find out which.
    for target, result, reason in zip(targets, results, evidence, strict=True):
        verdict = "REJECTED" if reason is None else reason.upper()
        print(
            f"TARGET {target or '(whole suite)'} status={verdict} "
            f"classification={result.classification.value} returncode={result.returncode}"
        )
    assertion_count = evidence.count("assertion")
    new_target_count = evidence.count("new-target")
    rejected_count = evidence.count(None)
    if rejected_count:
        print(
            "RIGHT_REASON_RED: FAIL "
            f"tests={len(results)} assertion={assertion_count} "
            f"new-target={new_target_count} rejected={rejected_count}"
        )
        return 1
    print(
        "RIGHT_REASON_RED: PASS "
        f"tests={len(results)} assertion={assertion_count} new-target={new_target_count}"
    )
    return 0


def _apply_reverse_hunk(root: Path, hunk: DiffHunk) -> None:
    path = root / hunk.path
    _ensure_no_symlink_components(root, path)
    if path.is_symlink() or not path.is_file():
        raise GateError(f"reverse probe target is absent: {hunk.path}")
    original = path.read_text(encoding="utf-8")
    lines = original.splitlines()
    start = max(hunk.new_start - 1, 0)
    expected = list(hunk.new_lines)
    actual = lines[start : start + len(expected)]
    if actual != expected:
        raise GateError(
            f"reverse probe hunk does not match current source at {hunk.path}:{hunk.new_start}"
        )
    lines[start : start + len(expected)] = hunk.old_lines
    suffix = "\n" if original.endswith("\n") else ""
    _ensure_no_symlink_components(root, path)
    path.write_text("\n".join(lines) + suffix, encoding="utf-8")


def gate_revert_probe(
    source_root: Path,
    hunks: list[DiffHunk],
    commands: list[str],
    tests: list[str],
    scratch_root: Path,
) -> int:
    probes = production_hunks(hunks)
    if not probes:
        raise GateError("revert-probe found no changed production hunks")
    survivors: list[DiffHunk] = []
    inconclusive: list[DiffHunk] = []
    for index, hunk in enumerate(probes, 1):
        with _temporary_tree(source_root, scratch_root) as directory:
            probe_root = Path(directory)
            try:
                _apply_reverse_hunk(probe_root, hunk)
            except GateError as exc:
                print(f"HUNK {index} path={hunk.path} status=INVALID reason={exc}")
                return 1
            try:
                results = _run_suite(
                    commands,
                    probe_root,
                    tests,
                    protocol_build_source_root=source_root,
                )
            except GateError as exc:
                # Reverting one hunk can leave a deliberately incomplete
                # implementation that cannot reach the test process. That
                # is inconclusive evidence, and therefore a failed probe.
                inconclusive.append(hunk)
                print(f"HUNK {index} path={hunk.path} status=INCONCLUSIVE non-assertion red: {exc}")
                continue
        for result in results:
            _print_output(result)
        # Three outcomes, and only one of them is a finding.
        #
        # A SURVIVOR is a suite that stayed GREEN with the hunk reverted: the
        # tests genuinely do not constrain the change, which is what this gate
        # exists to catch. A red made of assertion failures KILLED the hunk.
        #
        # Anything else is INCONCLUSIVE, not a survivor. Reverting a hunk can
        # break the very import the tests need, and a suite that could not run
        # is evidence of nothing. Inconclusive evidence fails the probe rather
        # than laundering uncertainty into a passing gate.
        failures = [result for result in results if result.returncode != 0]
        if len(failures) == 0:
            survivors.append(hunk)
            print(f"HUNK {index} path={hunk.path} status=SURVIVED suite stayed green")
        elif any(result.classification is not FailureClass.ASSERTION for result in failures):
            inconclusive.append(hunk)
            print(f"HUNK {index} path={hunk.path} status=INCONCLUSIVE non-assertion red")
        elif len(failures) != len(results):
            # This interface carries no machine-readable proof that the green
            # target is inapplicable to the reverted hunk. Mixed evidence is
            # therefore inconclusive and cannot certify that the hunk was killed.
            inconclusive.append(hunk)
            print(f"HUNK {index} path={hunk.path} status=INCONCLUSIVE mixed assertion/green evidence")
        else:
            # Every target ran and every red is a meaningful assertion red.
            print(f"HUNK {index} path={hunk.path} status=KILLED")
    if survivors or inconclusive:
        print(
            f"REVERT_PROBE: FAIL hunks={len(probes)} survivors={len(survivors)} "
            f"inconclusive={len(inconclusive)}"
        )
        return 1
    print(
        f"REVERT_PROBE: PASS hunks={len(probes)} "
        f"killed={len(probes) - len(inconclusive)} survivors=0 "
        f"inconclusive={len(inconclusive)}"
    )
    return 0


def _write_mutant(root: Path, mutant: Mutant) -> None:
    path = root / mutant.path
    _ensure_no_symlink_components(root, path)
    if path.is_symlink() or not path.is_file():
        raise GateError(f"mutant source is not a regular scratch file: {mutant.path}")
    lines = path.read_text(encoding="utf-8").splitlines()
    if lines[mutant.line - 1] != mutant.before:
        raise GateError(f"mutant source drift at {mutant.path}:{mutant.line}")
    if mutant.after is None:
        del lines[mutant.line - 1]
    else:
        lines[mutant.line - 1] = mutant.after
    _ensure_no_symlink_components(root, path)
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def gate_changed_line_mutation(
    source_root: Path,
    mutants: list[Mutant],
    commands: list[str],
    tests: list[str],
    scratch_root: Path,
    risk: str,
) -> int:
    if not mutants:
        raise GateError("changed-lines-only mutation found no supported executable mutants")
    with _temporary_tree(source_root, scratch_root) as directory:
        baseline = _run_suite(commands, Path(directory), tests)
    for result in baseline:
        _print_output(result)
    invalid_baseline = [result for result in baseline if result.classification is not FailureClass.PASS]
    if invalid_baseline:
        print(
            "CHANGED_LINES_MUTATION: FAIL "
            f"baseline_results={len(baseline)} invalid_baseline={len(invalid_baseline)}"
        )
        return 1
    started = time.monotonic()
    survivors: list[Mutant] = []
    killed = 0
    for index, mutant in enumerate(mutants, 1):
        with _temporary_tree(source_root, scratch_root) as directory:
            mutant_root = Path(directory)
            _write_mutant(mutant_root, mutant)
            results = _run_suite(commands, mutant_root, tests)
        for result in results:
            _print_output(result)
        if not _all_assertion_failures(results):
            survivors.append(mutant)
            print(f"MUTANT {index} path={mutant.path}:{mutant.line} status=INVALID {mutant.description}")
        else:
            killed += 1
            print(f"MUTANT {index} path={mutant.path}:{mutant.line} status=KILLED {mutant.description}")
    elapsed = time.monotonic() - started
    print(
        f"CHANGED_LINES_MUTATION: {'PASS' if not survivors or risk not in {'crucial', 'terminal'} else 'FAIL'} "
        f"mutants={len(mutants)} killed={killed} survivors={len(survivors)} "
        f"wall_seconds={elapsed:.3f}"
    )
    return 1 if survivors and risk in {"crucial", "terminal"} else 0


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gate", choices=("right-reason-red", "revert-probe", "changed-lines-only"))
    parser.add_argument("--base", default="origin/main")
    parser.add_argument("--source-root", type=Path, default=Path.cwd())
    parser.add_argument("--scratch-root", type=Path, required=True)
    parser.add_argument("--test-command-py", required=True)
    parser.add_argument("--test-command-ts", required=True)
    parser.add_argument("--test", dest="tests", action="append", default=[])
    parser.add_argument("--risk", choices=("routine", "substantial", "crucial", "terminal"), default="crucial")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    source_root = args.source_root.resolve()
    diff_text = git_diff(source_root, args.base)
    hunks = parse_diff(diff_text)
    tests = args.tests or changed_test_paths(hunks)
    commands = {"py": args.test_command_py, "ts": args.test_command_ts}
    if not tests:
        if production_hunks(hunks):
            raise GateError("production changes have no new or changed tests")
        print(f"{args.gate.upper().replace('-', '_')}: SKIP tests=0 production_hunks=0")
        return 0
    if args.gate != "right-reason-red" and not production_hunks(hunks):
        print(f"{args.gate.upper().replace('-', '_')}: SKIP production_hunks=0")
        return 0
    if args.gate == "right-reason-red":
        return gate_right_reason_red(source_root, args.base, commands, tests, args.scratch_root.resolve())
    if args.gate == "revert-probe":
        return gate_revert_probe(source_root, hunks, commands, tests, args.scratch_root.resolve())
    mutants = build_mutants(source_root, hunks)
    return gate_changed_line_mutation(
        source_root, mutants, commands, tests, args.scratch_root.resolve(), args.risk
    )


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except GateError as exc:
        print(f"CHANGE_GATES: FAIL {exc}", file=sys.stderr)
        raise SystemExit(2)
