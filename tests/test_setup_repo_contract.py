from pathlib import Path
import json
import subprocess
import sys

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


def test_tracker_hook_routes_only_raw_writes_when_command_is_declared(tmp_path):
    hook = tmp_path / ".claude/hooks/tracker-route.py"
    hook.parent.mkdir(parents=True)
    hook.write_bytes((SKILL / "templates/claude-tracker-hook.py").read_bytes())
    settings = json.loads((SKILL / "templates/claude-tracker-settings.json").read_text())
    assert settings["hooks"]["PreToolUse"][0]["matcher"] == "Bash"
    assert settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"].endswith("tracker-route.py")
    declaration = tmp_path / "MAINTAINING.md"
    declaration.write_text("### Tracker\n- Choice: `github-issues`\n- Command: `pnpm issue`\n\n### Scope and stories\n")

    def decision(command, tool="Bash"):
        result = subprocess.run([sys.executable, str(hook)], input=json.dumps({
            "tool_name": tool, "tool_input": {"command": command},
        }), text=True, capture_output=True, check=True)
        return json.loads(result.stdout) if result.stdout else None

    for command in ("gh issue create -t bug", "gh issue edit 12 --title fixed",
                    "gh issue close 12", "gh project item-add 2 --url x",
                    "gh project item-archive 2 --id x", "gh project item-create 2 --title x",
                    "gh project item-edit --id x", "gh project item-delete 2 --id x",
                    "cd repo && gh -R owner/repo issue create -t bug"):
        assert decision(command)["hookSpecificOutput"] == {
            "hookEventName": "PreToolUse", "permissionDecision": "deny",
            "permissionDecisionReason": "Use the repository tracker command: pnpm issue",
        }
    for command in ("gh issue list", "gh issue view 12", "gh project item-list 2",
                    "pnpm issue create", "echo 'gh issue create'", "git status"):
        assert decision(command) is None
    assert decision("gh issue create", tool="Read") is None
    declaration.write_text("### Tracker\n- Choice: `github-issues`\n- Command: `none`\n")
    assert decision("gh issue create") is None
