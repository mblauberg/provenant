#!/usr/bin/env python3
"""Check that a move-only refactor moved its top-level definitions unchanged.

Usage: movecheck.py BASE_REF NEW_FILE OLD_FILE [OLD_FILE...]

Each top-level function, class and assignment in NEW_FILE is compared by AST
with the definition of the same name in the OLD_FILEs as they stood at
BASE_REF, so formatting and comments are ignored. A name defined in several
OLD_FILEs matches if it equals any of them. The report lists changed
definitions, new names, and moved names still defined in an OLD_FILE at the
working tree. The exit status is 1 when any moved definition changed.
"""

from __future__ import annotations

import ast
import subprocess
import sys
from pathlib import Path

DEFINITIONS = (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)


def definitions(source: str) -> dict[str, str]:
    return {node.name: ast.dump(node) for node in ast.parse(source).body if isinstance(node, DEFINITIONS)}


def assignments(source: str) -> dict[str, str]:
    found = {}
    for node in ast.parse(source).body:
        if isinstance(node, ast.Assign):
            targets = node.targets
        elif isinstance(node, ast.AnnAssign):
            targets = [node.target]
        else:
            continue
        for target in targets:
            if isinstance(target, ast.Name):
                found[target.id] = ast.dump(node.value) if node.value else ""
    return found


def compare(old_sources: list[str], new_source: str, current: dict[str, str] | None = None) -> dict:
    """Compare NEW against the OLD sources; CURRENT maps old paths to their present text."""
    old_defs, old_assigns = {}, {}
    for source in old_sources:
        for name, dump in definitions(source).items():
            old_defs.setdefault(name, set()).add(dump)
        for name, dump in assignments(source).items():
            old_assigns.setdefault(name, set()).add(dump)
    still = {name: path for path, source in (current or {}).items() for name in definitions(source)}
    report = {"identical": [], "changed": [], "new": [], "duplicated": []}
    for name, dump in definitions(new_source).items():
        if name not in old_defs:
            report["new"].append(name)
        elif dump not in old_defs[name]:
            report["changed"].append(name)
        else:
            report["identical"].append(name)
        if name in still:
            report["duplicated"].append((name, still[name]))
    for name, dump in assignments(new_source).items():
        if name not in old_assigns:
            report["new"].append(name)
        elif dump not in old_assigns[name]:
            report["changed"].append(name)
    return report


def git_show(ref: str, path: str) -> str:
    return subprocess.run(["git", "show", f"{ref}:{path}"], capture_output=True, text=True, check=True).stdout


def main(argv: list[str]) -> int:
    if len(argv) < 3:
        print(__doc__.strip().splitlines()[2], file=sys.stderr)
        return 2
    base, new, olds = argv[0], argv[1], argv[2:]
    current = {path: Path(path).read_text(encoding="utf-8") for path in olds if Path(path).exists()}
    report = compare([git_show(base, path) for path in olds], Path(new).read_text(encoding="utf-8"), current)
    for name in report["changed"]:
        print(f"CHANGED {name}")
    for name in report["new"]:
        print(f"NEW     {name}")
    for name, path in report["duplicated"]:
        print(f"STILL   {name} in {path}")
    print(f"{len(report['identical'])} identical, {len(report['changed'])} changed, {len(report['new'])} new in {new}")
    return 1 if report["changed"] else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
