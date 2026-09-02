from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import yaml


ROOT = Path(__file__).resolve().parents[1]


def _text(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def test_live_cleanup_requires_exact_run_owned_process_identity() -> None:
    live = _text("skills/ui-ux-design/references/live.md")
    cleanup = " ".join(live[live.index("## Exit"):live.index("## Cleanup")].split())
    assert "exact background-task handle returned by this run" in cleanup
    assert "run-owned PID plus its command and start identity" in cleanup
    assert "Refuse broad name or pattern kills" in cleanup
    assert "pkill" not in cleanup


def test_d2_publication_workflow_is_current_and_runtime_routed() -> None:
    publication = _text("skills/d2-diagrams/references/publication-figures.md")
    cli = _text("skills/d2-diagrams/references/cli.md")
    corpus = publication + "\n" + cli

    assert "https://www.d2lang.com/tour/exports/" in publication
    assert "native pdf exports place the png render" in " ".join(publication.lower().split())
    assert "project-approved" in publication.lower()
    assert "orchestrate" in publication
    assert "external disclosure" in cli.lower()
    for stale_or_fixed in (
        "SUPERSEDED",
        "gpt-5-codex",
        "gemini --approval-mode",
        "codex exec",
        "Claude subagents",
    ):
        assert stale_or_fixed not in corpus


def test_academic_capability_moved_into_the_two_writing_owners(tmp_path: Path) -> None:
    assert not (ROOT / "skills/academic-writing").exists()

    checker = ROOT / "skills/natural-writing/scripts/check_academic_style.py"
    sample = tmp_path / "chapter.tex"
    sample.write_text(
        "This thesis report demonstrates a groundbreaking result --- "
        "see \\cite{key-a}.\n",
        encoding="utf-8",
    )
    result = subprocess.run(
        [sys.executable, str(checker), str(sample)],
        cwd=tmp_path,
        text=True,
        capture_output=True,
        check=False,
    )
    assert result.returncode == 1, result.stdout + result.stderr
    assert "Meta-discourse or report-referential" in result.stdout
    assert "LaTeX prose em dash marker" in result.stdout
    assert "cite" not in result.stdout

    prose = ROOT / "skills/natural-writing/references/academic-prose.md"
    artefacts = ROOT / "skills/engineering-writing/references/academic-artefacts.md"
    assert prose.is_file() and artefacts.is_file()
    assert "Never invent citation keys" in prose.read_text(encoding="utf-8")
    assert "Preserve exactly unless explicitly asked" in artefacts.read_text(
        encoding="utf-8"
    )


def _description(skill: str) -> str:
    front = _text(f"skills/{skill}/SKILL.md").split("---", 2)[1]
    return yaml.safe_load(front)["description"]


def test_both_writing_owners_name_academic_prose_in_their_trigger() -> None:
    assert "academic" in _description("natural-writing").casefold()
    assert "thesis" in _description("natural-writing").casefold()
    assert "academic" in _description("engineering-writing").casefold()
    assert "thesis" in _description("engineering-writing").casefold()
