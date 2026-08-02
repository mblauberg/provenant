"""Every repository path cited in the given documents must exist.

The check is deliberately narrow so that it stays honest and low-noise:

- Markdown link targets are checked whenever they are relative (no scheme, no
  anchor-only reference), resolved against the document's own directory first
  and the repository root second.
- Inline-code citations are checked only when their first whitespace-delimited
  token starts with a committed top-level tree (``skills/``, ``scripts/``,
  ``config/``, ``docs/``, ``tests/``, ``runtime/``, ``workflows/``), so command
  lines with flags still resolve to the script they cite.
- ``:LINE`` and ``:LINE-LINE`` suffixes are stripped before resolution, and a
  citation containing a glob passes when it matches at least one path.
- Fenced blocks are skipped (sample layouts and transcripts are not claims),
  and so is anything carrying a placeholder or instance-only shape
  (``~/...``, ``$VAR``, ``<name>``, URLs), because those never name a
  repository path this tree could prove.
"""

from __future__ import annotations

import argparse
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]
REPO_TREES = ("skills/", "scripts/", "config/", "docs/", "tests/", "runtime/", "workflows/")
FENCE = re.compile(r"^\s*(```|~~~)")
LINK_TARGET = re.compile(r"\]\(([^)\s]+)\)")
INLINE_CODE = re.compile(r"`([^`\n]+)`")
LINE_SUFFIX = re.compile(r"(?::\d+(?:-\d+)?)+$")
SKIP_MARKS = ("~", "$", "<", ">", "://")


def candidates(line: str, doc_dir: Path, root: Path):
    """Yield (cited, bases) pairs found on one line of prose."""
    for target in LINK_TARGET.findall(line):
        if target.startswith("#") or any(mark in target for mark in SKIP_MARKS):
            continue
        yield target, (doc_dir, root)
    for code in INLINE_CODE.findall(line):
        token = code.split()[0] if code.split() else ""
        if any(mark in token for mark in SKIP_MARKS):
            continue
        if token.startswith(REPO_TREES):
            yield token, (root, doc_dir)


def exists(cited: str, bases: tuple[Path, ...]) -> bool:
    plain = LINE_SUFFIX.sub("", cited.split("#")[0]).rstrip("/")
    if not plain:
        return True
    for base in bases:
        if any(char in plain for char in "*?["):
            if next(iter(base.glob(plain)), None) is not None:
                return True
        elif (base / plain).exists():
            return True
    return False


def check_document(doc: Path, root: Path) -> tuple[list[str], int]:
    errors: list[str] = []
    checked = 0
    fenced = False
    display = doc.relative_to(root) if doc.is_relative_to(root) else doc.name
    for number, line in enumerate(doc.read_text().splitlines(), start=1):
        if FENCE.match(line):
            fenced = not fenced
            continue
        if fenced:
            continue
        for cited, bases in candidates(line, doc.parent, root):
            checked += 1
            if not exists(cited, bases):
                errors.append(f"{display}:{number}: cited path does not exist: {cited}")
    return errors, checked


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("documents", nargs="*", type=Path,
                        default=[ROOT / "docs" / "ARCHITECTURE.md", ROOT / "README.md"])
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args(argv)
    errors: list[str] = []
    checked = 0
    for doc in args.documents:
        doc_errors, doc_checked = check_document(doc.resolve(), args.root.resolve())
        errors.extend(doc_errors)
        checked += doc_checked
    if errors:
        for error in errors:
            print(f"FAIL: {error}", file=sys.stderr)
        return 1
    print(f"PASS: {checked} cited paths exist across {len(args.documents)} documents")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
