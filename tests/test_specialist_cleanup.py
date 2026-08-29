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


def test_react_request_and_listener_rules_are_stack_neutral() -> None:
    rules = ROOT / "skills/react-performance/rules"
    assert not (rules / "client-swr-dedup.md").exists()

    request_rule = _text("skills/react-performance/rules/client-request-dedup.md")
    listener_rule = _text("skills/react-performance/rules/client-event-listeners.md")
    index = _text("skills/react-performance/references/rule-index.md")

    assert "existing data owner" in request_rule.lower()
    assert "dependency and architecture" in request_rule.lower()
    assert "import useSWR" not in request_rule
    assert "useSWRSubscription" not in listener_rule
    assert "client-request-dedup.md" in index
    assert "client-swr-dedup.md" not in index


def test_uml_scripts_are_portable_and_templates_lint_from_another_cwd(tmp_path: Path) -> None:
    skill = ROOT / "skills/uml-diagrams"
    linter = skill / "scripts/lint_plantuml_diagram.py"

    for template, diagram_type in (
        ("use_case_package_template.puml", "package"),
        ("use_case_diagram_template.puml", "usecase"),
        ("activity_diagram_template.puml", "activity"),
    ):
        assert "skinparam handwritten" not in (skill / "templates" / template).read_text(
            encoding="utf-8"
        )
        result = subprocess.run(
            [
                sys.executable,
                str(linter),
                str(skill / "templates" / template),
                "--type",
                diagram_type,
            ],
            cwd=tmp_path,
            text=True,
            capture_output=True,
            check=False,
        )
        assert result.returncode == 0, result.stdout + result.stderr
        assert "ERROR:" not in result.stdout + result.stderr

    help_result = subprocess.run(
        [sys.executable, str(skill / "scripts/render_plantuml.py"), "--help"],
        cwd=tmp_path,
        text=True,
        capture_output=True,
        check=False,
    )
    assert help_result.returncode == 0, help_result.stdout + help_result.stderr


def test_uml_core_has_no_course_or_fixed_model_profile() -> None:
    skill = ROOT / "skills/uml-diagrams"
    corpus = "\n".join(
        path.read_text(encoding="utf-8")
        for path in sorted(skill.rglob("*"))
        if path.is_file() and path.suffix in {".md", ".py", ".puml"}
    )
    for fixed_name in ("gpt-5-codex", "Sonnet", "Haiku", "Gemini", "Band-7"):
        assert fixed_name not in corpus
    for course_contract in ("Section 5 Table 5", "(defined in P<n>)"):
        assert course_contract not in corpus

    entry = _text("skills/uml-diagrams/SKILL.md")
    assert '${AGENTS_HOME:-$HOME/.agents}' in entry
    assert "project's diagram profile" in " ".join(entry.lower().split())


def test_uml_auto_detects_grouped_oval_use_cases(tmp_path: Path) -> None:
    diagram = tmp_path / "grouped-use-cases.puml"
    diagram.write_text(
        """@startuml
actor "User" as User
rectangle "Account service" {
  package "Authentication" {
    (Sign in) as UC_SignIn
  }
}
User --> UC_SignIn
@enduml
""",
        encoding="utf-8",
    )
    linter = ROOT / "skills/uml-diagrams/scripts/lint_plantuml_diagram.py"
    result = subprocess.run(
        [sys.executable, str(linter), str(diagram), "--type", "auto"],
        text=True,
        capture_output=True,
        check=False,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    assert "package-only overview" not in result.stdout + result.stderr


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
