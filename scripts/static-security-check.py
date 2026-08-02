"""Fail on a small, deterministic set of high-risk Python call patterns."""

from __future__ import annotations

import argparse
import ast
import json
import os
from pathlib import Path
import sys


EXCLUDED = {".git", ".agent-run", ".worktrees", "node_modules", "__pycache__", ".venv", "venv"}
DANGEROUS_CALLS = {"eval", "exec", "os.system", "pickle.load", "pickle.loads"}


def dotted(node: ast.AST) -> str:
    if isinstance(node, ast.Name):
        return node.id
    if isinstance(node, ast.Attribute):
        parent = dotted(node.value)
        return f"{parent}.{node.attr}" if parent else node.attr
    return ""


def canonical_name(node: ast.AST, aliases: dict[str, str]) -> str:
    value = dotted(node)
    first, separator, rest = value.partition(".")
    return aliases.get(first, first) + (separator + rest if separator else "")


def _check_call(node: ast.Call, aliases: dict[str, str], path: Path, findings: list[dict[str, object]]) -> None:
    raw_name = dotted(node.func)
    root_name = raw_name.partition(".")[0]
    resolved_binding = root_name in aliases
    name = canonical_name(node.func, aliases)
    rule = ""
    if name in {"eval", "exec"} or (resolved_binding and name in DANGEROUS_CALLS):
        rule = "dangerous-dynamic-call"
    elif resolved_binding and name.startswith("subprocess.") and any(
        keyword.arg == "shell" and isinstance(keyword.value, ast.Constant) and keyword.value.value is True
        for keyword in node.keywords
    ):
        rule = "subprocess-shell-true"
    elif resolved_binding and name == "yaml.load":
        loader = next((canonical_name(keyword.value, aliases) for keyword in node.keywords if keyword.arg == "Loader"), "")
        if loader not in {"yaml.SafeLoader", "yaml.CSafeLoader"}:
            rule = "unsafe-yaml-load"
    if rule:
        findings.append({"path": str(path), "line": node.lineno, "rule": rule, "detail": name})


class _CallVisitor(ast.NodeVisitor):
    def __init__(self, aliases: dict[str, str], path: Path, findings: list[dict[str, object]]) -> None:
        self.aliases = aliases
        self.path = path
        self.findings = findings

    def visit_Call(self, node: ast.Call) -> None:  # noqa: N802 - ast visitor API
        _check_call(node, self.aliases, self.path, self.findings)
        self.generic_visit(node)

    def visit_FunctionDef(self, node: ast.FunctionDef) -> None:  # noqa: N802
        return

    visit_AsyncFunctionDef = visit_FunctionDef

    def visit_ClassDef(self, node: ast.ClassDef) -> None:  # noqa: N802
        return


def _inspect(node: ast.AST | None, aliases: dict[str, str], path: Path, findings: list[dict[str, object]]) -> None:
    if node is not None:
        _CallVisitor(aliases, path, findings).visit(node)


def _bind(target: ast.AST, value: ast.AST | None, aliases: dict[str, str]) -> None:
    names = [item.id for item in ast.walk(target) if isinstance(item, ast.Name)]
    resolved = canonical_name(value, aliases) if isinstance(value, (ast.Name, ast.Attribute)) else ""
    for name in names:
        if resolved:
            aliases[name] = resolved
        else:
            aliases.pop(name, None)


def _pattern_bindings(pattern: ast.pattern) -> set[str]:
    names: set[str] = set()
    for node in ast.walk(pattern):
        if isinstance(node, (ast.MatchAs, ast.MatchStar)) and node.name:
            names.add(node.name)
        elif isinstance(node, ast.MatchMapping) and node.rest:
            names.add(node.rest)
    return names


