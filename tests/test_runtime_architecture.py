"""Guardrails owned by the #328 architecture guardrail child.

The first invariant ratchets every oversized hand-written runtime TypeScript
source file while holding every new source file to 1,000 lines. The second
keeps the daemonless Fabric package independent of the retired runtime packages,
inside its package boundary, and layered so its store cannot depend on its CLI
or MCP surfaces. Temporary allowances and permanent declared placements stay
live through staleness checks.

Specifier extraction deliberately uses regexes over comment-masked source
rather than a TypeScript AST: the relevant ESM forms have literal string
specifiers, and checker-negative tests keep that simpler tradeoff honest. This
module belongs in the Python harness job because it is the only CI job whose
path filter covers all of ``runtime/**``; package-specific jobs can be skipped
for changes elsewhere in the runtime tree.
"""

from __future__ import annotations

import re
import warnings
from pathlib import Path

import pytest
import yaml


ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "tests" / "fixtures" / "runtime-architecture.yaml"
DEFAULT_SOURCE_SIZE_CAP = 1_000
EXCLUDED_SOURCE_DIRECTORIES = frozenset(
    {"__tests__", "generated", "vendor", "vendored"}
)
RETIRED_FABRIC_PACKAGES = frozenset(
    {
        "agent-fabric",
        "agent-fabric-console",
        "agent-fabric-herdr",
        "agent-fabric-protocol",
        "agent-fabric-review-portal-supervisor",
    }
)
FABRIC_SURFACE_FILES = frozenset({"cli.ts", "server.ts"})
REMOVAL_ISSUE = re.compile(r"^#\d+$")
IMPORT_ALLOWANCE_KEYS = frozenset(
    {"file", "specifier_prefix", "removal_issue", "rationale"}
)
IMPORT_FROM = re.compile(
    r"^[ \t]*(?:import|export)\s+(?![('\"])[^;]*?\bfrom\s*"
    r"(?P<quote>['\"])(?P<specifier>[^'\"]+)(?P=quote)",
    re.MULTILINE,
)
BARE_IMPORT = re.compile(
    r"^[ \t]*import\s*(?P<quote>['\"])(?P<specifier>[^'\"]+)(?P=quote)",
    re.MULTILINE,
)
DYNAMIC_IMPORT = re.compile(
    r"\bimport\s*\(\s*(?P<quote>['\"])(?P<specifier>[^'\"]+)(?P=quote)\s*\)"
)


def production_typescript_files(root: Path) -> list[Path]:
    """Return the hand-written runtime production TypeScript files below root."""

    files: list[Path] = []
    runtime = root / "runtime"
    if not runtime.is_dir():
        return files
    for source_root in sorted(runtime.glob("*/src")):
        if not source_root.is_dir():
            continue
        for source_file in source_root.rglob("*.ts"):
            relative_parts = source_file.relative_to(source_root).parts
            if source_file.name.endswith((".test.ts", ".spec.ts", ".d.ts")):
                continue
            if any(part.casefold() in EXCLUDED_SOURCE_DIRECTORIES for part in relative_parts[:-1]):
                continue
            files.append(source_file)
    return sorted(files)


def _load_fixture(fixture_path: Path = FIXTURE) -> tuple[dict[str, int], list[dict[str, str]]]:
    document = yaml.safe_load(fixture_path.read_text(encoding="utf-8"))
    assert isinstance(document, dict), f"{fixture_path} must contain a mapping"
    ceilings = document.get("source_size_ceilings")
    allowances = document.get("import_allowances")
    assert isinstance(ceilings, dict), "source_size_ceilings must be a mapping"
    assert isinstance(allowances, list), "import_allowances must be a list"
    assert all(isinstance(file, str) and isinstance(limit, int) for file, limit in ceilings.items())
    assert all(isinstance(allowance, dict) for allowance in allowances)
    return ceilings, allowances


def _raw_line_count(source_file: Path) -> int:
    return source_file.read_bytes().count(b"\n")


