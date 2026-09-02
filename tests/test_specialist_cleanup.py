from __future__ import annotations

import subprocess
import sys
from pathlib import Path


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


def test_academic_workflow_defers_to_project_and_dynamic_routing() -> None:
    entry = _text("skills/academic-writing/SKILL.md")
    workflow = _text("skills/academic-writing/references/editing-workflows.md")
    references = "\n".join(
        path.read_text(encoding="utf-8")
        for path in sorted((ROOT / "skills/academic-writing/references").glob("*.md"))
    )

    assert "take precedence" in entry
    assert "runtime routing" in entry
    assert "scout capacity" in workflow
    for fixed_name in ("Codex", "Gemini", "Haiku", "Sonnet", "GPT-5"):
        assert fixed_name not in references
    for project_example in (
        "decision-turn AUPRC",
        "paired-seed evidence bundle",
        "hierarchical-memory scam detection",
    ):
        assert project_example not in references
