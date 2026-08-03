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
import posixpath
from dataclasses import dataclass
from enum import Enum
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
from pathlib import Path

try:
    from .change_gate_reports import (
        FailureClass,
        classify_structured_report,
        parse_junit_report,
        parse_vitest_report,
    )
    from .change_gate_runner import (
        CommandResult,
        Runner,
        classify_failure as _structured_classify_failure,
        runner_for_command as _structured_runner_for_command,
        run_command as _structured_run_command,
    )
except ImportError:  # pragma: no cover - direct script execution fallback
    from change_gate_reports import (
        FailureClass,
        classify_structured_report,
        parse_junit_report,
        parse_vitest_report,
    )
    from change_gate_runner import (
        CommandResult,
        Runner,
        classify_failure as _structured_classify_failure,
        runner_for_command as _structured_runner_for_command,
        run_command as _structured_run_command,
    )


class GateError(RuntimeError):
    """A gate cannot certify its required evidence."""


class ChangeMode(str, Enum):
    """The closed set of change shapes understood by the gates."""

    BEHAVIOUR = "behaviour"
    IMPORT_HELPER = "import-helper"
    REFACTOR = "refactor"
    TYPE_ONLY = "type-only"


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


_HUNK_RE = re.compile(r"^@@ -(?P<old_start>\d+)(?:,(?P<old_count>\d+))? \+(?P<new_start>\d+)(?:,(?P<new_count>\d+))? @@")
_DIFF_FILE_RE = re.compile(r"^diff --git a/(.+) b/(.+)$")
_TYPE_ONLY_LINE_RE = re.compile(
    r"^\s*(?:"
    r"(?:(?:export\s+)?(?:type|interface)\s+[A-Za-z_$][\w$]*(?:\s*<[^>\n]+>)?"
    r"(?:\s*=\s*[^;\n]+|\s*(?:extends\s+[^\{\n]+)?\{[^}\n]*\})?;?)"
    r"|(?:(?:export\s+)?declare\s+(?:const|let|var)\s+[A-Za-z_$][\w$]*"
    r"(?:\s*:\s*[^;\n]+)?;?)"
    r"|(?:import\s+type\s+[^;\n]+;?)"
    r"|(?:(?:import|export)\s*\{\s*type\s+[A-Za-z_$][\w$]*"
    r"(?:\s+as\s+[A-Za-z_$][\w$]*)?(?:\s*,\s*type\s+"
    r"[A-Za-z_$][\w$]*(?:\s+as\s+[A-Za-z_$][\w$]*)?)*\s*\}\s*"
    r"(?:from\s+['\"][^'\"]+['\"])?\s*;?)"
    r")\s*$"
)


def classify_failure(returncode: int, output: str) -> FailureClass:
    """Expose the explicit legacy classifier, with a partial-revert fallback."""

    implementation = globals().get("_structured_classify_failure")
    if implementation is not None:
        return implementation(returncode, output)
    if returncode == 0:
        return FailureClass.PASS
    folded = output.casefold()
    if any(token in folded for token in ("typeerror:", "referenceerror:", "syntaxerror:")):
        return FailureClass.UNKNOWN
    if "assertionerror" in folded or "assert " in folded:
        return FailureClass.ASSERTION
    if "modulenotfounderror" in folded or "importerror" in folded:
        return FailureClass.IMPORT
    if "collection" in folded:
        return FailureClass.COLLECTION
    return FailureClass.UNKNOWN


def runner_for_command(command: str) -> Runner | None:
    implementation = globals().get("_structured_runner_for_command")
    return implementation(command) if implementation is not None else None


def run_command(
    command: str,
    cwd: Path,
    test_path: str | None = None,
    *,
    runner: Runner | str | None = None,
) -> CommandResult:
    implementation = globals().get("_structured_run_command")
    if implementation is not None:
        return implementation(command, cwd, test_path, runner=runner)
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


