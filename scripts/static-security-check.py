#!/usr/bin/env python3
"""Fail on a small, deterministic set of high-risk Python call patterns."""

from __future__ import annotations

import argparse
import ast
import copy
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


def _binding_name(target: ast.AST) -> str:
    return dotted(target) if isinstance(target, (ast.Name, ast.Attribute)) else ""


class _PipeWaitVisitor(ast.NodeVisitor):
    """Find a local Popen pipe wait with no preceding local drain."""

    def __init__(
        self, aliases: dict[str, str], path: Path, findings: list[dict[str, object]]
    ) -> None:
        self.aliases = dict(aliases)
        self.path = path
        self.findings = findings
        self.processes: dict[str, dict[str, object]] = {}
        self.readers: dict[str, set[tuple[str, str]]] = {}

    def _fork(self) -> _PipeWaitVisitor:
        visitor = _PipeWaitVisitor(self.aliases, self.path, self.findings)
        visitor.processes = copy.deepcopy(self.processes)
        visitor.readers = copy.deepcopy(self.readers)
        return visitor

    def _visit_branch(self, body: list[ast.stmt]) -> _PipeWaitVisitor:
        visitor = self._fork()
        for statement in body:
            visitor.visit(statement)
        return visitor

    def _merge_branches(self, branches: list[_PipeWaitVisitor]) -> None:
        alias_names = set.intersection(*(set(branch.aliases) for branch in branches))
        self.aliases = {
            name: branches[0].aliases[name]
            for name in alias_names
            if all(branch.aliases[name] == branches[0].aliases[name] for branch in branches)
        }

        merged_processes: dict[str, dict[str, object]] = {}
        process_names = set().union(*(branch.processes for branch in branches))
        for binding in process_names:
            states = [
                branch.processes[binding]
                for branch in branches
                if binding in branch.processes
            ]
            pipes = set().union(*(state["pipes"] for state in states))
            drained = set.intersection(*(set(state["drained"]) for state in states))
            selector_names = set().union(*(state["selectors"] for state in states))
            selectors = {
                selector: set.intersection(
                    *(set(state["selectors"].get(selector, set())) for state in states)
                )
                for selector in selector_names
            }
            merged_processes[binding] = {
                "pipes": pipes,
                "drained": drained,
                "selectors": selectors,
                "reported": any(bool(state["reported"]) for state in states),
            }
        self.processes = merged_processes

        reader_names = set().union(*(branch.readers for branch in branches))
        self.readers = {
            name: streams
            for name in reader_names
            if (
                streams := set.intersection(
                    *(set(branch.readers.get(name, set())) for branch in branches)
                )
            )
        }

    def _stream_refs(self, node: ast.AST) -> set[tuple[str, str]]:
        refs: set[tuple[str, str]] = set()
        for item in ast.walk(node):
            if not isinstance(item, ast.Attribute) or item.attr not in {"stdout", "stderr"}:
                continue
            owner = dotted(item.value)
            if owner in self.processes:
                refs.add((owner, item.attr))
        return refs

    def _drain(self, refs: set[tuple[str, str]]) -> None:
        for binding, stream in refs:
            state = self.processes.get(binding)
            if state is not None:
                drained = state["drained"]
                assert isinstance(drained, set)
                drained.add(stream)

    def _drain_selector(self, selector: str) -> None:
        for binding, state in self.processes.items():
            pipes = state["pipes"]
            selectors = state["selectors"]
            assert isinstance(pipes, set) and isinstance(selectors, dict)
            if pipes and pipes <= selectors.get(selector, set()):
                self._drain({(binding, stream) for stream in pipes})

    @staticmethod
    def _selector_drain_loops(node: ast.While) -> set[str]:
        selectors = {
            dotted(call.func.value)
            for call in ast.walk(node.test)
            if isinstance(call, ast.Call)
            and isinstance(call.func, ast.Attribute)
            and call.func.attr == "get_map"
        }
        drained: set[str] = set()
        for selector in selectors:
            has_named_drain = any(
                isinstance(call, ast.Call)
                and dotted(call.func) == "_drain"
                and bool(call.args)
                and dotted(call.args[0]) == selector
                for statement in node.body
                for call in ast.walk(statement)
            )
            has_select = any(
                isinstance(call, ast.Call)
                and isinstance(call.func, ast.Attribute)
                and call.func.attr == "select"
                and dotted(call.func.value) == selector
                for statement in node.body
                for call in ast.walk(statement)
            )
            has_read = any(
                isinstance(call, ast.Call)
                and isinstance(call.func, ast.Attribute)
                and call.func.attr in {"read", "read1", "readinto", "readline", "readlines"}
                for statement in node.body
                for call in ast.walk(statement)
            )
            if has_named_drain or (has_select and has_read):
                drained.add(selector)
        return drained

    def _thread_streams(self, node: ast.AST) -> set[tuple[str, str]]:
        if not isinstance(node, ast.Call):
            return set()
        if canonical_name(node.func, self.aliases) != "threading.Thread":
            return set()
        return self._stream_refs(node)

    def _record_process(self, target: ast.AST, value: ast.AST | None) -> bool:
        binding = _binding_name(target)
        if not binding or not isinstance(value, ast.Call):
            return False
        if canonical_name(value.func, self.aliases) != "subprocess.Popen":
            return False
        pipes = {
            keyword.arg
            for keyword in value.keywords
            if keyword.arg in {"stdout", "stderr"}
            and canonical_name(keyword.value, self.aliases) == "subprocess.PIPE"
        }
        if not pipes:
            return False
        self.processes[binding] = {
            "pipes": pipes,
            "drained": set(),
            "selectors": {},
            "reported": False,
        }
        return True

    def _record_assignment(self, targets: list[ast.AST], value: ast.AST | None) -> None:
        thread_streams = self._thread_streams(value) if value is not None else set()
        for target in targets:
            binding = _binding_name(target)
            if self._record_process(target, value):
                continue
            if binding:
                self.processes.pop(binding, None)
                if thread_streams:
                    self.readers[binding] = thread_streams
                else:
                    self.readers.pop(binding, None)
            _bind(target, value, self.aliases)

    def visit_Import(self, node: ast.Import) -> None:  # noqa: N802
        for item in node.names:
            bound = item.asname or item.name.split(".")[0]
            self.aliases[bound] = item.name if item.asname else item.name.split(".")[0]

    def visit_ImportFrom(self, node: ast.ImportFrom) -> None:  # noqa: N802
        if node.module:
            for item in node.names:
                self.aliases[item.asname or item.name] = f"{node.module}.{item.name}"

    def visit_Assign(self, node: ast.Assign) -> None:  # noqa: N802
        self._record_assignment(list(node.targets), node.value)
        self.visit(node.value)

    def visit_AnnAssign(self, node: ast.AnnAssign) -> None:  # noqa: N802
        self._record_assignment([node.target], node.value)
        if node.value is not None:
            self.visit(node.value)

    def visit_NamedExpr(self, node: ast.NamedExpr) -> None:  # noqa: N802
        self._record_assignment([node.target], node.value)
        self.visit(node.value)

    def visit_AugAssign(self, node: ast.AugAssign) -> None:  # noqa: N802
        binding = _binding_name(node.target)
        self.processes.pop(binding, None)
        self.readers.pop(binding, None)
        self.visit(node.value)

    def visit_For(self, node: ast.For) -> None:  # noqa: N802
        self._drain(self._stream_refs(node.iter))
        self.visit(node.iter)
        before = self._fork()
        body = self._visit_branch(node.body)
        self._merge_branches([before, body])
        if node.orelse:
            before_else = self._fork()
            after_else = self._visit_branch(node.orelse)
            self._merge_branches([before_else, after_else])

    visit_AsyncFor = visit_For

    def visit_If(self, node: ast.If) -> None:  # noqa: N802
        self.visit(node.test)
        body = self._visit_branch(node.body)
        otherwise = self._visit_branch(node.orelse)
        self._merge_branches([body, otherwise])

    def visit_While(self, node: ast.While) -> None:  # noqa: N802
        for selector in self._selector_drain_loops(node):
            self._drain_selector(selector)
        self.visit(node.test)
        before = self._fork()
        body = self._visit_branch(node.body)
        self._merge_branches([before, body])
        if node.orelse:
            before_else = self._fork()
            after_else = self._visit_branch(node.orelse)
            self._merge_branches([before_else, after_else])

    def visit_With(self, node: ast.With) -> None:  # noqa: N802
        for item in node.items:
            if item.optional_vars is not None:
                self._record_assignment([item.optional_vars], item.context_expr)
            self.visit(item.context_expr)
        for statement in node.body:
            self.visit(statement)

    visit_AsyncWith = visit_With

    def visit_Call(self, node: ast.Call) -> None:  # noqa: N802
        name = dotted(node.func)
        for binding, state in self.processes.items():
            if name == f"{binding}.communicate":
                pipes = state["pipes"]
                assert isinstance(pipes, set)
                self._drain({(binding, stream) for stream in pipes})
            elif name in {
                f"{binding}.stdout.read",
                f"{binding}.stdout.read1",
                f"{binding}.stdout.readinto",
                f"{binding}.stdout.readline",
                f"{binding}.stdout.readlines",
                f"{binding}.stderr.read",
                f"{binding}.stderr.read1",
                f"{binding}.stderr.readinto",
                f"{binding}.stderr.readline",
                f"{binding}.stderr.readlines",
            }:
                parts = name.split(".")
                self._drain({(binding, parts[-2])})

        if isinstance(node.func, ast.Attribute) and node.func.attr == "register" and node.args:
            selector = dotted(node.func.value)
            for binding, stream in self._stream_refs(node.args[0]):
                state = self.processes[binding]
                selectors = state["selectors"]
                assert isinstance(selectors, dict)
                registered = selectors.setdefault(selector, set())
                registered.add(stream)

        if isinstance(node.func, ast.Attribute) and node.func.attr == "start":
            streams = self.readers.get(dotted(node.func.value), set())
            if isinstance(node.func.value, ast.Call):
                streams = streams | self._thread_streams(node.func.value)
            self._drain(streams)

        for binding, state in self.processes.items():
            if name != f"{binding}.wait" or state["reported"]:
                continue
            pipes = state["pipes"]
            drained = state["drained"]
            assert isinstance(pipes, set) and isinstance(drained, set)
            if not pipes <= drained:
                self.findings.append({
                    "path": str(self.path),
                    "line": node.lineno,
                    "rule": "subprocess-pipe-wait-before-drain",
                    "detail": f"{binding}.wait",
                })
                state["reported"] = True
        self.generic_visit(node)

    def visit_FunctionDef(self, node: ast.FunctionDef) -> None:  # noqa: N802
        return

    visit_AsyncFunctionDef = visit_FunctionDef

    def visit_ClassDef(self, node: ast.ClassDef) -> None:  # noqa: N802
        return

    def visit_Lambda(self, node: ast.Lambda) -> None:  # noqa: N802
        return


def _check_pipe_wait(
    node: ast.FunctionDef | ast.AsyncFunctionDef,
    aliases: dict[str, str],
    path: Path,
    findings: list[dict[str, object]],
) -> None:
    visitor = _PipeWaitVisitor(aliases, path, findings)
    for statement in node.body:
        visitor.visit(statement)


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
            _check_pipe_wait(node, local, path, findings)
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
