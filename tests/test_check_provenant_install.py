"""Registration diagnostics used by the surviving Fabric host query."""
import json
from pathlib import Path

import pytest
from tests.support.modules import load_script

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/check-provenant-install.py"


@pytest.fixture
def registration(tmp_path, monkeypatch):
    monkeypatch.syspath_prepend(str(ROOT / "scripts"))
    for name in ("CLAUDE_CONFIG_DIR", "CLAUDE_MCP_CONFIG", "CODEX_HOME", "CODEX_MCP_CONFIG",
                 "OPENCODE_CONFIG_DIR", "OPENCODE_MCP_CONFIG", "AGY_CONFIG_DIR", "AGY_MCP_CONFIG",
                 "CURSOR_MCP_CONFIG", "KIRO_MCP_CONFIG"):
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setenv("PATH", "/usr/bin:/bin")
    return load_script(SCRIPT, "install_check_hosts_test"), tmp_path / "home"


def test_registration_diagnostics_do_not_require_an_adapter_on_path(registration):
    module, home = registration
    registry = home / ".gemini/config/mcp_config.json"
    registry.parent.mkdir(parents=True)
    registry.write_text(json.dumps({"mcpServers": {"fabric": {"command": "provenant"}}}))
    status = module.provider_registration_status(home)["agy"]
    assert status == {"present": False, "skills": False, "registered": True}


def test_registration_accepts_commented_opencode_jsonc(registration):
    module, home = registration
    root = home / ".config/opencode"
    skill = root / "skills/orchestrate/SKILL.md"
    skill.parent.mkdir(parents=True)
    skill.write_text("# Test\n")
    (root / "opencode.jsonc").write_text('{\n// keep\n"mcp": {"fabric": {}},\n}\n')
    assert module.provider_registration_status(home)["opencode"] == {
        "present": True, "skills": True, "registered": True,
    }


@pytest.mark.parametrize("content", ["[]", "invalid json", '{"mcp": []}'])
def test_registration_reports_invalid_provider_config(registration, content):
    module, home = registration
    config = home / ".config/opencode/opencode.jsonc"
    config.parent.mkdir(parents=True)
    config.write_text(content)
    assert module.provider_registration_status(home)["opencode"] == {
        "present": True, "skills": False, "registered": False,
    }


def test_registration_does_not_detect_agy_from_gemini_directory_alone(registration):
    module, home = registration
    (home / ".gemini").mkdir(parents=True)
    assert module.provider_registration_status(home)["agy"]["present"] is False


def test_registration_uses_the_mcp_config_override(registration, tmp_path, monkeypatch):
    module, home = registration
    skill = home / ".cursor/skills/orchestrate/SKILL.md"
    skill.parent.mkdir(parents=True)
    skill.write_text("# Test\n")
    config = tmp_path / "custom-cursor.json"
    config.write_text('{"mcpServers": {"fabric": {}}}')
    monkeypatch.setenv("CURSOR_MCP_CONFIG", str(config))
    assert module.provider_registration_status(home)["cursor"] == {
        "present": True, "skills": True, "registered": True,
    }
