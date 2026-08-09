import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace

import pytest


ROOT = Path(__file__).resolve().parents[1]
PATH = ROOT / "skills" / "orchestrate" / "scripts" / "codex_capabilities.py"
SPEC = importlib.util.spec_from_file_location("codex_capabilities", PATH)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def test_discovery_ignores_stderr_when_stdout_contains_valid_catalogue(tmp_path):
    executable = tmp_path / "codex"
    executable.write_text(
        "#!/usr/bin/env python3\n"
        "import sys; sys.stderr.write('provider warning\\n'); "
        "sys.stdout.write('{\"models\":[{\"slug\":\"gpt-5.6-sol\","
        "\"supported_reasoning_levels\":[{\"effort\":\"high\"}]}]}')"
    )
    executable.chmod(0o700)
    output = tmp_path / "capabilities.json"

    assert MODULE.main(["--out", str(output), "--codex-bin", str(executable)]) == 0

    snapshot = json.loads(output.read_text())
    assert snapshot["models"] == {
        "gpt-5.6-sol": {
            "resolved_model": "gpt-5.6-sol",
            "supported_efforts": ["high"],
        }
    }


def test_nonzero_exit_reports_stderr(tmp_path, capsys):
    executable = tmp_path / "codex"
    executable.write_text(
        "#!/usr/bin/env python3\n"
        "import sys; sys.stderr.write('codex failed\\n'); sys.exit(7)"
    )
    executable.chmod(0o700)

    assert MODULE.main(["--codex-bin", str(executable)]) == 1
    assert "codex debug models exited 7: codex failed" in capsys.readouterr().err


def test_normalize_keeps_model_specific_efforts_only():
    value = MODULE.normalize({"models": [
        {"slug": "gpt-5.6-sol", "supported_reasoning_levels": [{"effort": "ultra"}, {"effort": "max"}]},
        {"slug": "gpt-5.6-luna", "supported_reasoning_levels": [{"effort": "max"}]},
    ]})
    assert value["models"]["gpt-5.6-sol"]["supported_efforts"] == ["ultra", "max"]
    assert value["models"]["gpt-5.6-luna"]["supported_efforts"] == ["max"]


def test_normalize_rejects_empty_or_malformed_catalogue():
    for value in ({}, {"models": []}, {"models": [{"display_name": "missing slug"}]}):
        try:
            MODULE.normalize(value)
        except ValueError:
            pass
        else:
            raise AssertionError("expected malformed catalogue to fail")


@pytest.mark.parametrize(
    "malformed",
    [
        "not a model entry",
        {"slug": 7, "supported_reasoning_levels": []},
        {"slug": "", "supported_reasoning_levels": []},
        {"slug": "gpt-malformed"},
        {"slug": "gpt-malformed", "supported_reasoning_levels": {}},
        {"slug": "gpt-malformed", "supported_reasoning_levels": ["high"]},
        {"slug": "gpt-malformed", "supported_reasoning_levels": [{"effort": 7}]},
        {"slug": "gpt-malformed", "supported_reasoning_levels": [{"effort": " "}]},
    ],
)
def test_normalize_rejects_entire_mixed_payload_for_any_malformed_entry(malformed):
    raw = {
        "models": [
            {
                "slug": "gpt-5.6-sol",
                "supported_reasoning_levels": [{"effort": "high"}],
            },
            malformed,
        ]
    }
    with pytest.raises(ValueError):
        MODULE.normalize(raw)


@pytest.mark.parametrize(
    "catalogue",
    [
        {"models": [{"slug": "gpt-empty", "supported_reasoning_levels": []}]},
        {
            "models": [
                {"slug": "GPT-Duplicate", "supported_reasoning_levels": [{"effort": "high"}]},
                {"slug": "gpt-duplicate", "supported_reasoning_levels": [{"effort": "max"}]},
            ]
        },
    ],
)
def test_normalize_rejects_empty_effort_sets_and_casefolded_duplicate_slugs(catalogue):
    with pytest.raises(ValueError):
        MODULE.normalize(catalogue)


@pytest.mark.parametrize(
    "raw",
    [
        '{"models":[{"slug":"gpt-a","supported_reasoning_levels":[{"effort":"high"}]}],'
        '"models":[{"slug":"gpt-b","supported_reasoning_levels":[{"effort":"max"}]}]}',
        '{"models":[{"slug":"gpt-a","slug":"gpt-b",'
        '"supported_reasoning_levels":[{"effort":"high"}]}]}',
        '{"models":[{"slug":"gpt-a","supported_reasoning_levels":[{"effort":"high"}],'
        '"supported_reasoning_levels":[{"effort":"max"}]}]}',
        '{"models":[{"slug":"gpt-a",'
        '"supported_reasoning_levels":[{"effort":"high","effort":"max"}]}]}',
    ],
)
def test_discovery_rejects_duplicate_json_members_before_normalization(tmp_path, monkeypatch, raw):
    result = SimpleNamespace(
        returncode=0,
        output=raw,
        stdout=raw,
        stderr="",
        timed_out=False,
    )
    monkeypatch.setattr(MODULE, "run_bounded", lambda *args, **kwargs: result)
    output = tmp_path / "capabilities.json"
    assert MODULE.main(["--out", str(output)]) == 1
    assert not output.exists()
