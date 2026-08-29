from pathlib import Path
import re
import shutil
import subprocess

import pytest
import yaml


ROOT = Path(__file__).resolve().parents[1]


def frontmatter_name(path: Path) -> str:
    text = path.read_text()
    match = re.search(r"^name:\s*([^\n]+)$", text, re.MULTILINE)
    assert match, f"missing name in {path}"
    return match.group(1).strip()


def test_lifecycle_skills_are_portable_and_named_for_their_directory():
    for name in ("implement", "code-review"):
        skill = ROOT / "skills" / name / "SKILL.md"
        assert skill.is_file(), f"missing portable {name} skill"
        assert frontmatter_name(skill) == name


def test_root_harness_checker_is_available():
    checker = ROOT / "scripts" / "check-harness"
    assert checker.is_file()
    assert checker.stat().st_mode & 0o111


def test_dispatchers_use_the_stable_product_command_and_local_skill_helpers():
    dispatcher = (ROOT / "skills" / "orchestrate" / "scripts" / "cf_dispatch.sh").read_text()
    assert 'resolve_routing' in dispatcher  # tries provenant, falls back to model_route.py
    assert '"$SCRIPT_DIR/codex_capabilities.py"' in dispatcher
    assert '-c service_tier="default"' in dispatcher
    assert "AGENTS_ROOT" not in dispatcher
    assert "HARNESS_ROOT" not in dispatcher


def test_default_agent_run_directory_is_ignored_in_the_harness_repo():
    assert ".agent-run/" in (ROOT / ".gitignore").read_text().splitlines()


def test_configured_workspace_execution_is_family_agnostic_but_assurance_is_explicit():
    harness = (ROOT / "HARNESS.md").read_text()
    scope = (ROOT / "skills/scope/SKILL.md").read_text()
    routing = (ROOT / "skills/orchestrate/references/routing-and-tiers.md").read_text()
    orchestrate = (ROOT / "skills/orchestrate/SKILL.md").read_text()

    assert "ordinary workspace content" in harness
    assert "ordinary authorised workspace content" in scope.lower()
    assert "family separation" in harness
    assert "family-separation" in scope
    assert "family separation" in routing
    assert "family separation" in orchestrate
    assert "family separation is an assurance property" in " ".join(harness.split())
    assert "without a family-separation gate" in scope
    assert "assurance claim" in routing
    assert "execution freedom" in orchestrate


def test_dispatch_manifest_and_delivery_run_have_distinct_owners():
    adr = (ROOT / "docs/adr/0021-configured-workspace-dispatch-boundaries.md").read_text()

    assert "compact dispatch manifest" in adr
    assert "is not a delivery `RUN.json`" in adr
    assert "one dispatch owner" in adr.lower()
    assert "Fabric remains coordination-only" in adr


def test_thin_cli_decision_is_amended_by_dispatch_boundary_decision():
    index = (ROOT / "docs/adr/README.md").read_text()
    thin_cli = (ROOT / "docs/adr/0013-thin-provenant-cli.md").read_text()

    assert "0021" in index
    assert "0021-configured-workspace-dispatch-boundaries.md" in thin_cli
    assert "bounded dispatch and batch commands" in thin_cli


@pytest.mark.skipif(shutil.which("mmdc") is None, reason="optional local Mermaid CLI is absent")
def test_readme_mermaid_parses_with_available_local_renderer(tmp_path):
    readme = (ROOT / "README.md").read_text()
    diagrams = re.findall(r"```mermaid\n(.*?)\n```", readme, re.DOTALL)
    for index, diagram in enumerate(diagrams):
        source = tmp_path / f"diagram-{index}.mmd"
        output = tmp_path / f"diagram-{index}.svg"
        source.write_text(diagram)
        subprocess.run(
            ["mmdc", "-i", str(source), "-o", str(output)],
            cwd=tmp_path,
            check=True,
            capture_output=True,
            text=True,
        )
        assert output.is_file()


def test_openai_skill_sidecar_descriptions_fit_provider_contract():
    for path in (ROOT / "skills").glob("*/agents/openai.yaml"):
        value = yaml.safe_load(path.read_text())
        description = value["interface"]["short_description"]
        assert 25 <= len(description) <= 64, path


@pytest.mark.parametrize("broken_suffix", ("js", "mjs", "cjs"))
def test_skill_javascript_gate_checks_all_module_suffixes_and_prunes_dependencies(
    tmp_path: Path,
    broken_suffix: str,
):
    checker = ROOT / "scripts" / "check-skill-javascript"
    assert checker.stat().st_mode & 0o111
    skills = tmp_path / "skills"
    skills.mkdir()
    (skills / "valid.js").write_text("const valid = true;\n")
    (skills / "valid.mjs").write_text(
        "export const valid = await Promise.resolve(true);\n"
    )
    (skills / "valid.cjs").write_text("module.exports = { valid: true };\n")
    vendored = skills / "node_modules"
    vendored.mkdir()
    (vendored / "ignored.mjs").write_text("const = broken;\n")

    passing = subprocess.run(
        [str(checker), str(skills)],
        check=False,
        capture_output=True,
        text=True,
    )
    assert passing.returncode == 0, passing.stderr
    assert "PASS: checked 3 skill JavaScript files" in passing.stdout

    broken = skills / f"broken.{broken_suffix}"
    broken.write_text("const = broken;\n")
    failing = subprocess.run(
        [str(checker), str(skills)],
        check=False,
        capture_output=True,
        text=True,
    )
    assert failing.returncode != 0
    assert str(broken) in failing.stderr


def test_skill_javascript_gate_rejects_an_empty_tree(tmp_path: Path):
    checker = ROOT / "scripts" / "check-skill-javascript"
    result = subprocess.run(
        [str(checker), str(tmp_path)],
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode != 0
    assert "no skill JavaScript files found" in result.stderr
