import importlib.util
import json
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
PATH = ROOT / "skills" / "orchestrate" / "scripts" / "agy_capabilities.py"
SPEC = importlib.util.spec_from_file_location("agy_capabilities", PATH)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def test_discovery_ignores_stderr_when_stdout_contains_model_ids(tmp_path):
    executable = tmp_path / "agy"
    executable.write_text(
        "#!/usr/bin/env python3\n"
        "import sys; sys.stderr.write('provider warning\\n'); "
        "sys.stdout.write('gemini-3.1-pro-high\\ngemini-3.1-pro-low\\n')"
    )
    executable.chmod(0o700)
    output = tmp_path / "capabilities.json"

    assert MODULE.main(["--out", str(output), "--agy-bin", str(executable)]) == 0

    snapshot = json.loads(output.read_text())
    assert snapshot["models"] == {
        "gemini-3.1-pro": {
            "resolved_model": "gemini-3.1-pro",
            "supported_efforts": ["low", "high"],
            "dispatchable_ids": ["gemini-3.1-pro-high", "gemini-3.1-pro-low"],
        }
    }
    assert snapshot["effortless_models"] == []


def test_discovery_accepts_current_tab_separated_model_listing():
    snapshot = MODULE.normalize(
        "gemini-3.7-flash-high\tGemini 3.7 Flash (High)\n"
        "gemini-3.7-flash-medium\tGemini 3.7 Flash (Medium)\n"
        "gemini-3.7-flash-low\tGemini 3.7 Flash (Low)\n"
        "claude-opus-4-6-thinking\tClaude Opus 4.6 (Thinking)\n"
    )

    assert snapshot["models"]["gemini-3.7-flash"] == {
        "resolved_model": "gemini-3.7-flash",
        "supported_efforts": ["low", "medium", "high"],
        "dispatchable_ids": [
            "gemini-3.7-flash-high",
            "gemini-3.7-flash-low",
            "gemini-3.7-flash-medium",
        ],
    }
    assert snapshot["effortless_models"] == ["claude-opus-4-6-thinking"]


def test_nonzero_exit_reports_stderr(tmp_path, capsys):
    executable = tmp_path / "agy"
    executable.write_text(
        "#!/usr/bin/env python3\n"
        "import sys; sys.stderr.write('agy failed\\n'); sys.exit(7)"
    )
    executable.chmod(0o700)

    assert MODULE.main(["--agy-bin", str(executable)]) == 1
    assert "agy models exited 7: agy failed" in capsys.readouterr().err
