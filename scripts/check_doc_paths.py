"""Every cited repository path must exist; documented Python commands must be invocable.

The check is deliberately narrow so that it stays honest and low-noise:

- Markdown link targets are checked whenever they are relative (no scheme, no
  anchor-only reference), resolved against the document's own directory first
  and the repository root second.
- Inline-code citations are checked whenever a token starts with a committed
  top-level tree (``skills/``, ``scripts/``, ``config/``, ``docs/``, ``tests/``,
  ``runtime/``, ``workflows/``), so an interpreter prefix does not hide the
  script a command cites.
- ``:LINE`` and ``:LINE-LINE`` suffixes are stripped before resolution, and a
  citation containing a glob passes when it matches at least one path.
- Runnable shell fences (``sh``, ``bash``, ``shell``, ``zsh``, ``ksh``, ``csh``
  and ``fish``) are checked; other fenced blocks, including ``text``
  transcripts, are skipped. A direct command for ``scripts/**/*.py`` is
  rejected because those scripts deliberately have no shebang or executable
  bit; use an interpreter or one of ``run_stdlib``, ``run_yaml`` or ``run_test``.
- Anything carrying a placeholder or instance-only shape
  (``~/...``, ``$VAR``, ``<name>``, URLs), because those never name a
  repository path this tree could prove.

This check does not execute commands or validate their arguments, dependencies,
shell semantics or placeholder substitution.
"""

from __future__ import annotations

import argparse
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]
FENCE = re.compile(r"^\s*(```|~~~)(.*)$")
LINK_TARGET = re.compile(r"\]\(([^)\s]+)\)")
INLINE_CODE = re.compile(r"`([^`\n]+)`")
REPO_PATH_TOKEN = re.compile(
    r"(?<![A-Za-z0-9_./-])"
    r"(?P<path>(?:skills|scripts|config|docs|tests|runtime|workflows)/[^\s`'\"<>()]+)"
)
LINE_SUFFIX = re.compile(r"(?::\d+(?:-\d+)?)+$")
SKIP_MARKS = ("~", "$", "<", ">", "://")
PYTHON_INVOKER = re.compile(r"\b(?:HARNESS_PYTHON|python(?:3)?|run_(?:stdlib|yaml|test))\b")


def _plain_path(cited: str) -> str:
    plain = cited.split("#")[0].rstrip(".,;")
    return LINE_SUFFIX.sub("", plain).rstrip("/")


def _is_script_python(cited: str) -> bool:
    plain = _plain_path(cited)
    return plain.startswith("scripts/") and plain.endswith(".py")


def _is_command(code: str, start: int, end: int, *, runnable_fence: bool) -> bool:
    if runnable_fence:
        return True
    before = code[:start].strip()
    after = LINE_SUFFIX.sub("", code[end:]).strip().strip(".,;")
    return bool(before or after)


def candidates(line: str, doc_dir: Path, root: Path, *, runnable_fence: bool = False):
    """Yield (cited, bases, is_command, has_interpreter) tuples."""
    for target in LINK_TARGET.findall(line):
        if target.startswith("#") or any(mark in target for mark in SKIP_MARKS):
            continue
        yield target, (doc_dir, root), False, False
    for code in INLINE_CODE.findall(line):
        for match in REPO_PATH_TOKEN.finditer(code):
            token = match.group("path")
            if any(mark in token for mark in SKIP_MARKS):
                continue
            yield (
                token,
                (root, doc_dir),
                _is_command(
                    code, match.start("path"), match.end("path"),
                    runnable_fence=runnable_fence,
                ),
                bool(PYTHON_INVOKER.search(code[:match.start("path")])),
            )
    if runnable_fence and not line.lstrip().startswith("#"):
        for match in REPO_PATH_TOKEN.finditer(line):
            token = match.group("path")
            if any(mark in token for mark in SKIP_MARKS):
                continue
            yield (
                token,
                (root, doc_dir),
                True,
                bool(PYTHON_INVOKER.search(line[:match.start("path")])),
            )


def exists(cited: str, bases: tuple[Path, ...]) -> bool:
    plain = _plain_path(cited)
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
    fenced: str | None = None
    runnable_fence = False
    display = doc.relative_to(root) if doc.is_relative_to(root) else doc.name
    for number, line in enumerate(doc.read_text().splitlines(), start=1):
        fence = FENCE.match(line)
        if fence:
            if fenced is None:
                fenced = fence.group(1)
                # A fence may open with no language at all, so take the first
                # word only if there is one rather than indexing an empty split.
                words = fence.group(2).strip().split(maxsplit=1)
                language = words[0].lower() if words else ""
                runnable_fence = language in {
                    "sh", "bash", "shell", "zsh", "ksh", "csh", "fish"
                }
            else:
                fenced = None
                runnable_fence = False
            continue
        if fenced is not None and not runnable_fence:
            continue
        for cited, bases, is_command, has_interpreter in candidates(
            line, doc.parent, root, runnable_fence=runnable_fence
        ):
            checked += 1
            if not exists(cited, bases):
                errors.append(f"{display}:{number}: cited path does not exist: {cited}")
            elif is_command and _is_script_python(cited) and not has_interpreter:
                errors.append(
                    f"{display}:{number}: Python command must use an interpreter: {cited}"
                )
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
    print(
        f"PASS: {checked} cited paths and commands pass across "
        f"{len(args.documents)} documents"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
