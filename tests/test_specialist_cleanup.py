from __future__ import annotations

import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def _text(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


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
