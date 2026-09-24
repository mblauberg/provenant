"""Documented Fabric calls must fit the registered server schemas."""

from __future__ import annotations

from pathlib import Path
import re


ROOT = Path(__file__).resolve().parents[1]
SERVER = ROOT / "runtime/fabric/src/server.ts"
REGISTER = re.compile(r'register\(\s*"(fabric_[a-z_]+)"\s*,\s*"[^"]*"\s*,\s*(\{|[a-z_]+)')
SCHEMA_FIELD = re.compile(r"\b([a-z_][a-z_0-9]*)\s*(?::|(?=,|$))")
SPREAD = re.compile(r"\.\.\.([a-z_]+)")
CALL = re.compile(r"fabric_([a-z_]+)\s*([({])([^})\n]*)[})]")
FIELD = re.compile(r"\b([a-z_][a-z_0-9]*)\s*:")
TOOL = re.compile(r"\bfabric_[a-z_]+\b")
BACKTICK_FIELD = re.compile(r"`([a-z_][a-z_0-9]*)(?:\s*:[^`]*)?`")


def object_body(source: str, start: int) -> str:
    """Return the outer object body; inner zod calls may contain objects."""
    depth = 0
    quote = None
    escaped = False
    for index in range(start, len(source)):
        char = source[index]
        if quote:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == quote:
                quote = None
        elif char in "\"'`":
            quote = char
        elif char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return source[start + 1:index]
    raise AssertionError("unclosed server schema")


def server_schema(source: str) -> dict[str, set[str]]:
    source = source.split('if (process.env.FABRIC_LEGACY_TOOLS === "1")', 1)[0]
    shared = {}
    for name in ("route", "task", "batch"):
        match = re.search(rf"const {name} =\s*{{", source)
        assert match, name
        shared[name] = object_body(source, match.end() - 1)

    def fields(body: str) -> set[str]:
        result = set(SCHEMA_FIELD.findall(body))
        for spread in SPREAD.findall(body):
            result.update(fields(shared[spread]))
        return result

    schema = {}
    for match in REGISTER.finditer(source):
        token = match.group(2)
        body = object_body(source, match.end() - 1) if token == "{" else shared[token]
        schema[match.group(1)] = fields(body)
    return schema


SCHEMA = server_schema(SERVER.read_text())


def call_fits_schema(tool: str, fields: list[str]) -> bool:
    return tool in SCHEMA and not (set(fields) - SCHEMA[tool])


def documentation_paths():
    for directory in ("skills", "docs"):
        yield from (ROOT / directory).rglob("*.md")
    yield from (ROOT / "workflows").rglob("*.js")


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


def test_checker_reads_twelve_registered_tools_and_all_doc_surfaces():
    assert len(SCHEMA) == 12
    assert {"tasks", "concurrency", "resume"} <= SCHEMA["fabric_dispatch"]
    assert {"ids", "wait_seconds", "until"} <= SCHEMA["fabric_status"]
    assert {"id", "reason"} <= SCHEMA["fabric_cancel"]
    assert {"id", "part", "offset", "max_bytes"} <= SCHEMA["fabric_output"]
    assert {"detail"} <= SCHEMA["fabric_dispatch"]
    assert {"detail"} <= SCHEMA["fabric_status"]
    assert {"detail"} <= SCHEMA["fabric_adapters"]
    assert {"detail"} <= SCHEMA["fabric_whoami"]
    directories = {path.relative_to(ROOT).parts[0] for path in documentation_paths()}
    assert {"skills", "workflows", "docs"} <= directories


def test_checker_rejects_unknown_field_and_removed_tool():
    assert list(documented_calls("fabric_dispatch{imaginary: true}")) == [("fabric_dispatch", ["imaginary"])]
    assert "imaginary" not in SCHEMA["fabric_dispatch"]
    assert "fabric_batch" not in SCHEMA


def test_checker_rejects_removed_tool_even_without_fields():
    tool, fields = next(documented_calls("fabric_batch()"))
    assert not call_fits_schema(tool, fields)