def _check_source_sizes(root: Path, ceilings: dict[str, int]) -> None:
    production_files = {
        source_file.relative_to(root).as_posix(): source_file
        for source_file in production_typescript_files(root)
    }
    errors: list[str] = []

    for fixture_file, ceiling in sorted(ceilings.items()):
        if ceiling <= DEFAULT_SOURCE_SIZE_CAP:
            errors.append(
                f"invalid source-size ceiling for {fixture_file}: {ceiling} <= "
                f"{DEFAULT_SOURCE_SIZE_CAP}; it belongs to the default cap"
            )
        if fixture_file not in production_files:
            errors.append(f"stale source-size ceiling: {fixture_file} no longer exists")

    for relative_file, source_file in sorted(production_files.items()):
        actual = _raw_line_count(source_file)
        ceiling = ceilings.get(relative_file)
        if ceiling is None:
            if actual > DEFAULT_SOURCE_SIZE_CAP:
                errors.append(
                    f"source-size cap exceeded for {relative_file}: {actual} > "
                    f"{DEFAULT_SOURCE_SIZE_CAP}; split the file or add a reviewed ratchet ceiling"
                )
            continue
        if actual > ceiling:
            errors.append(
                f"source-size ceiling exceeded for {relative_file}: {actual} > {ceiling}; "
                "shrink the file or, with review justification, raise the ceiling in "
                "tests/fixtures/runtime-architecture.yaml"
            )
        elif actual < ceiling:
            warnings.warn(
                f"stale ceiling for {relative_file}: ratchet down to {actual}",
                stacklevel=2,
            )

    assert not errors, "\n".join(errors)


def _mask_comments(source: str) -> str:
    """Replace comment bytes with spaces while preserving strings and line numbers."""

    output = list(source)
    index = 0
    quote: str | None = None
    while index < len(source):
        character = source[index]
        following = source[index + 1] if index + 1 < len(source) else ""
        if quote is not None:
            if character == "\\":
                index += 2
                continue
            if character == quote:
                quote = None
            index += 1
            continue
        if character in {"'", '"', "`"}:
            quote = character
            index += 1
            continue
        if character == "/" and following == "/":
            while index < len(source) and source[index] != "\n":
                output[index] = " "
                index += 1
            continue
        if character == "/" and following == "*":
            output[index] = " "
            output[index + 1] = " "
            index += 2
            while index < len(source):
                if source[index] == "*" and index + 1 < len(source) and source[index + 1] == "/":
                    output[index] = " "
                    output[index + 1] = " "
                    index += 2
                    break
                if source[index] != "\n":
                    output[index] = " "
                index += 1
            continue
        index += 1
    return "".join(output)


def _import_specifiers(source: str) -> list[tuple[int, str]]:
    masked = _mask_comments(source)
    matches = [
        match
        for pattern in (IMPORT_FROM, BARE_IMPORT, DYNAMIC_IMPORT)
        for match in pattern.finditer(masked)
    ]
    return sorted(
        (masked.count("\n", 0, match.start()) + 1, match.group("specifier"))
        for match in matches
    )


def _matches_package(specifier: str, package: str) -> bool:
    return any(
        specifier == candidate or specifier.startswith(f"{candidate}/")
        for candidate in (package, f"@local/{package}")
    )


def _resolved_relative_import(importer: Path, specifier: str) -> Path | None:
    if not specifier.startswith("."):
        return None
    clean_specifier = specifier.split("?", 1)[0].split("#", 1)[0]
    resolved = importer.parent / clean_specifier
    if resolved.suffix == ".js":
        resolved = resolved.with_suffix(".ts")
    return resolved.resolve()


def _is_below(candidate: Path | None, parent: Path) -> bool:
    if candidate is None:
        return False
    try:
        candidate.relative_to(parent.resolve())
    except ValueError:
        return False
    return True


def _violated_rules(root: Path, importer: Path, specifier: str) -> list[str]:
    root = root.resolve()
    importer = importer.resolve()
    resolved = _resolved_relative_import(importer, specifier)
    runtime = root / "runtime"
    fabric_package = runtime / "fabric"
    fabric_source = fabric_package / "src"
    rules: list[str] = []

    if _is_below(importer, fabric_source):
        if any(
            _matches_package(specifier, package)
            for package in RETIRED_FABRIC_PACKAGES
        ):
            rules.append("R1")
        if _is_below(resolved, runtime) and not _is_below(resolved, fabric_package):
            rules.append("R2")
        if (
            importer.name in {"identity.ts", "store.ts"}
            and resolved is not None
            and resolved.parent == fabric_source.resolve()
            and resolved.name in FABRIC_SURFACE_FILES
        ):
            rules.append("R3")

    return rules


