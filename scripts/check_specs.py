"""Check the small, durable invariants of current specifications."""

from __future__ import annotations

import re
import sys
from pathlib import Path
from urllib.parse import unquote


MAX_LINES = 999
MAX_BYTES = 100 * 1024
HEADING_RE = re.compile(r"^#{1,6}\s+(.+?)\s*#*\s*$", re.MULTILINE)
LINK_RE = re.compile(r"!?\[[^\]]*\]\(([^)]+)\)")
DEFINITION_RE = re.compile(
    r"(?m)^(?:\s*[-*]\s+(?:\*\*)?(?P<bullet>[A-Z][A-Z0-9]*-[0-9]+[A-Z]?)"
    r"(?=\s|:|\*|\(|$)|\s*#{1,6}\s+(?P<heading>[A-Z][A-Z0-9]*-[0-9]+[A-Z]?)"
    r"(?:\s|:|\(|$))"
)
POSITIONAL_RE = re.compile(r"^(?:[0-9]+[-_]|.*-continued-[0-9]+(?:-|\.|$))")


class SpecCheckError(ValueError):
    def __init__(self, code: str, path: Path, detail: str) -> None:
        super().__init__(code, str(path), detail)
        self.code = code
        self.path = path
        self.detail = detail

    def __str__(self) -> str:
        return f"{self.code}: {self.path}: {self.detail}"


def _fail(code: str, path: Path, detail: str) -> None:
    raise SpecCheckError(code, path, detail)


def _read(path: Path, *, capped: bool = False) -> str:
    if path.is_symlink() or not path.is_file():
        _fail("unsafe-path", path, "expected a regular, non-symlink file")
    raw = path.read_bytes()
    if capped and len(raw) > MAX_BYTES:
        _fail("over-cap", path, f"{len(raw)} bytes exceeds {MAX_BYTES}")
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        _fail("invalid-utf8", path, str(exc))
    if capped and len(text.splitlines()) > MAX_LINES:
        _fail("over-cap", path, f"more than {MAX_LINES} lines")
    return text


def _without_fences(text: str) -> str:
    output: list[str] = []
    opened: tuple[str, int] | None = None
    for line in text.splitlines(keepends=True):
        marker = re.match(r"^\s*([`~]{3,})", line)
        if opened is None and marker:
            opened = (marker.group(1)[0], len(marker.group(1)))
        elif opened is not None and marker:
            token = marker.group(1)
            if token[0] == opened[0] and len(token) >= opened[1]:
                opened = None
        else:
            output.append(line if opened is None else ("\n" if line.endswith("\n") else ""))
        if marker:
            output.append("\n" if line.endswith("\n") else "")
    return "".join(output)


def _slug(value: str) -> str:
    value = re.sub(r"[^\w\- ]", "", value.strip().lower())
    return re.sub(r"[\s-]+", "-", value).strip("-")


def _fragments(path: Path) -> set[str]:
    seen: dict[str, int] = {}
    result: set[str] = set()
    for heading in HEADING_RE.findall(_without_fences(_read(path))):
        base = _slug(heading)
        ordinal = seen.get(base, 0)
        seen[base] = ordinal + 1
        result.add(base if ordinal == 0 else f"{base}-{ordinal}")
    return result


def _check_links(repo_root: Path, source: Path, text: str) -> None:
    for raw in LINK_RE.findall(_without_fences(text)):
        target = raw.strip().split(maxsplit=1)[0].strip("<>")
        if not target or target.startswith(("http://", "https://", "mailto:")):
            continue
        path_part, marker, fragment = target.partition("#")
        destination = source if not path_part else source.parent / unquote(path_part)
        try:
            destination.resolve().relative_to(repo_root)
        except ValueError:
            _fail("broken-link", source, f"link escapes repository: {target}")
        if not destination.exists() or destination.is_symlink() or not destination.is_file():
            _fail("broken-link", source, f"missing regular target: {target}")
        if marker and fragment:
            if destination.suffix.lower() != ".md" or unquote(fragment).lower() not in _fragments(destination):
                _fail("broken-link", source, f"missing fragment: {target}")


def current_spec_paths(repo_root: Path) -> tuple[Path, ...]:
    specs = repo_root / "docs" / "specs"
    index = specs / "README.md"
    if not index.is_file():
        _fail("missing-specs", specs, "README and standalone specifications are required")
    supported: list[Path] = []
    for path in sorted(specs.rglob("*.md")):
        if path == index:
            continue
        relative = path.relative_to(specs)
        if len(relative.parts) != 2:
            _fail(
                "unsupported-path",
                path,
                "spec Markdown must be README.md or <domain>/<topic>.md",
            )
        supported.append(path)
    if not supported:
        _fail("missing-specs", specs, "README and standalone specifications are required")
    return (index, *supported)


def _check_index(repo_root: Path, index: Path, text: str, specs: tuple[Path, ...]) -> None:
    spec_root = (repo_root / "docs" / "specs").resolve()
    indexed: set[Path] = set()
    for raw in LINK_RE.findall(_without_fences(text)):
        target = raw.strip().split(maxsplit=1)[0].strip("<>")
        if not target or target.startswith(("http://", "https://", "mailto:")):
            continue
        path_part = target.partition("#")[0]
        if not path_part:
            continue
        destination = (index.parent / unquote(path_part)).resolve()
        try:
            destination.relative_to(spec_root)
        except ValueError:
            continue
        if destination != index.resolve() and destination.suffix.lower() == ".md":
            indexed.add(destination)

    discovered = {path.resolve() for path in specs}
    if indexed != discovered:
        display = lambda paths: sorted(str(path.relative_to(spec_root)) for path in paths)
        _fail(
            "index-drift",
            index,
            f"missing={display(discovered - indexed)} unexpected={display(indexed - discovered)}",
        )


def check_repository(repo_root: Path) -> tuple[Path, ...]:
    repo_root = repo_root.resolve()
    paths = current_spec_paths(repo_root)
    definitions: dict[str, Path] = {}
    for path in paths:
        relative = path.relative_to(repo_root / "docs" / "specs")
        for part in relative.parts:
            if POSITIONAL_RE.match(part):
                _fail("positional-name", path, f"rejected path component: {part}")
        text = _read(path, capped=path.name != "README.md")
        _check_links(repo_root, path, text)
        for match in DEFINITION_RE.finditer(_without_fences(text)):
            identifier = match.group("bullet") or match.group("heading")
            if identifier in definitions:
                _fail("duplicate-id", path, f"{identifier} also defined in {definitions[identifier]}")
            definitions[identifier] = path
    _check_index(repo_root, paths[0], _read(paths[0]), paths[1:])
    return paths


def main() -> int:
    try:
        paths = check_repository(Path.cwd())
    except SpecCheckError as exc:
        print(exc, file=sys.stderr)
        return 1
    print(f"spec-check: ok ({len(paths) - 1} standalone specs)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