def _canonical_module_path(path: str) -> str:
    normalized = posixpath.normpath(path.removeprefix("./")).lstrip("/")
    suffix = Path(normalized).suffix.casefold()
    if suffix in {".py", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"}:
        normalized = normalized[: -len(suffix)]
    if normalized.endswith("/__init__"):
        normalized = normalized[: -len("/__init__")]
    return normalized


def added_module_names(hunks: list[DiffHunk]) -> frozenset[str]:
    """Return canonical path and dotted spellings for newly added modules."""

    names: set[str] = set()
    for hunk in hunks:
        if "--- /dev/null" not in hunk.header or not is_source_path(hunk.path):
            continue
        module = _canonical_module_path(hunk.path)
        if not module:
            continue
        names.update({module, f"./{module}", module.replace("/", ".")})
    return frozenset(names)


def module_is_added(
    unresolved_module: str,
    added_modules: frozenset[str] | set[str],
    importer: str | None = None,
) -> bool:
    """Resolve a structured import against the changed module identities."""

    candidates: set[str] = set()
    raw = unresolved_module.strip().replace("\\", "/")
    if not raw:
        return False
    resolved = raw
    if raw.startswith(".") and importer:
        resolved = posixpath.normpath(posixpath.join(posixpath.dirname(importer), raw))
    candidates.add(resolved)
    candidates.add(raw)
    for candidate in tuple(candidates):
        canonical = _canonical_module_path(candidate)
        if canonical:
            candidates.update({canonical, f"./{canonical}", canonical.replace("/", ".")})
    if "/" not in raw and not Path(raw).suffix and "." in raw:
        candidates.add(raw.replace(".", "/"))
    return bool(candidates & set(added_modules))


def hunk_mode(hunk: DiffHunk) -> ChangeMode | None:
    """Classify a TypeScript hunk only when every changed line is bounded type syntax."""

    if Path(hunk.path).suffix.casefold() not in {".ts", ".tsx"}:
        return ChangeMode.BEHAVIOUR
    changed = [line[1:] for line in hunk.body if line.startswith(("+", "-"))]
    if not changed:
        return ChangeMode.BEHAVIOUR
    type_only = [_TYPE_ONLY_LINE_RE.fullmatch(line) is not None for line in changed]
    if all(type_only):
        return ChangeMode.TYPE_ONLY
    if any(type_only):
        return None
    return ChangeMode.BEHAVIOUR


def validate_change_mode(hunks: list[DiffHunk], mode: ChangeMode | str) -> None:
    """Reject mixed or misdeclared type-only production hunks."""

    mode = ChangeMode(mode)
    production = production_hunks(hunks)
    mixed = [hunk.path for hunk in production if hunk_mode(hunk) is None]
    if mixed:
        raise GateError(
            "mixed type/runtime production hunk is not certifiable: "
            + ", ".join(sorted(set(mixed)))
        )
    type_only = [hunk for hunk in production if hunk_mode(hunk) is ChangeMode.TYPE_ONLY]
    if mode is ChangeMode.TYPE_ONLY and (
        not type_only or len(type_only) != len(production)
    ):
        raise GateError("type-only mode requires every production hunk to be type-only")
    if type_only and mode is not ChangeMode.TYPE_ONLY and len(type_only) != len(production):
        paths = ", ".join(sorted({hunk.path for hunk in type_only}))
        raise GateError(f"type-only production hunks require --mode type-only: {paths}")


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


def _copy_tree(source_root: Path, destination: Path) -> None:
    def ignore(path: str, names: list[str]) -> set[str]:
        ignored = {".git", ".pytest_cache", ".mypy_cache", ".ruff_cache", "node_modules", ".venv"}
        return {name for name in names if name in ignored}

    destination.mkdir(parents=True, exist_ok=True)
    for child in source_root.iterdir():
        target = destination / child.name
        if child.name in {".git", ".pytest_cache", ".mypy_cache", ".ruff_cache", "node_modules", ".venv"}:
            continue
        if child.is_symlink():
            target.symlink_to(os.readlink(child))
        elif child.is_dir():
            shutil.copytree(child, target, symlinks=True, ignore=ignore)
        else:
            shutil.copy2(child, target)
    for name in ("node_modules", ".venv"):
        source = source_root / name
        target = destination / name
        if source.exists() and not target.exists():
            target.symlink_to(source, target_is_directory=source.is_dir())


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
        tar.extractall(destination)

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
        if not source.is_file():
            raise GateError(f"changed test is unavailable in the source tree: {relative}")
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
    for name in ("node_modules", ".venv"):
        source = source_root / name
        target = destination / name
        if source.exists() and not target.exists():
            target.symlink_to(source, target_is_directory=source.is_dir())


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


def _run_suite(
    commands: list[str] | dict[str, str],
    cwd: Path,
    tests: list[str],
    *,
    fail_fast: bool = False,
) -> list[CommandResult]:
    def commands_for_target(target: str | None) -> list[tuple[str, Runner | str | None]]:
        if not isinstance(commands, dict):
            return [(command, None) for command in commands]
        suffix = Path(target).suffix.casefold() if target else ""
        if suffix == ".py":
            language = "py"
        elif suffix in {".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs"}:
            language = "ts"
        else:
            return [("", "unknown-runner")]
        if language not in commands:
            return [("", "unknown-runner")]
        command = commands[language]
        detector = globals().get("runner_for_command")
        runner = detector(command) if detector is not None else None
        if fail_fast and runner is Runner.PYTEST:
            command = f"{command} --maxfail=1"
        elif fail_fast and runner is Runner.VITEST:
            command = f"{command} --bail=1"
        return [(command, runner)]

    def invoke(command: str, target: str | None, runner: Runner | str | None) -> CommandResult:
        try:
            return run_command(command, cwd, target, runner=runner)
        except TypeError as error:
            if "runner" not in str(error):
                raise
            return run_command(command, cwd, target)

    return [
        invoke(command, target, runner)
        for target in _targets(commands, tests)
        for command, runner in commands_for_target(target)
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


def right_reason_red_evidence(
    result: CommandResult,
    target_existed: bool,
    *,
    mode: ChangeMode | str = ChangeMode.BEHAVIOUR,
    added_modules: frozenset[str] | set[str] = frozenset(),
    importer: str | None = None,
) -> str | None:
    """Return the accepted evidence kind, or ``None`` for a defective red."""

    try:
        mode = ChangeMode(mode)
    except (TypeError, ValueError):
        return None
    if mode is ChangeMode.TYPE_ONLY:
        return (
            "type-only"
            if result.returncode == 0 and result.classification is FailureClass.PASS
            else None
        )
    if mode is ChangeMode.REFACTOR and target_existed:
        if result.returncode == 0 and result.classification is FailureClass.PASS:
            return "refactor"
    if result.returncode == 0:
        return None
    if result.classification is FailureClass.ASSERTION:
        return "assertion"
    if not target_existed:
        return "new-target"
    if (
        mode is ChangeMode.IMPORT_HELPER
        and result.classification in {FailureClass.IMPORT, FailureClass.COLLECTION}
        and result.unresolved_module is not None
        and result.structured_import_evidence
        and module_is_added(result.unresolved_module, added_modules, importer)
    ):
        return "added-module"
    return None


def _all_assertion_failures(results: list[CommandResult]) -> bool:
    return bool(results) and all(result.classification is FailureClass.ASSERTION for result in results)


def gate_right_reason_red(
    source_root: Path,
    base: str,
    commands: list[str] | dict[str, str],
    tests: list[str],
    scratch_root: Path,
    *,
    mode: ChangeMode | str = ChangeMode.BEHAVIOUR,
    added_modules: frozenset[str] | set[str] = frozenset(),
) -> int:
    with _temporary_tree(source_root, scratch_root) as directory:
        base_root = Path(directory)
        _materialise_base(source_root, base, base_root, tests)
        results = _run_suite(commands, base_root, tests)
        targets = _targets(commands, tests)
        evidence = [
            right_reason_red_evidence(
                result,
                target_existed_at_base(base_root, target),
                mode=mode,
                added_modules=added_modules,
                importer=target,
            )
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


def _gate_refactor_no_production(
    source_root: Path,
    base: str,
    commands: list[str] | dict[str, str],
    tests: list[str],
    scratch_root: Path,
    gate_name: str,
) -> int:
    """Run the mandatory non-red gate when refactor work has no production hunk."""

    if not tests:
        raise GateError(f"{gate_name} refactor mode requires a changed test target")
    with _temporary_tree(source_root, scratch_root) as directory:
        base_root = Path(directory)
        _materialise_base(source_root, base, base_root, tests)
        results = _run_suite(commands, base_root, tests)
    for result in results:
        _print_output(result)
    if not results or any(
        result.returncode != 0 or result.classification is not FailureClass.PASS
        for result in results
    ):
        print(
            f"{gate_name}: FAIL mode=refactor production_hunks=0 "
            f"tests={len(results)} survivors=0 inconclusive=1"
        )
        return 1
    if gate_name == "REVERT_PROBE":
        print(
            "REVERT_PROBE: PASS mode=refactor production_hunks=0 "
            "killed=0 survivors=0 inconclusive=0"
        )
    else:
        print(
            "CHANGED_LINES_MUTATION: PASS mode=refactor production_hunks=0 "
            "mutants=0 killed=0 survivors=0 inconclusive=0"
        )
    return 0


def gate_type_only_probe(
    source_root: Path,
    hunks: list[DiffHunk],
    gate_name: str,
    scratch_root: Path,
) -> int:
    """Run the parser-owned reverse and negative-mutation probes for type-only work."""

    probes = production_hunks(hunks)
    if not probes or any(hunk_mode(hunk) is not ChangeMode.TYPE_ONLY for hunk in probes):
        raise GateError("type-only probe requires only type-only production hunks")
    if gate_name == "RIGHT_REASON_RED":
        print(
            "RIGHT_REASON_RED: TYPE_ONLY owner=type-gate "
            f"verified_hunks={len(probes)} tests=0"
        )
        return 0
    if gate_name == "REVERT_PROBE":
        print(
            "REVERT_PROBE: FAIL owner=type-gate "
            f"hunks={len(probes)} survivors=0 inconclusive=1 "
            "reason=no-runtime-evidence"
        )
    else:
        print(
            "CHANGED_LINES_MUTATION: FAIL owner=type-gate "
            f"hunks={len(probes)} survivors=0 inconclusive=1 "
            "reason=no-runtime-evidence"
        )
    return 1


def _apply_reverse_hunk(root: Path, hunk: DiffHunk) -> None:
    path = root / hunk.path
    if not path.is_file():
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
    path.write_text("\n".join(lines) + suffix, encoding="utf-8")


def gate_revert_probe(
    source_root: Path,
    hunks: list[DiffHunk],
    commands: list[str],
    tests: list[str],
    scratch_root: Path,
    *,
    mode: ChangeMode | str = ChangeMode.BEHAVIOUR,
    base: str | None = None,
) -> int:
    mode = ChangeMode(mode)
    probes = production_hunks(hunks)
    if not probes:
        if mode is ChangeMode.REFACTOR and base is not None:
            return _gate_refactor_no_production(
                source_root, base, commands, tests, scratch_root, "REVERT_PROBE"
            )
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
            candidate = (Path("tests") / f"test_{Path(hunk.path).stem}.py").as_posix()
            probe_tests = [candidate] if candidate in tests else tests
            results = _run_suite(commands, probe_root, probe_tests)
        for result in results:
            _print_output(result)
        targets = _targets(commands, locals().get("probe_tests", tests))
        evidence = [
            right_reason_red_evidence(
                result,
                target is None or target_existed_at_base(source_root, target),
            )
            for result, target in zip(results, targets, strict=True)
        ]
        # Three outcomes, and only one of them is a finding.
        #
        # A SURVIVOR is a suite that stayed GREEN with the hunk reverted: the
        # tests genuinely do not constrain the change, which is what this gate
        # exists to catch. A red made of assertion failures KILLED the hunk.
        #
        # Anything else is INCONCLUSIVE, not a survivor. Reverting a hunk can
        # break the very import the tests need, and a suite that could not run
        # is evidence of nothing. Counting that as a survivor reports "this
        # change is unconstrained" on evidence that says only "I could not
        # tell", which is the failure this gate is supposed to prevent, not
        # commit.
        failures = [result for result in results if result.returncode != 0]
        evidence = locals().get("evidence", [])
        if any(
            result.returncode == 0 and result.classification is not FailureClass.PASS
            for result in results
        ):
            inconclusive.append(hunk)
            print(f"HUNK {index} path={hunk.path} status=INCONCLUSIVE unusable evidence")
        elif not failures:
            survivors.append(hunk)
            print(f"HUNK {index} path={hunk.path} status=SURVIVED suite stayed green")
        elif any(reason is None for reason in evidence):
            inconclusive.append(hunk)
            print(f"HUNK {index} path={hunk.path} status=INCONCLUSIVE non-assertion red")
        elif len(failures) != len(results):
            # Mixed evidence: one target caught the revert and another stayed
            # green. Ambiguous is not good enough, so this stays a finding.
            survivors.append(hunk)
            print(f"HUNK {index} path={hunk.path} status=SURVIVED mixed evidence")
        else:
            print(
                f"HUNK {index} path={hunk.path} status=KILLED "
                f"evidence={','.join(sorted(set(evidence)))}"
            )
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
    lines = path.read_text(encoding="utf-8").splitlines()
    if lines[mutant.line - 1] != mutant.before:
        raise GateError(f"mutant source drift at {mutant.path}:{mutant.line}")
    if mutant.after is None:
        del lines[mutant.line - 1]
    else:
        lines[mutant.line - 1] = mutant.after
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def gate_changed_line_mutation(
    source_root: Path,
    mutants: list[Mutant],
    commands: list[str] | dict[str, str],
    tests: list[str],
    scratch_root: Path,
    risk: str,
    *,
    mode: ChangeMode | str = ChangeMode.BEHAVIOUR,
    base: str | None = None,
) -> int:
    mode = ChangeMode(mode)
    if not mutants:
        if mode is ChangeMode.REFACTOR and base is not None:
            return _gate_refactor_no_production(
                source_root, base, commands, tests, scratch_root, "CHANGED_LINES_MUTATION"
            )
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
    inconclusive: list[Mutant] = []
    killed = 0

    def run_mutant_suite(cwd: Path, selected_tests: list[str]) -> list[CommandResult]:
        try:
            return _run_suite(commands, cwd, selected_tests, fail_fast=True)
        except TypeError as error:
            if "fail_fast" not in str(error):
                raise
            return _run_suite(commands, cwd, selected_tests)

    for index, mutant in enumerate(mutants, 1):
        candidate = (Path("tests") / f"test_{Path(mutant.path).stem}.py").as_posix()
        mutant_tests = [candidate] if candidate in tests else tests
        with _temporary_tree(source_root, scratch_root) as directory:
            mutant_root = Path(directory)
            _write_mutant(mutant_root, mutant)
            results = run_mutant_suite(mutant_root, mutant_tests)
        for result in results:
            _print_output(result)
        if _all_assertion_failures(results):
            killed += 1
            print(f"MUTANT {index} path={mutant.path}:{mutant.line} status=KILLED {mutant.description}")
        elif results and all(
            result.returncode == 0 and result.classification is FailureClass.PASS
            for result in results
        ):
            survivors.append(mutant)
            print(f"MUTANT {index} path={mutant.path}:{mutant.line} status=SURVIVED {mutant.description}")
        else:
            inconclusive.append(mutant)
            print(
                f"MUTANT {index} path={mutant.path}:{mutant.line} "
                f"status=INCONCLUSIVE {mutant.description}"
            )
    elapsed = time.monotonic() - started
    print(
        f"CHANGED_LINES_MUTATION: {'PASS' if not survivors and not inconclusive or risk not in {'crucial', 'terminal'} else 'FAIL'} "
        f"mutants={len(mutants)} killed={killed} survivors={len(survivors)} "
        f"inconclusive={len(inconclusive)} wall_seconds={elapsed:.3f}"
    )
    return 1 if (survivors or inconclusive) and risk in {"crucial", "terminal"} else 0


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
    parser.add_argument(
        "--mode",
        type=ChangeMode,
        choices=tuple(mode.value for mode in ChangeMode),
        default=ChangeMode.BEHAVIOUR,
        help="declared change shape for the applicable gate",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    source_root = args.source_root.resolve()
    diff_text = git_diff(source_root, args.base)
    hunks = parse_diff(diff_text)
    production = production_hunks(hunks)
    mode = args.mode
    if mode is ChangeMode.BEHAVIOUR and production and all(
        hunk_mode(hunk) is ChangeMode.TYPE_ONLY for hunk in production
    ):
        mode = ChangeMode.TYPE_ONLY
    validate_change_mode(hunks, mode)
    added_modules = added_module_names(hunks)
    tests = args.tests or changed_test_paths(hunks)
    commands = {"py": args.test_command_py, "ts": args.test_command_ts}
    if mode is ChangeMode.TYPE_ONLY:
        return gate_type_only_probe(
            source_root,
            hunks,
            {
                "right-reason-red": "RIGHT_REASON_RED",
                "revert-probe": "REVERT_PROBE",
                "changed-lines-only": "CHANGED_LINES_MUTATION",
            }[args.gate],
            args.scratch_root.resolve(),
        )
    if not tests:
        if production:
            raise GateError("production changes have no new or changed tests")
        print(f"{args.gate.upper().replace('-', '_')}: SKIP tests=0 production_hunks=0")
        return 0
    if args.gate != "right-reason-red" and not production and mode is not ChangeMode.REFACTOR:
        print(f"{args.gate.upper().replace('-', '_')}: SKIP production_hunks=0")
        return 0
    if args.gate == "right-reason-red":
        return gate_right_reason_red(
            source_root,
            args.base,
            commands,
            tests,
            args.scratch_root.resolve(),
            mode=mode,
            added_modules=added_modules,
        )
    if args.gate == "revert-probe":
        return gate_revert_probe(
            source_root,
            hunks,
            commands,
            tests,
            args.scratch_root.resolve(),
            mode=mode,
            base=args.base,
        )
    mutants = build_mutants(source_root, hunks)
    return gate_changed_line_mutation(
        source_root,
        mutants,
        commands,
        tests,
        args.scratch_root.resolve(),
        args.risk,
        mode=mode,
        base=args.base,
    )


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except GateError as exc:
        print(f"CHANGE_GATES: FAIL {exc}", file=sys.stderr)
        raise SystemExit(2)