def _scan_body(body: list[ast.stmt], aliases: dict[str, str], path: Path, findings: list[dict[str, object]]) -> None:
    """Scan lexical statement order so aliases exist only after their binding."""
    for node in body:
        if isinstance(node, ast.Import):
            for item in node.names:
                bound = item.asname or item.name.split(".")[0]
                aliases[bound] = item.name if item.asname else item.name.split(".")[0]
        elif isinstance(node, ast.ImportFrom) and node.module:
            for item in node.names:
                aliases[item.asname or item.name] = f"{node.module}.{item.name}"
        elif isinstance(node, (ast.Assign, ast.AnnAssign)):
            value = node.value
            _inspect(value, aliases, path, findings)
            targets = node.targets if isinstance(node, ast.Assign) else [node.target]
            for target in targets:
                _bind(target, value, aliases)
        elif isinstance(node, ast.AugAssign):
            _inspect(node.value, aliases, path, findings)
            _bind(node.target, None, aliases)
        elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            for value in [*node.decorator_list, *node.args.defaults, *node.args.kw_defaults]:
                _inspect(value, aliases, path, findings)
            aliases.pop(node.name, None)
            local = dict(aliases)
            for argument in [*node.args.posonlyargs, *node.args.args, *node.args.kwonlyargs]:
                local.pop(argument.arg, None)
            if node.args.vararg:
                local.pop(node.args.vararg.arg, None)
            if node.args.kwarg:
                local.pop(node.args.kwarg.arg, None)
            _scan_body(node.body, local, path, findings)
        elif isinstance(node, ast.ClassDef):
            for value in [*node.decorator_list, *node.bases, *node.keywords]:
                _inspect(value.value if isinstance(value, ast.keyword) else value, aliases, path, findings)
            aliases.pop(node.name, None)
            _scan_body(node.body, dict(aliases), path, findings)
        elif isinstance(node, (ast.If, ast.While)):
            _inspect(node.test, aliases, path, findings)
            _scan_body(node.body, dict(aliases), path, findings)
            _scan_body(node.orelse, dict(aliases), path, findings)
        elif isinstance(node, (ast.For, ast.AsyncFor)):
            _inspect(node.iter, aliases, path, findings)
            branch = dict(aliases)
            _bind(node.target, None, branch)
            _scan_body(node.body, branch, path, findings)
            _scan_body(node.orelse, dict(aliases), path, findings)
        elif isinstance(node, (ast.With, ast.AsyncWith)):
            branch = dict(aliases)
            for item in node.items:
                _inspect(item.context_expr, aliases, path, findings)
                if item.optional_vars:
                    _bind(item.optional_vars, None, branch)
            _scan_body(node.body, branch, path, findings)
        elif isinstance(node, ast.Match):
            _inspect(node.subject, aliases, path, findings)
            for case in node.cases:
                branch = dict(aliases)
                bindings = _pattern_bindings(case.pattern)
                for name in bindings:
                    branch.pop(name, None)
                _inspect(case.guard, branch, path, findings)
                _scan_body(case.body, branch, path, findings)
        elif isinstance(node, (ast.Try, ast.TryStar)):
            _scan_body(node.body, dict(aliases), path, findings)
            for handler in node.handlers:
                branch = dict(aliases)
                if handler.name:
                    branch.pop(handler.name, None)
                _scan_body(handler.body, branch, path, findings)
            _scan_body(node.orelse, dict(aliases), path, findings)
            _scan_body(node.finalbody, dict(aliases), path, findings)
        else:
            _inspect(node, aliases, path, findings)


def scan_file(path: Path) -> list[dict[str, object]]:
    try:
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    except (OSError, UnicodeDecodeError, SyntaxError) as exc:
        return [{"path": str(path), "line": getattr(exc, "lineno", 0) or 0, "rule": "python-parse", "detail": str(exc)}]
    findings: list[dict[str, object]] = []
    _scan_body(tree.body, {}, path, findings)
    return findings


def scan(root: Path) -> list[dict[str, object]]:
    findings = []
    for current, directories, files in os.walk(root):
        directories[:] = sorted(name for name in directories if name not in EXCLUDED)
        for name in sorted(files):
            if name.endswith(".py"):
                findings.extend(scan_file(Path(current) / name))
    return findings


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", nargs="?", type=Path, default=Path(__file__).resolve().parents[1])
    args = parser.parse_args(argv)
    findings = scan(args.root.resolve())
    print(json.dumps({"schema_version": 1, "status": "fail" if findings else "pass", "findings": findings}, indent=2))
    return 1 if findings else 0


if __name__ == "__main__":
    raise SystemExit(main())
