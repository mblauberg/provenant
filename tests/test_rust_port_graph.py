"""Retired tooling is recorded for deletion, never dispatched as a Rust port."""

import json
from pathlib import Path
import re
import subprocess
import sys

import pytest

from tests.support.modules import load_script

ROOT = Path(__file__).resolve().parents[1]
PLAN = ROOT / "docs/specs/rust-port"


@pytest.fixture(scope="module")
def graph():
    generator = load_script(PLAN / "derive_graph.py", "rust_port_graph")
    return json.loads(generator.generate()[PLAN / "graph.json"])


def test_graph_version_matches_the_rulebook(graph):
    version = re.search(r"Version: \*\*(\d+)\*\*", (PLAN / "PORTING.md").read_text())
    assert graph["version"] == graph["rulebook_version"] == int(version[1])


@pytest.mark.parametrize("crate", ["core", "cli"])
def test_workspace_tooling_stays_with_its_crate(crate):
    generator = load_script(PLAN / "derive_graph.py", "rust_port_graph_owner")
    assert generator.crate(f"tools/front-door/crates/{crate}/scripts/generate_unicode.py") == crate


def test_retired_units_keep_deletion_criteria_without_port_prerequisites(graph):
    criteria = {
        "scripts/static-security-check.py": "no tracked Python sources remain",
        "scripts/lint-timebomb-fixtures.py": "no tests/*.py and no *.test.ts remain",
    }
    seen = set()
    for node in graph["nodes"]:
        if node["source_files"] and node["source_files"][0] in criteria and node["kind"] == "retirement":
            source = node["source_files"][0]
            seen.add(source)
            assert node["action"] == "retired"
            assert node["deletion_criterion"] == criteria[source]
            assert node["dispatchable"] is False
            assert node["gates"] == node["depends_on"] == []
            assert node["lane_days"] == 0
    assert seen == set(criteria)
    assert not any(node["kind"] == "port" and set(node["source_files"]) & criteria.keys()
                   for node in graph["nodes"])


def test_yaml_work_is_separate_and_blocks_the_harness_port(graph):
    nodes = {node["id"]: node for node in graph["nodes"]}
    migration = nodes["records:yaml-data-to-json"]
    reader = nodes["records:external-yaml-subset"]
    assert migration["target_crate"] == reader["target_crate"] == "records"
    assert "config/adapter-compatibility.yaml" in migration["source_files"]
    assert "docs/evals/skill-portfolio-2026/predecessor/routing-holdout-20260714.yaml" in migration["source_files"]
    assert "skills/setup-repo/templates/ISSUE_TEMPLATE/bug.yml" in reader["source_files"]
    templates = subprocess.check_output([
        "git", "ls-files", "skills/setup-repo/templates/*.yml", "skills/setup-repo/templates/*.yaml",
    ], cwd=ROOT, text=True).splitlines()
    assert set(templates) <= set(reader["source_files"])
    assert "skills/engineering-writing/agents/openai.yaml" in reader["source_files"]
    assert "skills/engineering-writing/SKILL.md" in reader["source_files"]
    assert not set(migration["source_files"]) & set(reader["source_files"])
    for node in nodes.values():
        if "scripts/check_harness.py" in node["source_files"]:
            assert {migration["id"], reader["id"]} <= set(node["depends_on"])


def test_command_lint_scope_includes_integration_tests(graph):
    lint = next(node for node in graph["nodes"] if node["id"] == "workspace:command-construction-lint")
    integration_tests = subprocess.check_output([
        "git", "ls-files", "tools/front-door/tests/*.rs", "tools/front-door/crates/*/tests/*.rs",
    ], cwd=ROOT, text=True).splitlines()
    assert integration_tests
    assert set(integration_tests) <= set(lint["source_files"])


def test_ready_skips_retired_units_and_releases_their_dependants(tmp_path):
    (tmp_path / "ready.py").write_text((PLAN / "ready.py").read_text())
    (tmp_path / "ported.txt").write_text("")
    nodes = [
        {"id": "retire", "action": "retired", "dispatchable": False, "target_crate": "cli",
         "depends_on": [], "lane_days": 20, "source_files": []},
        {"id": "retarget", "action": "redesigned", "kind": "integration", "risk": "medium",
         "target_crate": "cli", "depends_on": ["retire"], "lane_days": 0.5, "source_files": []},
    ]
    (tmp_path / "graph.json").write_text(json.dumps({
        "nodes": nodes, "leaves_first_waves": [["retire"], ["retarget"]],
    }))
    result = subprocess.run([sys.executable, str(tmp_path / "ready.py"), "cli", "12"],
                            capture_output=True, text=True, check=True)
    assert "`retire`" not in result.stdout
    assert "`retarget`" in result.stdout
    assert "1 units, 0.5 lane-days" in result.stderr
