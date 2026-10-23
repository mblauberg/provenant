#!/usr/bin/env python3
"""Catch fixed ISO dates used with now-relative windows in Python tests."""

from __future__ import annotations

import ast
import re
import subprocess
import sys
from datetime import date
from pathlib import Path


ISO_DATE = re.compile(
    r"^\d{4}-\d{2}-\d{2}(?:[T ]\d{2}:\d{2}(?::\d{2}(?:\.\d+)?)?(?:Z|[+-]\d{2}:?\d{2})?)?$"
)
ISO_DATE_PREFIX = re.compile(r"^\d{4}-\d{2}-\d{2}")
ISO_DATE_LITERAL = re.compile(
    r"['\"](\d{4}-\d{2}-\d{2}(?:T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?)?)['\"]"
)
WINDOW = re.compile(r"\b(?:now|today|utcnow)\s*\(|\btimedelta\s*\(|\bnow\b|\btoday\b")


def _functions(node: ast.AST):
    for child in ast.iter_child_nodes(node):
        if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef)):
            yield child
        yield from _functions(child)


def _start(function: ast.FunctionDef | ast.AsyncFunctionDef) -> int:
    return min([function.lineno, *(item.lineno for item in function.decorator_list)])


def _assigned_names(tree: ast.AST, line: int) -> set[str]:
    assignments = [
        node for node in ast.walk(tree)
        if isinstance(node, (ast.Assign, ast.AnnAssign, ast.NamedExpr))
        and node.lineno <= line <= (node.end_lineno or node.lineno)
    ]
    if not assignments:
        return set()
    assignment = min(
        assignments,
        key=lambda node: (node.end_lineno or node.lineno) - node.lineno,
    )
    targets = assignment.targets if isinstance(assignment, ast.Assign) else [assignment.target]
    return {
        node.id
        for target in targets
        for node in ast.walk(target)
        if isinstance(node, ast.Name)
    }


def _loaded_names(function: ast.FunctionDef | ast.AsyncFunctionDef) -> set[str]:
    names = [function]
    pending = list(ast.iter_child_nodes(function))
    while pending:
        node = pending.pop()
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef, ast.Lambda)):
            continue
        names.append(node)
        pending.extend(ast.iter_child_nodes(node))
    loaded = {
        node.id for node in names
        if isinstance(node, ast.Name) and isinstance(node.ctx, ast.Load)
    }
    attributes = {
        node.attr for node in names
        if isinstance(node, ast.Attribute) and isinstance(node.ctx, ast.Load)
    }
    local = {
        node.id for node in names
        if isinstance(node, ast.Name) and isinstance(node.ctx, (ast.Store, ast.Del))
    }
    local.update(
        argument.arg
        for argument in [
            *function.args.posonlyargs,
            *function.args.args,
            *function.args.kwonlyargs,
        ]
    )
    if function.args.vararg:
        local.add(function.args.vararg.arg)
    if function.args.kwarg:
        local.add(function.args.kwarg.arg)
    global_names = {
        name for node in names if isinstance(node, ast.Global) for name in node.names
    }
    return (loaded - (local - global_names)) | attributes


def scan_file(path: Path) -> list[tuple[int, str]]:
    source = path.read_text(encoding="utf-8")
    lines = source.splitlines()
    if path.suffix in {".ts", ".js", ".mjs"}:
        if "Date.now(" not in source and "new Date()" not in source:
            return []
        return [
            (line_number, match.group(1))
            for line_number, line in enumerate(lines, 1)
            if "timebomb: allow" not in line
            for match in ISO_DATE_LITERAL.finditer(line)
        ]
    tree = ast.parse(source, filename=str(path))
    findings: list[tuple[int, str]] = []
    functions = list(_functions(tree))
    window_functions = {
        function.name for function in functions
        if WINDOW.search(ast.get_source_segment(source, function) or "")
    }
    referenced_helpers = {
        node.func.id
        for function in functions if function.name in window_functions
        for node in ast.walk(function)
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
    }
    active_names = {
        name
        for function in functions
        if function.name in window_functions | referenced_helpers
        for name in _loaded_names(function)
    }
    candidates: list[tuple[ast.AST, str]] = []
    for node in ast.walk(tree):
        if (
            isinstance(node, ast.Constant)
            and isinstance(node.value, str)
            and ISO_DATE.fullmatch(node.value)
        ):
            candidates.append((node, node.value))
        elif isinstance(node, ast.JoinedStr):
            candidates.extend(
                (part, match.group())
                for part in node.values
                if isinstance(part, ast.Constant) and isinstance(part.value, str)
                if (match := ISO_DATE_PREFIX.match(part.value))
            )
    for node, value in candidates:
        try:
            date.fromisoformat(value[:10])
        except ValueError:
            continue
        containing = [
            function for function in functions
            if _start(function) <= node.lineno <= (function.end_lineno or function.lineno)
        ]
        if containing:
            function = min(containing, key=lambda item: (item.end_lineno or item.lineno) - item.lineno)
            has_window = function.name in window_functions or function.name in referenced_helpers
        else:
            fixture_names = _assigned_names(tree, node.lineno)
            # Unassigned module-level dates keep the file-wide fallback.
            has_window = (
                bool(fixture_names & active_names) if fixture_names else bool(window_functions)
            )
        line = lines[node.lineno - 1]
        if has_window and "timebomb: allow" not in line:
            findings.append((node.lineno, value))
    return findings


