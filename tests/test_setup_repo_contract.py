from pathlib import Path

import yaml


ROOT = Path(__file__).resolve().parents[1]
SKILL = ROOT / "skills" / "setup-repo"


def _instructions() -> str:
    return (SKILL / "SKILL.md").read_text()


def test_frontmatter_names_and_routes_the_broadened_skill():
    instructions = _instructions()
    frontmatter = yaml.safe_load(instructions.split("---", 2)[1])
    boundary = frontmatter["description"][:250].lower()

    assert set(frontmatter) == {"name", "description"}
    assert frontmatter["name"] == "setup-repo"
    for trigger in (
        "set up",
        "scaffold",
        "repository",
        "process",
        "labels",
        "tracker",
        "branch ruleset",
        "issue forms",
        "docs layout",
        "work-item runbook",
        "board",
    ):
        assert trigger in boundary
    assert "not for this repo's own day-to-day github mechanics" in boundary
    assert "use its runbook" in boundary


def test_fresh_scaffold_includes_the_security_policy_linked_by_issue_forms():
    security_policy = SKILL / "templates" / "SECURITY.md"
    config = yaml.safe_load(
        (SKILL / "templates" / "ISSUE_TEMPLATE" / "config.yml").read_text()
    )
    security_link = config["contact_links"][0]
    instructions = _instructions()
    normalised = " ".join(instructions.split())
    policy = security_policy.read_text()

    assert security_policy.is_file()
    assert "<private-reporting-route>" in policy
    assert security_link["url"].endswith("/blob/main/SECURITY.md")
    assert "templates/SECURITY.md" in normalised
    assert "private vulnerability reporting is enabled" in normalised
    assert "working confidential contact method" in normalised
    assert "replace `<private-reporting-route>`" in normalised
    assert "Publish only when the placeholder is gone and both routes work" in normalised


def test_repository_process_template_is_the_invariant_completion_artifact():
    instructions = _instructions()
    normalised = " ".join(instructions.split())
    declarations = SKILL / "templates" / "repo-declarations.md"

    assert declarations.is_file()
    template = declarations.read_text()
    assert template.startswith("## Repository process\n")
    for heading in ("### Tracker", "### Docs layout", "### Merge policy", "### Work-item runbook"):
        assert heading in template
    for placeholder in (
        "<github-issues|tracker-name|none>",
        "<tracker-url-or-none>",
        "<docs-index-or-home-list>",
        "<merge-policy-and-authority-path>",
        "<work-item-runbook-path-or-none>",
    ):
        assert placeholder in template

    assert "Use GitHub issues?" in instructions
    assert "Tracker-specific setup remains out of scope" in normalised
    assert "Repository process" in instructions
    assert "completion gate" in instructions
    assert "not the GitHub scaffolding" in instructions
    assert "per heading" in normalised
    assert "engineering-docs" in instructions
    assert "skills/engineering-docs" not in instructions


def test_inspect_classify_and_remote_gates_make_setup_convergent_and_safe():
    instructions = " ".join(_instructions().split())

    assert "keep (exact match)" in instructions
    assert "create (absent)" in instructions
    assert "adapt (compatible" in instructions
    assert "conflict (semantic mismatch" in instructions
    assert "A re-run against an already-set-up repository must produce no diff" in instructions
    assert "Never overwrite" in instructions
    assert "confirming the remote host before any `gh` command" in instructions
    assert "Only when the tracker is GitHub issues and the remote host is confirmed" in instructions
    for stop in (
        "unnamed target",
        "unconfirmed write permission",
        "replacement of an existing ruleset or labels file",
        "semantic conflict",
        "ambiguous `ci-status` dependencies",
    ):
        assert stop in instructions


def test_trigger_fixtures_cover_broadened_and_adjacent_routes():
    cases = yaml.safe_load((SKILL / "evals" / "trigger_cases.yaml").read_text())["cases"]
    routes = {case["id"]: case["expected"] for case in cases}

    assert routes["q900"] == {
        "primary_skill": "setup-repo",
        "companion_skills": [],
    }
    assert routes["q901"] == {
        "primary_skill": "setup-repo",
        "companion_skills": [],
    }
    assert routes["q903"] == {
        "primary_skill": None,
        "companion_skills": [],
    }
    assert routes["q905"] == {
        "primary_skill": "engineering-docs",
        "companion_skills": [],
    }
    assert routes["q906"] == {
        "primary_skill": "skill-craft",
        "companion_skills": [],
    }
    assert routes["q907"] == {
        "primary_skill": "setup-repo",
        "companion_skills": ["engineering-docs"],
    }
    assert routes["q908"] == {
        "primary_skill": "setup-repo",
        "companion_skills": ["implement"],
    }
