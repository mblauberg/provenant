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
