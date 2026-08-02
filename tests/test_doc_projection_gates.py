"""The derived-doc gates: state machine, risk factors, constants and cited paths.

The load-bearing behaviours (issue #548): the graph structure of the delivery
state diagram is generated and cannot be hand-edited, while the accessibility
text, palette and edge labels round-trip freely; the comparison-form checks
fail when a doc figure leaves its source constant; and a cited repository path
that stopped existing fails the paths gate.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from scripts import check_doc_constants, check_doc_paths, render_doc_projections

ROOT = Path(__file__).resolve().parents[1]
ARCHITECTURE = ROOT / "docs" / "ARCHITECTURE.md"


def render_args(architecture: Path) -> list[str]:
    return ["--check", "--architecture", str(architecture), "--skills-root", str(ROOT / "skills")]


def constants_args(architecture: Path = ARCHITECTURE, harness: Path | None = None) -> list[str]:
    return [
        "--architecture", str(architecture),
        "--harness", str(harness or ROOT / "HARNESS.md"),
        "--readme", str(ROOT / "README.md"),
        "--skills-root", str(ROOT / "skills"),
        "--risk-policy", str(ROOT / "config" / "risk-policy.json"),
    ]


def test_the_checked_in_documents_pass_every_gate():
    assert render_doc_projections.main(render_args(ARCHITECTURE)) == 0
    assert check_doc_constants.main(constants_args()) == 0
    assert check_doc_paths.main(
        [
            str(ARCHITECTURE),
            str(ROOT / "README.md"),
            str(ROOT / "MAINTAINING.md"),
            str(ROOT / "docs" / "runbooks" / "agent-fabric-operations.md"),
            "--root",
            str(ROOT),
        ]
    ) == 0


def test_editing_the_generated_graph_structure_fails_check(tmp_path, capsys):
    drifted = tmp_path / "ARCHITECTURE.md"
    original = ARCHITECTURE.read_text()
    assert "        repairing --> verifying : the repair is re-verified\n" in original
    drifted.write_text(
        original.replace("        repairing --> verifying : the repair is re-verified\n", "")
    )
    assert render_doc_projections.main(render_args(drifted)) == 1
    assert "repairing --> verifying" in capsys.readouterr().out


def test_editing_the_hand_written_accessibility_text_round_trips(tmp_path):
    edited = tmp_path / "ARCHITECTURE.md"
    edited.write_text(
        ARCHITECTURE.read_text().replace(
            "accTitle: The delivery-run state machine",
            "accTitle: The delivery-run state machine, reworded by hand",
        )
    )
    assert render_doc_projections.main(render_args(edited)) == 0


@pytest.mark.parametrize("marker", ("delivery-state-machine", "risk-factor-table"))
def test_deleting_a_marker_pair_fails_instead_of_skipping(tmp_path, marker):
    unmarked = tmp_path / "ARCHITECTURE.md"
    unmarked.write_text(ARCHITECTURE.read_text().replace(f"{marker}:", "deleted:"))
    assert render_doc_projections.main(render_args(unmarked)) == 1


def test_a_label_on_a_transition_the_validator_lacks_fails(tmp_path, capsys):
    invented = tmp_path / "ARCHITECTURE.md"
    invented.write_text(
        ARCHITECTURE.read_text().replace(
            "        draft --> scoped\n",
            "        draft --> scoped\n        scoped --> executing : jumps the approval gate\n",
        )
    )
    assert render_doc_projections.main(render_args(invented)) == 1
    assert "scoped --> executing" in capsys.readouterr().err


def test_a_class_line_styling_an_unknown_state_fails(tmp_path):
    stale = tmp_path / "ARCHITECTURE.md"
    stale.write_text(ARCHITECTURE.read_text().replace("    class closed inert", "    class finished inert"))
    assert render_doc_projections.main(render_args(stale)) == 1


def test_a_stale_repair_budget_figure_fails_the_constants_gate(tmp_path, capsys):
    stale = tmp_path / "ARCHITECTURE.md"
    stale.write_text(
        ARCHITECTURE.read_text().replace(
            "repair by tier: routine 2, substantial 4, crucial/terminal 5",
            "repair by tier: routine 3, substantial 4, crucial/terminal 5",
        )
    )
    assert check_doc_constants.main(constants_args(architecture=stale)) == 1
    assert "repair-budget" in capsys.readouterr().err


def test_a_reordered_review_pressure_table_fails_the_constants_gate(tmp_path, capsys):
    reordered = tmp_path / "HARNESS.md"
    reordered.write_text(
        (ROOT / "HARNESS.md")
        .read_text()
        .replace("| `routine` |", "| `substantial` |", 1)
        .replace("| `substantial` |", "| `routine` |", 2)
    )
    assert check_doc_constants.main(constants_args(harness=reordered)) == 1
    assert "review-pressure table rows" in capsys.readouterr().err


def test_cited_paths_tolerate_line_suffixes_and_globs_but_not_absence(tmp_path, capsys):
    doc = tmp_path / "doc.md"
    doc.write_text(
        "`skills/deliver/scripts/validate_delivery.py:30-49` holds the states.\n"
        "Every skill ships `skills/*/SKILL.md` and a [runbook](docs/runbooks/github-workflow.md).\n"
        "Instance shapes such as `~/.codex/skills/` and `${AGENTS_HOME}` are not repo claims.\n"
        "```text\n`scripts/inside_a_fence_never_checked.py`\n```\n"
    )
    assert check_doc_paths.main([str(doc), "--root", str(ROOT)]) == 0
    doc.write_text("The gate reads `scripts/no_such_checker.py` on every run.\n")
    assert check_doc_paths.main([str(doc), "--root", str(ROOT)]) == 1
    assert "scripts/no_such_checker.py" in capsys.readouterr().err


def test_cited_python_commands_are_invocable_and_shell_fences_are_checked(
    tmp_path, capsys
):
    doc = tmp_path / "doc.md"
    doc.write_text(
        "```sh\n"
        "scripts/check_doc_paths.py --root .\n"
        "python3 skills/deliver/scripts/validate_delivery.py RUN.json\n"
        "```\n"
        "```text\n"
        "scripts/no_such_checker.py\n"
        "```\n"
    )

    assert check_doc_paths.main([str(doc), "--root", str(ROOT)]) == 1
    assert "Python command must use an interpreter" in capsys.readouterr().err

    doc.write_text(
        '```sh\n"${HARNESS_PYTHON:-.venv/bin/python}" '
        "scripts/check_doc_paths.py --root .\n```\n"
    )
    assert check_doc_paths.main([str(doc), "--root", str(ROOT)]) == 0
