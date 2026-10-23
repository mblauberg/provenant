"""Documented Fabric calls must fit the registered server schemas."""

from __future__ import annotations

import json
from pathlib import Path
import re
import subprocess


ROOT = Path(__file__).resolve().parents[1]
CALL = re.compile(r"fabric_([a-z_]+)\s*([({])([^})\n]*)[})]")
FIELD = re.compile(r"\b([a-z_][a-z_0-9]*)\s*:")
TOOL = re.compile(r"\bfabric_[a-z_]+\b")
BACKTICK_FIELD = re.compile(r"`([a-z_][a-z_0-9]*)(?:\s*:[^`]*)?`")


schema_export = subprocess.run(
    ["node", "--import", "tsx", "src/export-tool-schemas.ts"],
    cwd=ROOT / "runtime/fabric",
    check=True,
    capture_output=True,
    text=True,
)
SCHEMA = {tool: set(fields) for tool, fields in json.loads(schema_export.stdout).items()}


def call_fits_schema(tool: str, fields: list[str]) -> bool:
    return tool in SCHEMA and not (set(fields) - SCHEMA[tool])


def documentation_paths():
    for directory in ("skills", "docs"):
        yield from (ROOT / directory).rglob("*.md")


def documented_calls(text: str):
    for match in CALL.finditer(text):
        tool = "fabric_" + match.group(1)
        body = match.group(3)
        if match.group(2) == "{":
            fields = FIELD.findall(body)
        else:
            fields = [value.strip() for value in body.split(",") if re.fullmatch(r"[a-z_][a-z_0-9]*", value.strip())]
        yield tool, fields

    # Skills use prose more often than call syntax. A `with` clause directly
    # after a tool name names its parameters, including `field: value` forms.
    for line in text.splitlines():
        matches = list(TOOL.finditer(line))
        for index, match in enumerate(matches):
            end = matches[index + 1].start() if index + 1 < len(matches) else len(line)
            clause = line[match.end():end].split(".", 1)[0]
            fields = BACKTICK_FIELD.findall(clause)
            if re.search(r"\bwith\b", clause) and fields:
                yield match.group(), fields


def test_documented_fabric_call_parameters_fit_server_schema():
    violations = []
    covered = set()
    for path in documentation_paths():
        for tool, fields in documented_calls(path.read_text()):
            if fields:
                covered.add(tool)
            invalid = set(fields) - set(SCHEMA.get(tool, []))
            if not call_fits_schema(tool, fields):
                violations.append((str(path.relative_to(ROOT)), tool, sorted(invalid)))
    assert not violations, violations
    assert {"fabric_dispatch", "fabric_status", "fabric_cancel", "fabric_output"} <= covered
    assert {"skills", "docs"} <= {path.relative_to(ROOT).parts[0] for path in documentation_paths()}


def test_checker_rejects_unknown_field_and_removed_tool():
    for snippet in ("fabric_dispatch{imaginary: true}", "fabric_retired_tool()"):
        tool, fields = next(documented_calls(snippet))
        assert not call_fits_schema(tool, fields), snippet
