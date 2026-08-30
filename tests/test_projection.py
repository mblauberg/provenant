"""The marked-region projection primitive: split, compare, report, write.

The load-bearing case is the absent marker. A projection whose markers have
been deleted must raise, never return an empty report: an implementation that
silently skips the region would let marker deletion opt a document out of its
drift gate, which is the failure the gate exists to stop.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from scripts.lib.projection import ProjectionError, project, split_marked_region

ROOT = Path(__file__).resolve().parents[1]

MARKER = "sample-region"
DOC = (
    "# Title\n\n"
    "<!-- sample-region:start -->\n"
    "old body\n"
    "<!-- sample-region:end -->\n\n"
    "tail prose\n"
)
RENDERED = DOC.replace("old body", "new body")


def test_split_partitions_the_document_around_the_markers():
    head, region, tail = split_marked_region(DOC, MARKER)
    assert head + region + tail == DOC
    assert head.endswith("<!-- sample-region:start -->")
    assert tail.startswith("<!-- sample-region:end -->")
    assert region == "\nold body\n"


@pytest.mark.parametrize(
    ("name", "text"),
    (
        ("no markers at all", DOC.replace("sample-region:", "other-region:")),
        ("start marker deleted", DOC.replace("<!-- sample-region:start -->\n", "")),
        ("end marker deleted", DOC.replace("<!-- sample-region:end -->\n", "")),
        ("a second pair", DOC + "<!-- sample-region:start -->\n<!-- sample-region:end -->\n"),
        ("end before start", "<!-- sample-region:end -->\nbody\n<!-- sample-region:start -->\n"),
    ),
)
def test_split_refuses_anything_but_one_ordered_pair(name, text):
    with pytest.raises(ProjectionError):
        split_marked_region(text, MARKER)


def test_split_names_the_document_in_its_report():
    with pytest.raises(ProjectionError, match="README needs exactly one"):
        split_marked_region("no markers here\n", MARKER, name="README")


def test_project_reports_nothing_when_the_document_matches(tmp_path):
    path = tmp_path / "doc.md"
    path.write_text(DOC)
    assert project(path, MARKER, DOC, check=True) == []
    assert project(path, MARKER, DOC, check=False) == []
    assert path.read_text() == DOC


def test_project_check_reports_drift_without_writing(tmp_path):
    path = tmp_path / "doc.md"
    path.write_text(DOC)
    report = project(path, MARKER, RENDERED, check=True)
    assert any(line.startswith("-old body") for line in report)
    assert any(line.startswith("+new body") for line in report)
    assert path.read_text() == DOC, "check mode must not write"


def test_project_write_mode_rewrites_a_drifted_document(tmp_path):
    path = tmp_path / "doc.md"
    path.write_text(DOC)
    report = project(path, MARKER, RENDERED, check=False)
    assert report, "a rewrite must be reported, not silent"
    assert path.read_text() == RENDERED


def test_project_labels_the_report_with_the_source_of_truth(tmp_path):
    path = tmp_path / "doc.md"
    path.write_text(DOC)
    report = project(path, MARKER, RENDERED, check=True, source="skills/")
    assert any("doc.md (on disk)" in line for line in report)
    assert any("doc.md (rendered from skills/)" in line for line in report)


@pytest.mark.parametrize("check", (True, False))
def test_project_fails_loudly_when_the_marked_region_is_absent_on_disk(tmp_path, check):
    # The regression the primitive exists to close: deleting the markers must
    # fail the gate. A version that silently skips returns [] and passes here,
    # so this test fails against it.
    path = tmp_path / "doc.md"
    path.write_text(DOC.replace("sample-region:", "deleted:"))
    before = path.read_text()
    with pytest.raises(ProjectionError):
        project(path, MARKER, RENDERED, check=check)
    assert path.read_text() == before, "a failing projection must not write"


def test_project_refuses_a_rendering_that_lost_the_region(tmp_path):
    # If the renderer itself drops the markers, writing would destroy the
    # region permanently; refuse and leave the document alone.
    path = tmp_path / "doc.md"
    path.write_text(DOC)
    with pytest.raises(ProjectionError, match="rendered doc.md"):
        project(path, MARKER, "no markers survived rendering\n", check=False)
    assert path.read_text() == DOC


def test_architecture_lifecycle_projection_contains_the_runtime_state_graph():
    architecture = (ROOT / "docs" / "ARCHITECTURE.md").read_text()
    contract = json.loads(
        (ROOT / "skills" / "deliver" / "contract" / "lifecycle.v1.json").read_text()
    )
    _, region, _ = split_marked_region(architecture, "delivery-state-machine")

    normal_edges = set()
    for line in region.splitlines():
        if " --> " not in line:
            continue
        source, target = (part.strip() for part in line.split(" --> ", 1))
        target = target.split(":", 1)[0].strip()
        if source == "[*]" or target == "[*]":
            continue
        normal_edges.add((source, target))

    expected_edges = {
        (transition["state"], transition["to_state"])
        for transition in contract["transitions"]
    }
    assert normal_edges == expected_edges
    assert {node for edge in normal_edges for node in edge} == set(contract["states"])
    assert {
        line.strip()
        for line in region.splitlines()
        if line.strip() in contract["side_states"]
    } == set(contract["side_states"])
    assert contract["source"]["state_graph"].endswith("#state-graph")


def test_architecture_risk_projection_matches_the_runtime_policy():
    architecture = (ROOT / "docs" / "ARCHITECTURE.md").read_text()
    policy = json.loads((ROOT / "config" / "risk-policy.json").read_text())
    _, region, _ = split_marked_region(architecture, "risk-factor-table")
    rows = {}
    for line in region.splitlines():
        if not line.startswith("|") or line.startswith("|---"):
            continue
        cells = [cell.strip() for cell in line.strip("|").split("|")]
        factor = cells[0].replace(" ", "_") if cells else ""
        if factor in policy["factors"]:
            rows[factor] = cells[1:]

    tiers = policy["tier_order"]
    assert set(rows) == set(policy["factors"])
    for factor, mapping in policy["factors"].items():
        expected = [
            {value for value, tier in mapping.items() if tier == level}
            for level in tiers
        ]
        actual = [
            {value.strip() for value in cell.replace("`", "").split(",") if value.strip()}
            for cell in rows[factor]
        ]
        assert actual == expected
