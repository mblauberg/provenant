#!/usr/bin/env python3
"""Catch fixed ISO dates used with now-relative windows in Python tests."""

from __future__ import annotations

import ast
from datetime import date
from pathlib import Path
import re
import subprocess
import sys


ISO_DATE = re.compile(r"^\d{4}-\d{2}-\d{2}(?:[T ]\d{2}:\d{2}(?::\d{2}(?:\.\d+)?)?(?:Z|[+-]\d{2}:?\d{2})?)?$")
ISO_DATE_LITERAL = re.compile(r"['\"](\d{4}-\d{2}-\d{2}(?:T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?)?)['\"]")
WINDOW = re.compile(r"\b(?:now|today|utcnow)\s*\(|\btimedelta\s*\(|\bnow\b|\btoday\b")


def _functions(node: ast.AST):
    for child in ast.iter_child_nodes(node):
        if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef)):
            yield child
        yield from _functions(child)


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
    module_has_window = any(WINDOW.search(ast.get_source_segment(source, function) or "") for function in functions)
    for node in ast.walk(tree):
        if not isinstance(node, ast.Constant) or not isinstance(node.value, str):
            continue
        if not ISO_DATE.fullmatch(node.value):
            continue
        try:
            date.fromisoformat(node.value[:10])
        except ValueError:
            continue
        containing = [function for function in functions if function.lineno <= node.lineno <= (function.end_lineno or function.lineno)]
        if containing:
            function = min(containing, key=lambda item: (item.end_lineno or item.lineno) - item.lineno)
            has_window = bool(WINDOW.search(ast.get_source_segment(source, function) or ""))
        else:
            has_window = module_has_window
        line = lines[node.lineno - 1]
        if has_window and "timebomb: allow" not in line:
            findings.append((node.lineno, node.value))
    return findings


def scan(root: Path) -> list[tuple[Path, int, str]]:
    repo = root.resolve().parent if root.name == "tests" else root.resolve()
    tests = repo / "tests"
    runtime_tests = repo / "runtime" / "fabric" / "tests"
    base = subprocess.run(
        ["git", "merge-base", "origin/main", "HEAD"], cwd=repo,
        capture_output=True, text=True, check=False,
    )
    changed: dict[Path, set[int]] | None = None
    if base.returncode == 0:
        diff = subprocess.run(
            ["git", "diff", "--unified=0", base.stdout.strip(), "HEAD", "--", "tests", "runtime/fabric/tests"],
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
        spans = [
            (node.lineno, node.end_lineno or node.lineno)
            for node in ast.walk(tree)
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
            and node.lineno <= line <= (node.end_lineno or node.lineno)
        ]
        if not spans:
            return bool(added_lines)
        start, end = min(spans, key=lambda span: span[1] - span[0])
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
        print(f"{path}:{line}: fixed ISO date {value!r} in a now-relative test; add '# timebomb: allow <reason>' if intentional")
    return 1 if findings else 0


if __name__ == "__main__":
    raise SystemExit(main())
