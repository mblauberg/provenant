"""Skill call examples must stay within the Fabric v2 tool contract.

Lane D checks against the reviewed design fixture. After lane B merges, the chair
must repoint this at the registered zod schemas in runtime/fabric/src/server.ts.
"""

from __future__ import annotations

import json
from pathlib import Path
import re


ROOT = Path(__file__).resolve().parents[1]
SCHEMA = json.loads((ROOT / "tests/fixtures/fabric-v1/lane-d-tool-params.json").read_text())["tools"]
CALL = re.compile(r"fabric_([a-z_]+)\s*([({])([^})\n]*)[})]")
FIELD = re.compile(r"\b([a-z_][a-z_0-9]*)\s*:")
TOOL = re.compile(r"\bfabric_[a-z_]+\b")
BACKTICK_FIELD = re.compile(r"`([a-z_][a-z_0-9]*)(?:\s*:[^`]*)?`")


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
            if re.search(r"\bwith\b", clause):
                yield match.group(), BACKTICK_FIELD.findall(clause)


def test_skill_fabric_call_parameters_fit_design_schema():
    violations = []
    covered = set()
    for path in (ROOT / "skills").rglob("*.md"):
        for tool, fields in documented_calls(path.read_text()):
            if fields:
                covered.add(tool)
            invalid = set(fields) - set(SCHEMA.get(tool, []))
            if tool not in SCHEMA or invalid:
                violations.append((str(path.relative_to(ROOT)), tool, sorted(invalid)))
    assert not violations, violations
    assert {"fabric_dispatch", "fabric_status", "fabric_cancel", "fabric_output"} <= covered


def test_checker_rejects_unknown_field_and_removed_tool():
    assert list(documented_calls("fabric_dispatch{imaginary: true}")) == [("fabric_dispatch", ["imaginary"])]
    assert "imaginary" not in SCHEMA["fabric_dispatch"]
    assert "fabric_batch" not in SCHEMA
