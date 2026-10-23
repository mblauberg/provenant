"""Cross-implementation MCP protocol and tool contract judge."""

from __future__ import annotations

import json
import os
from pathlib import Path
import tempfile

import pytest

from client import McpProcess, capture_path, normalize, resolve_refs, seed_fetch_fixture, seed_hosts, seed_run


HERE = Path(__file__).resolve().parent
TRANSCRIPTS = HERE / "transcripts"


def read_jsonl(path: Path) -> list[dict]:
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def write_jsonl(path: Path, rows: list[dict]) -> None:
    path.write_text("".join(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n" for row in rows), encoding="utf-8")


def tool_result(reply: dict) -> dict:
    assert "error" not in reply, reply
    assert "result" in reply, reply
    return reply["result"]


def test_initialize_and_tools_list_match_golden():
    path = TRANSCRIPTS / "protocol.jsonl"
    recording = os.environ.get("MCP_JUDGE_RECORD") == "1"
    with tempfile.TemporaryDirectory(prefix="mcp-judge-") as tmp:
        base = Path(tmp)
        home, state = base / "home", base / "state"
        project = home / "Repos/project"
        for directory in (home, project, state):
            directory.mkdir(parents=True)
        seed_hosts(home)
        server = McpProcess(home, project, state)
        try:
            initialize = server.initialization
            tools = server.request("tools/list", {})
        finally:
            server.close()
    actual = [
        {"label": "initialize", "request": normalize(server.requests[0], base), "response": normalize(initialize, base)},
        {"label": "tools/list", "request": normalize(server.requests[1], base), "response": normalize(tools, base)},
    ]
    if recording:
        write_jsonl(path, actual)
    expected = read_jsonl(path)
    assert actual == expected
    listed = tools["result"]["tools"]
    assert len({item["name"] for item in listed}) == len(listed)


@pytest.mark.parametrize("scenario", sorted(TRANSCRIPTS.glob("*.scenario.jsonl")), ids=lambda path: path.stem)
def test_tool_scenario_transcript(scenario: Path):
    path = scenario.with_name(scenario.name.replace(".scenario", ""))
    recording = os.environ.get("MCP_JUDGE_RECORD") == "1"
    steps = read_jsonl(scenario)
    with tempfile.TemporaryDirectory(prefix="mcp-judge-") as tmp:
        base = Path(tmp)
        home, state = base / "home", base / "state"
        project = home / "Repos/project"
        for directory in (home, project, state):
            directory.mkdir(parents=True)
        seed_run(project)
        seed_hosts(home)
        fixture_env = seed_fetch_fixture(home, project, state) if any(step.get("fixture") == "fetch" for step in steps) else None
        server = McpProcess(home, project, state, fixture_env)
        captures: dict[str, object] = {}
        actual = []
        try:
            for step in steps:
                request = step["request"]
                arguments = resolve_refs(request.get("arguments", {}), captures)
                reply = server.call(request["name"], arguments)
                tool_result(reply)
                if step.get("kind") == "error":
                    result = reply["result"]
                    structure = result.get("structuredContent", {})
                    response_text = " ".join(block.get("text", "") for block in result.get("content", []))
                    assert result.get("isError") or structure.get("status") in {"rejected", "unknown"} or response_text.startswith("rejected"), step["label"]
                elif step.get("kind") == "happy":
                    result = reply["result"]
                    structure = result.get("structuredContent", {})
                    response_text = " ".join(block.get("text", "") for block in result.get("content", []))
                    assert not result.get("isError"), step["label"]
                    assert structure.get("status") not in {"rejected", "unknown"} and not response_text.startswith("rejected"), step["label"]
                for key, path_expression in step.get("capture", {}).items():
                    captures[key] = capture_path(reply, path_expression)
                actual.append({
                    "label": step["label"],
                    "request": request,
                    "wire_request": normalize(server.requests[-1], base),
                    "response": normalize(reply, base),
                })
        finally:
            server.close()
    if recording:
        write_jsonl(path, actual)
    expected = read_jsonl(path)
    assert actual == expected


def test_every_exposed_tool_has_happy_and_error_scenario():
    protocol = read_jsonl(TRANSCRIPTS / "protocol.jsonl")
    names = {tool["name"] for tool in protocol[1]["response"]["result"]["tools"]}
    steps = [step for file in TRANSCRIPTS.glob("*.scenario.jsonl") for step in read_jsonl(file)]
    covered = {step["request"]["name"] for step in steps}
    assert covered == names
    error_only = {"fabric_dispatch": "provider-free validation only, as required by task"}
    for name in names:
        cases = [step for step in steps if step["request"]["name"] == name]
        assert any(step.get("kind") == "error" for step in cases), name
        if name in error_only:
            assert sum(step.get("kind") == "error" for step in cases) >= 2, error_only[name]
        else:
            assert any(step.get("kind") == "happy" for step in cases), name
    assert {step["request"]["name"] for step in steps if step.get("kind") == "happy"} | error_only.keys() == names
