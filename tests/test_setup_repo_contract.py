from pathlib import Path

import yaml


ROOT = Path(__file__).resolve().parents[1]
SKILL = ROOT / "skills" / "setup-repo"


def _instructions() -> str:
    return (SKILL / "SKILL.md").read_text()


def test_frontmatter_carries_exactly_the_loader_schema():
    frontmatter = yaml.safe_load(_instructions().split("---", 2)[1])

    assert set(frontmatter) == {"name", "description"}
    assert frontmatter["name"] == "setup-repo"
    assert frontmatter["name"] == SKILL.name


def test_fresh_scaffold_includes_the_security_policy_linked_by_issue_forms():
    security_policy = SKILL / "templates" / "SECURITY.md"
    config = yaml.safe_load(
        (SKILL / "templates" / "ISSUE_TEMPLATE" / "config.yml").read_text()
    )
    security_link = config["contact_links"][0]
    policy = security_policy.read_text()

    assert security_policy.is_file()
    assert "<private-reporting-route>" in policy
    assert security_link["url"].endswith("/blob/main/SECURITY.md")


def test_repository_process_template_is_the_invariant_completion_artifact():
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