def _import_violations(root: Path) -> list[dict[str, object]]:
    violations: list[dict[str, object]] = []
    for source_file in production_typescript_files(root):
        relative_file = source_file.relative_to(root).as_posix()
        source = source_file.read_text(encoding="utf-8")
        for line, specifier in _import_specifiers(source):
            for rule_id in _violated_rules(root, source_file, specifier):
                violations.append(
                    {
                        "file": relative_file,
                        "line": line,
                        "specifier": specifier,
                        "rule_id": rule_id,
                    }
                )
    return violations


def _check_import_boundaries(root: Path, allowances: list[dict[str, str]]) -> None:
    errors: list[str] = []
    used_allowances: set[int] = set()
    for index, allowance in enumerate(allowances):
        unknown_keys = sorted(set(allowance) - IMPORT_ALLOWANCE_KEYS)
        if unknown_keys:
            errors.append(
                f"invalid import allowance at index {index}: unknown keys {unknown_keys}"
            )
        required = ("file", "specifier_prefix")
        if not all(isinstance(allowance.get(field), str) for field in required):
            errors.append(f"invalid import allowance at index {index}: {required} are required strings")
            continue
        declared_fields = {field for field in ("removal_issue", "rationale") if field in allowance}
        if len(declared_fields) != 1:
            errors.append(
                f"invalid import allowance at index {index}: exactly one of "
                "removal_issue or rationale is required"
            )
            continue
        declared_field = next(iter(declared_fields))
        declared_value = allowance[declared_field]
        if not isinstance(declared_value, str) or not declared_value.strip():
            errors.append(
                f"invalid {declared_field} for import allowance at index {index}: "
                "a non-empty string is required"
            )
        elif declared_field == "removal_issue" and REMOVAL_ISSUE.fullmatch(declared_value) is None:
            errors.append(
                f"invalid removal_issue for import allowance at index {index}: "
                f"{declared_value!r} must match ^#\\d+$"
            )

    for violation in _import_violations(root):
        matching_allowance = next(
            (
                index
                for index, allowance in enumerate(allowances)
                if allowance.get("file") == violation["file"]
                and isinstance(allowance.get("specifier_prefix"), str)
                and str(violation["specifier"]).startswith(allowance["specifier_prefix"])
            ),
            None,
        )
        if matching_allowance is None:
            errors.append(
                f"{violation['rule_id']} import-boundary violation: {violation['file']}:"
                f"{violation['line']} imports {violation['specifier']!r}"
            )
        else:
            used_allowances.add(matching_allowance)

    for index, allowance in enumerate(allowances):
        if index not in used_allowances:
            errors.append(
                "stale import allowance: "
                f"{allowance.get('file', '<missing file>')} -> "
                f"{allowance.get('specifier_prefix', '<missing prefix>')}"
            )

    assert not errors, "\n".join(errors)


def _write_source(root: Path, relative_file: str, source: str) -> Path:
    source_file = root / relative_file
    source_file.parent.mkdir(parents=True, exist_ok=True)
    source_file.write_text(source, encoding="utf-8")
    return source_file


def test_runtime_source_size_ratchet() -> None:
    ceilings, _ = _load_fixture()
    _check_source_sizes(ROOT, ceilings)


def test_runtime_import_boundaries() -> None:
    _, allowances = _load_fixture()
    _check_import_boundaries(ROOT, allowances)


def test_production_walker_honours_exclusions(tmp_path: Path) -> None:
    included = _write_source(tmp_path, "runtime/package/src/kept.ts", "export {};\n")
    excluded = (
        "runtime/package/src/unit.test.ts",
        "runtime/package/src/unit.spec.ts",
        "runtime/package/src/types.d.ts",
        "runtime/package/src/__tests__/helper.ts",
        "runtime/package/src/generated/schema.ts",
        "runtime/package/src/vendor/library.ts",
        "runtime/package/src/vendored/library.ts",
    )
    for relative_file in excluded:
        _write_source(tmp_path, relative_file, "export {};\n")

    assert production_typescript_files(tmp_path) == [included]