def scan(root: Path) -> list[tuple[Path, int, str]]:
    repo = root.resolve().parent if root.name == "tests" else root.resolve()
    tests = repo / "tests"
    runtime_tests = repo / "runtime" / "fabric" / "tests"
    base = subprocess.run(
        ["git", "merge-base", "origin/main", "HEAD"], cwd=repo,
        capture_output=True, text=True, check=False,
    )
    if base.returncode != 0:
        detail = base.stderr.strip()
        suffix = f": {detail}" if detail else ""
        print(
            "warning: could not find merge-base with origin/main; "
            f"skipping timebomb fixture lint{suffix}",
            file=sys.stderr,
        )
        return []
    diff = subprocess.run(
        [
            "git", "diff", "--unified=0", base.stdout.strip(), "HEAD", "--",
            "tests", "runtime/fabric/tests",
        ],
        cwd=repo, capture_output=True, text=True, check=False,
    )
    if diff.returncode != 0:
        raise RuntimeError(diff.stderr.strip() or "git diff failed")
    changed = {}
    current: Path | None = None
    for line in diff.stdout.splitlines():
        if line.startswith("+++ b/"):
            current = (repo / line[6:]).resolve()
        elif line.startswith("@@") and current is not None:
            match = re.search(r"\+(\d+)(?:,(\d+))?", line)
            if match:
                start = int(match.group(1))
                count = int(match.group(2) or "1")
                changed.setdefault(current, set()).update(range(start, start + count))
    findings = []
    paths = [*tests.rglob("test*.py"), *runtime_tests.glob("*.test.ts")]
    for path in sorted(paths):
        if path.name == "test_timebomb_fixture_lint.py":
            continue
        added_lines = changed.get(path.resolve(), set()) if changed is not None else None
        findings.extend(
            (path, line, value) for line, value in scan_file(path)
            if added_lines is None or _context_changed(path, line, added_lines)
        )
    return findings


def _context_changed(path: Path, line: int, added_lines: set[int]) -> bool:
    source = path.read_text(encoding="utf-8")
    if path.suffix == ".py":
        tree = ast.parse(source, filename=str(path))
        functions = [
            node for node in ast.walk(tree)
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
        ]
        spans = [
            (_start(node), node.end_lineno or node.lineno)
            for node in functions
            if _start(node) <= line <= (node.end_lineno or node.lineno)
        ]
        if not spans:
            if line in added_lines:
                return True
            fixture_names = _assigned_names(tree, line)
            if not fixture_names:
                return False
            changed_functions = [
                node for node in functions
                if any(
                    node.lineno <= added <= (node.end_lineno or node.lineno)
                    for added in added_lines
                )
            ]
            if any(
                fixture_names & _loaded_names(function)
                for function in changed_functions
            ):
                return True
            return any(
                isinstance(node, ast.Name)
                and isinstance(node.ctx, ast.Load)
                and node.id in fixture_names
                for statement in tree.body
                if getattr(statement, "lineno", None) in added_lines
                and not isinstance(statement, (ast.FunctionDef, ast.AsyncFunctionDef))
                for node in ast.walk(statement)
            )
        start, end = min(spans, key=lambda span: span[1] - span[0])
        helper = next(
            (
                node for node in functions
                if node.lineno == start and (node.end_lineno or node.lineno) == end
            ),
            None,
        )
        if helper is not None:
            changed_functions = [
                node for node in functions
                if any(
                    node.lineno <= added <= (node.end_lineno or node.lineno)
                    for added in added_lines
                )
            ]
            if any(
                isinstance(node, ast.Name) and node.id == helper.name
                for function in changed_functions for node in ast.walk(function)
            ):
                return True
    else:
        lines = source.splitlines()
        starts = [index for index, text in enumerate(lines, 1) if re.search(r"\b(?:it|test)\s*(?:\.each\([^\n]*\)\s*)?\(", text)]
        start = max((value for value in starts if value <= line), default=0)
        end = min((value - 1 for value in starts if value > line), default=len(lines))
    if start == 0:
        return bool(added_lines)
    return any(start <= added <= end for added in added_lines)


def main(argv: list[str] | None = None) -> int:
    root = Path((argv or sys.argv[1:])[0]) if (argv or sys.argv[1:]) else Path(__file__).resolve().parents[1]
    findings = scan(root)
    for path, line, value in findings:
        print(
            f"{path}:{line}: fixed ISO date {value!r} in a now-relative test; "
            "add '# timebomb: allow <reason>' if intentional"
        )
    return 1 if findings else 0


if __name__ == "__main__":
    raise SystemExit(main())