def test_source_size_checker_rejects_new_and_grown_files_but_warns_on_shrink(
    tmp_path: Path,
) -> None:
    new_file = "runtime/package/src/new.ts"
    _write_source(tmp_path, new_file, "x\n" * 1_001)
    with pytest.raises(AssertionError, match="source-size cap exceeded"):
        _check_source_sizes(tmp_path, {})

    ceiling_file = "runtime/package/src/legacy.ts"
    (tmp_path / new_file).unlink()
    _write_source(tmp_path, ceiling_file, "x\n" * 1_002)
    with pytest.raises(AssertionError, match="source-size ceiling exceeded"):
        _check_source_sizes(tmp_path, {ceiling_file: 1_001})

    _write_source(tmp_path, ceiling_file, "x\n" * 999)
    with pytest.warns(UserWarning, match=f"stale ceiling for {re.escape(ceiling_file)}"):
        _check_source_sizes(tmp_path, {ceiling_file: 1_001})


@pytest.mark.parametrize(
    ("relative_file", "source", "rule_id"),
    (
        (
            "runtime/fabric/src/store.ts",
            'import { x } from "@local/agent-fabric";\n',
            "R1",
        ),
        (
            "runtime/fabric/src/store.ts",
            'import { x } from "../../other-runtime/src/index.js";\n',
            "R2",
        ),
        (
            "runtime/fabric/src/store.ts",
            'import { x } from "./server.js";\n',
            "R3",
        ),
    ),
)
def test_import_checker_detects_each_rule(
    tmp_path: Path, relative_file: str, source: str, rule_id: str
) -> None:
    _write_source(tmp_path, relative_file, source)

    with pytest.raises(AssertionError, match=rf"{rule_id} import-boundary violation"):
        _check_import_boundaries(tmp_path, [])


def test_import_checker_detects_dynamic_import(tmp_path: Path) -> None:
    _write_source(
        tmp_path,
        "runtime/fabric/src/store.ts",
        'const server = import("./server.js");\n',
    )

    with pytest.raises(AssertionError, match=r"R3 import-boundary violation"):
        _check_import_boundaries(tmp_path, [])


def test_import_checker_rejects_stale_allowance(tmp_path: Path) -> None:
    _write_source(tmp_path, "runtime/fabric/src/store.ts", "export {};\n")
    allowance = {
        "file": "runtime/fabric/src/store.ts",
        "specifier_prefix": "./server.js",
        "removal_issue": "#344",
    }

    with pytest.raises(AssertionError, match="stale import allowance"):
        _check_import_boundaries(tmp_path, [allowance])


def test_import_checker_accepts_permanent_allowance(tmp_path: Path) -> None:
    _write_source(
        tmp_path,
        "runtime/fabric/src/store.ts",
        'import { x } from "./server.js";\n',
    )
    allowance = {
        "file": "runtime/fabric/src/store.ts",
        "specifier_prefix": "./server.js",
        "rationale": "permanent composition placement",
    }

    _check_import_boundaries(tmp_path, [allowance])


def test_import_checker_rejects_allowance_missing_both_fields(tmp_path: Path) -> None:
    _write_source(tmp_path, "runtime/fabric/src/store.ts", "export {};\n")
    allowance = {
        "file": "runtime/fabric/src/store.ts",
        "specifier_prefix": "./server.js",
    }

    with pytest.raises(AssertionError, match="exactly one of removal_issue or rationale"):
        _check_import_boundaries(tmp_path, [allowance])


def test_import_checker_rejects_empty_rationale(tmp_path: Path) -> None:
    _write_source(tmp_path, "runtime/fabric/src/store.ts", "export {};\n")
    allowance = {
        "file": "runtime/fabric/src/store.ts",
        "specifier_prefix": "./server.js",
        "rationale": " ",
    }

    with pytest.raises(AssertionError, match="invalid rationale"):
        _check_import_boundaries(tmp_path, [allowance])


def test_import_checker_ignores_comment_only_daemon_mention(tmp_path: Path) -> None:
    _write_source(
        tmp_path,
        "runtime/fabric/src/store.ts",
        '// import { x } from "./server.js";\n'
        '/* export { x } from "./server.js"; */\n',
    )

    _check_import_boundaries(tmp_path, [])
