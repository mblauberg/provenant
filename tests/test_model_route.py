import json
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timedelta, timezone
import io
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
from contextlib import redirect_stderr, redirect_stdout
from unittest.mock import patch

from tests.support.modules import load_script

import pytest
import yaml


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "model-route"
CATALOG = json.loads((ROOT / "config" / "model-routing.json").read_text())


@pytest.fixture(autouse=True)
def default_test_identity(tmp_path, monkeypatch):
    monkeypatch.setenv('FABRIC_COOLDOWNS_PATH', str(tmp_path / 'cooldowns.json'))
    # Legacy cooldown fixtures belong to the default identity, not the caller's lane.
    for name in ('CLAUDE_CONFIG_DIR', 'CODEX_HOME', 'XDG_DATA_HOME'):
        monkeypatch.delenv(name, raising=False)


def test_free_opencode_models_are_derived_from_the_free_pattern():
    router = load_router()
    adapter = router.catalogue_snapshot()["adapters"]["opencode"]
    assert adapter["free_pattern"]
    for model in adapter["models"]:
        free = router.is_free_model(adapter, model["id"])
        training = router.model_route_live.training_flag(adapter, model)
        # Free ids default to training unless flagged zero-retention; paid ids train only when flagged.
        assert training is ((model.get("trains_on_prompts") is not False) if free
                            else model.get("trains_on_prompts") is True)
        assert (model["plan_cap_usd"] == 0) is free
        assert bool(model.get("warning")) is training
    fixture = {"free_pattern": "-free$", "models": [{"id": "opencode/rotating-free"}]}
    router._apply_free_pattern(fixture)
    assert fixture["models"][0]["trains_on_prompts"] is True and fixture["models"][0]["plan_cap_usd"] == 0
    assert fixture["models"][0]["warning"]
    explicit = {"free_pattern": "-free$", "models": [{"id": "opencode/zero-free", "trains_on_prompts": False}]}
    router._apply_free_pattern(explicit)
    assert explicit["models"][0]["trains_on_prompts"] is False and "warning" not in explicit["models"][0]
    assert explicit["models"][0]["plan_cap_usd"] == 0


def test_required_task_classes_bind_registered_exact_models_and_supported_efforts():
    required = {"mechanical", "legwork", "implementation", "ui-taste",
                "screenshots", "review", "second-opinion", "research", "bulk",
                "critical-review", "orchestration"}
    routes = CATALOG["task_class_routes"]
    assert required <= routes.keys()
    for name in required:
        route = routes[name]
        assert isinstance(route["models"], dict) and route["models"]
        for adapter, models in route["models"].items():
            entries = {item["id"]: item for item in CATALOG["adapters"][adapter]["models"]}
            for model in models:
                assert model in entries
                assert route["effort"] in entries[model].get("efforts", [])
                assert entries[model].get("effort_transport") != "none"


def test_claude_sonnet_is_the_senior_default_and_opus_remains_principal():
    claude = CATALOG["adapters"]["claude"]
    sonnet = next(model for model in claude["models"] if model["id"] == "claude-sonnet-5-5")
    opus = next(model for model in claude["models"] if model["id"] == "claude-opus-5-5")
    assert sonnet["default"] is True
    assert {"sonnet", "claude-sonnet-5-5", "claude-sonnet-5"} <= set(sonnet["names"])
    assert opus["default"] is False
    anthropic = CATALOG["families"]["anthropic"]
    assert anthropic["aliases"]["senior"] == ["sonnet", "opus"]
    assert anthropic["aliases"]["principal"] == ["opus"]
    assert anthropic["role_overrides"] == {"critical-review": {"principal": ["opus"]}}
    assert CATALOG["task_class_routes"]["review"]["models"]["claude"] == [
        "claude-sonnet-5-5", "claude-opus-5-5"
    ]
    assert CATALOG["task_class_routes"]["critical-review"]["models"]["claude"] == [
        "claude-opus-5-5"
    ]
    for task_class in ("implementation", "research", "screenshots", "second-opinion", "ui-taste"):
        assert CATALOG["task_class_routes"][task_class]["models"]["claude"][0] == "claude-sonnet-5-5"
    for task_class in ("critical-review", "orchestration"):
        assert CATALOG["task_class_routes"][task_class]["models"]["claude"][0] == "claude-opus-5-5"
    assert CATALOG["task_class_routes"]["implementation"]["models"]["codex"] == [
        "gpt-6.1-sol", "gpt-6-luna"
    ]
    assert CATALOG["families"]["openai"]["aliases"]["senior"] == ["gpt-6.1-sol", "gpt-6-luna"]
    assert CATALOG["families"]["openai"]["aliases"]["junior"] == ["gpt-6-luna"]
    result, route = resolve("--adapter", "claude", "--alias", "senior", "--role", "worker")
    assert result.returncode == 0, result.stderr
    assert route["resolved_model"] == "sonnet"


def test_task_classification_is_advisory_and_role_is_only_a_label():
    with patch.dict(os.environ, {"PROVENANT_TASK_BRIEF": ""}):
        result, route = resolve("--adapter", "claude", "--task-class", "review", "--role", "worker")
    assert route["status"] != "task_class_role_mismatch"
    assert route["role"] == "worker" and route["task_class"] == "review"
    with patch.dict(os.environ, {"PROVENANT_TASK_BRIEF": ""}):
        result, route = resolve("--adapter", "claude", "--task-class", "review")
    assert route["status"] != "task_class_role_mismatch"
    assert route["role"] == "worker" and any("missing role" in item for item in route["warnings"])


def test_task_classification_uses_configured_rules_from_brief(monkeypatch):
    router = load_router()
    with patch.dict(os.environ, {"PROVENANT_TASK_BRIEF": "audit this repository", "PROVENANT_TASK_MODE": "read_only"}):
        result, route = resolve("--adapter", "claude", "--role", "worker")
    assert route["status"] != "task_class_role_mismatch"
    assert route["task_class"] == "review" and route["classified_by"] == "rules"


def test_advisory_classification_keeps_an_explicit_alias_route(monkeypatch, capsys):
    router = load_router()
    with patch.dict(os.environ, {"PROVENANT_TASK_BRIEF": "audit this", "PROVENANT_TASK_MODE": "read_only"}):
        code = router.main(["resolve", "--adapter", "claude", "--alias", "senior", "--role", "worker"])
    route = json.loads(capsys.readouterr().out)
    assert code == 0 and route["status"] == "ok"
    assert route["alias"] == "senior" and route["task_class"] == "review"
    assert route["classified_by"] == "rules"


def test_task_size_can_raise_the_configured_class_tier(monkeypatch, tmp_path, capsys):
    router = load_router()
    catalog = product_catalog()
    catalog_path = use_catalog(router, monkeypatch, tmp_path, catalog)
    monkeypatch.setenv("PROVENANT_TASK_SIZE_FILES", "30")
    code = router.main(["resolve", "--adapter", "claude", "--task-class", "mechanical", "--role", "worker"])
    route = json.loads(capsys.readouterr().out)
    assert route["alias"] == "principal" and route["requested_effort"] == "high"
    assert route["size_tier"] == "large" and code in (0, 1)


def test_fallback_inherits_adapter_training_flag_without_false_default():
    module = load_script(ROOT / "scripts/model_route.py", "route_under_test")
    catalog = {"adapters": {"fixture": {"trains_on_prompts": True,
                "aliases": {"senior": ["primary", "candidate"]},
                "models": [{"id": "primary", "plan_cap_usd": 1},
                           {"id": "candidate", "plan_cap_usd": 1}]}}}
    assert module.model_route_live._fallback_candidates("fixture", "senior", "primary", catalog, {}) == []
    catalog["adapters"]["fixture"]["trains_on_prompts"] = False
    candidates = module.model_route_live._fallback_candidates("fixture", "senior", "primary", catalog, {})
    assert len(candidates) == 1
    assert candidates[0]["trains_on_prompts"] is False
    del catalog["adapters"]["fixture"]["trains_on_prompts"]
    candidates = module.model_route_live._fallback_candidates("fixture", "senior", "primary", catalog, {})
    assert len(candidates) == 1
    assert candidates[0]["trains_on_prompts"] is None
CRUCIAL_RISK_OVERRIDE = CATALOG["families"]["anthropic"]["risk_tier_overrides"]["crucial"]
RISK_OVERRIDE_MODEL = CRUCIAL_RISK_OVERRIDE["models"][0]
NON_OCCUPANT_MODELS = tuple(dict.fromkeys(
    model
    for models in CATALOG["families"]["anthropic"]["aliases"].values()
    for model in models
    if RISK_OVERRIDE_MODEL.casefold() not in model.casefold()
))


@pytest.fixture
def harness_script(tmp_path):
    script = tmp_path / "run-harness.sh"
    script.write_text(
        "#!/usr/bin/env bash\n"
        "set -euo pipefail\n"
        f"source {ROOT / 'scripts' / 'lib' / 'harness-python.sh'}\n"
        '"$HARNESS_RUNNER" -c "import sys; print(sys.version_info[:2])"\n'
    )
    script.chmod(0o755)
    return script


def run_harness(harness_script, runner, *, harness_python, pythonpath, optimize):
    environment = {
        **os.environ,
        "HARNESS_PYTHON": harness_python,
        "HARNESS_RUNNER": runner,
        "PYTHONPATH": str(pythonpath) if pythonpath is not None else "",
        "PYTHONOPTIMIZE": optimize,
    }
    return subprocess.run(
        [str(harness_script)],
        cwd=ROOT,
        env=environment,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


@pytest.mark.parametrize("runner", ["run_stdlib", "run_yaml", "run_test"])
def test_python_floor_rejects_wrong_version_under_optimization(
    tmp_path, harness_script, runner,
):
    version_shim = tmp_path / "python-version-shim"
    version_shim.mkdir()
    (version_shim / "sitecustomize.py").write_text(
        "import sys\n"
        "import pytest\n"
        "import yaml\n"
        "sys.version_info = (3, 9, 6, 'final', 0)\n"
    )

    result = run_harness(
        harness_script,
        runner,
        harness_python=sys.executable,
        pythonpath=version_shim,
        optimize="1",
    )

    if result.returncode != 3:
        pytest.fail(f"expected probe refusal, got exit {result.returncode}: {result.stdout}")
    if result.stdout:
        pytest.fail(f"probe refusal wrote stdout: {result.stdout}")
    if "harness-python: interpreter probe failed" not in result.stderr:
        pytest.fail(f"missing probe refusal detail: {result.stderr}")


@pytest.mark.parametrize("runner", ["run_stdlib", "run_yaml", "run_test"])
def test_python_floor_rejects_missing_interpreter(harness_script, runner):
    result = run_harness(
        harness_script,
        runner,
        harness_python="/deliberately/missing/python",
        pythonpath=None,
        optimize="1",
    )

    if result.returncode != 3:
        pytest.fail(f"expected unusable-interpreter refusal, got exit {result.returncode}")
    if result.stdout:
        pytest.fail(f"unusable-interpreter refusal wrote stdout: {result.stdout}")
    if "harness-python: unusable interpreter" not in result.stderr:
        pytest.fail(f"missing unusable-interpreter detail: {result.stderr}")


@pytest.mark.parametrize("runner", ["run_stdlib", "run_yaml", "run_test"])
@pytest.mark.parametrize("variable", ["HARNESS_PYTHON", "AGENT_FABRIC_PRODUCT_ROOT"])
def test_python_rejects_relative_override(harness_script, runner, variable):
    environment = {**os.environ, "HARNESS_RUNNER": runner, "HARNESS_PYTHON": "",
        variable: "relative/path"}
    result = subprocess.run([str(harness_script)], cwd=ROOT, env=environment,
        text=True, capture_output=True, check=False)

    assert result.returncode == 3
    assert not result.stdout
    assert f"{variable} must be an absolute path" in result.stderr
    assert "repair:" in result.stderr


def resolve(*args):
    arguments = [str(ROOT / "scripts" / "model_route.py"), "resolve", *args]
    # The router reads its catalogue from the instance root, which defaults to
    # ~/.agents. Left unpinned these cases route against whatever catalogue the
    # developer happens to have installed, so they pass or fail on the machine
    # rather than on the repository. CI already pins the same variable.
    environment = {"PATH": "", "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT),
                   "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT)}
    stdout, stderr = io.StringIO(), io.StringIO()
    with patch.dict(os.environ, environment), redirect_stdout(stdout), redirect_stderr(stderr):
        result = load_router().main(["resolve", *args])
    result = subprocess.CompletedProcess(arguments, result, stdout.getvalue(), stderr.getvalue())
    return result, json.loads(result.stdout) if result.stdout else None


def test_snapshot_deep_merges_models_and_drops_malformed_overlay(tmp_path):
    product = tmp_path / "product"
    instance = tmp_path / "instance"
    (product / "config").mkdir(parents=True)
    (instance / "config").mkdir(parents=True)
    shutil.copy(ROOT / "config" / "model-routing.json", product / "config" / "model-routing.json")
    (instance / "config" / "model-routing.json").write_text(json.dumps({
        "adapters": {"codex": {"models": [
            {"id": "gpt-6-luna", "names": ["moon"]},
            {"id": 4, "names": ["bad"]},
            {"id": "bad-new", "names": 5},
        ]}, "broken": {"models": "not-a-list"},
        "also-broken": {"endpoint_provider": "new", "fixed_model_family": None,
                        "effort_transport": "flag", "models": [{"names": ["oops"]}]}},
        "families": {"bad-family": {"aliases": "not-a-map"}},
        "endpoints": {"bad": {"token_env": 3}},
        "task_class_routes": {"implementation": {
            "effort": "xhigh", "models": {"codex": ["gpt-6-astra"]},
        }},
    }))
    result = subprocess.run(
        [str(SCRIPT), "snapshot", "--json"], capture_output=True, text=True,
        env={**os.environ, "AGENT_FABRIC_PRODUCT_ROOT": str(product),
             "AGENT_FABRIC_INSTANCE_ROOT": str(instance),
             "AGENT_FABRIC_STATE_ROOT": str(tmp_path / "state"),
             "HARNESS_PYTHON": sys.executable},
    )
    assert result.returncode == 0, result.stderr
    snapshot = json.loads(result.stdout)
    assert snapshot["schema"] == "fabric.catalogue.v1"
    assert snapshot["sha256"]
    assert len(snapshot["sources"]) == 2
    luna = next(model for model in snapshot["adapters"]["codex"]["models"] if model["id"] == "gpt-6-luna")
    assert "moon" in luna["names"]
    assert "luna" in luna["names"]
    assert "bad-new" not in [model["id"] for model in snapshot["adapters"]["codex"]["models"]]
    assert "broken" not in snapshot["adapters"]
    assert "also-broken" not in snapshot["adapters"]
    assert "bad-family" not in snapshot["families"]
    assert "bad" not in snapshot["endpoints"]
    assert snapshot["task_class_routes"]["implementation"]["effort"] == "xhigh"
    assert snapshot["task_class_routes"]["implementation"]["models"]["codex"] == ["gpt-6-astra"]
    assert snapshot["adapters"]["codex"]["default_model"] if "default_model" in snapshot["adapters"]["codex"] else True
    assert any("codex.models" in note for note in snapshot["drift"])


def test_training_flag_survives_model_overlay_and_free_override(tmp_path, monkeypatch):
    router = load_router()
    instance = tmp_path / "instance"
    (instance / "config").mkdir(parents=True)
    (instance / "config/model-routing.json").write_text(json.dumps({"adapters": {
        "claude": {"models": [{"id": "claude-opus-5-5", "names": ["overlay-opus"]}]},
        "codex": {"models": [{"id": "gpt-6.1-sol", "names": ["overlay-sol"]}]},
        "agy": {"models": [{"id": "gemini-3.8-flash", "names": ["overlay-flash"]}]},
        "opencode": {"models": [{"id": "opencode/mimo-v2.6-flash-free", "names": ["overlay-free"]}]},
    }}))
    monkeypatch.setattr(router, "CATALOG_PATH", instance / "config/model-routing.json")
    catalog = router.catalogue_snapshot()["catalogue"]
    for adapter, model in (("claude", "claude-opus-5-5"), ("codex", "gpt-6.1-sol"),
                           ("agy", "gemini-3.8-flash")):
        entry = next(item for item in catalog["adapters"][adapter]["models"] if item["id"] == model)
        assert router.model_route_live.training_flag(catalog["adapters"][adapter], entry) is False
    free = next(item for item in catalog["adapters"]["opencode"]["models"]
                if item["id"] == "opencode/mimo-v2.6-flash-free")
    assert router.model_route_live.training_flag(catalog["adapters"]["opencode"], free) is True
    assert router.model_route_live.training_flag({}, {}) is None


def test_resolved_routes_expose_training_flag():
    for adapter, model, expected in (("claude", "opus", False),
                                     ("opencode", "opencode/mimo-v2.6-flash-free", True)):
        result, route = resolve("--adapter", adapter, "--model", model, "--role", "worker")
        assert result.returncode == 0, result.stderr
        assert route["trains_on_prompts"] is expected


def test_snapshot_drops_invalid_new_adapter_effort_and_allows_nullable_override(tmp_path, monkeypatch):
    router = load_router()
    instance = tmp_path / "instance"
    (instance / "config").mkdir(parents=True)
    (instance / "config/model-routing.json").write_text(json.dumps({"adapters": {
        "kiro": {"fixed_model_family": "anthropic"},
        "pi": {"endpoint_provider": "pi", "fixed_model_family": None,
               "effort_transport": "flag", "models": [{"id": "foo", "efforts": ["bogus"]}]},
    }}))
    monkeypatch.setattr(router, "CATALOG_PATH", instance / "config/model-routing.json")
    snapshot = router.catalogue_snapshot()
    assert snapshot["adapters"]["kiro"]["fixed_model_family"] == "anthropic"
    assert "pi" not in snapshot["adapters"]
    assert any("pi" in note for note in snapshot["drift"])


def test_explicit_endpoint_and_newer_model_preserve_identity(monkeypatch):
    monkeypatch.setenv("OPENROUTER_API_KEY", "fixture")
    result, endpoint = resolve("--adapter", "claude", "--model", "custom-model",
                               "--alias", "senior", "--endpoint", "openrouter-anthropic", "--role", "worker")
    assert result.returncode == 0, result.stderr
    assert endpoint["endpoint_profile"] == "openrouter-anthropic"
    no_alias_result, no_alias = resolve("--adapter", "claude", "--model", "custom-model",
                                        "--endpoint", "openrouter-anthropic", "--role", "worker")
    assert no_alias_result.returncode == 0, no_alias_result.stderr
    assert no_alias["endpoint_profile"] == "openrouter-anthropic"
    assert not any("alias and model both supplied" in note for note in no_alias["notes"])
    result, newer = resolve("--adapter", "codex", "--model", "gpt-7-luna", "--role", "worker")
    assert result.returncode == 0, result.stderr
    assert newer["resolved_model"] == "gpt-7-luna"


def test_canonical_cooldown_skips_suffix_and_warns_on_shorthand(tmp_path):
    until = (datetime.now(timezone.utc) + timedelta(hours=1)).isoformat()
    (tmp_path / "cooldowns.json").write_text(json.dumps({"cooldowns": {
        "agy/gemini-3.8-flash-high": {"cooling_until": until},
        "claude/claude-opus-5-5": {"cooling_until": until},
    }}))
    env = {**os.environ, "HARNESS_PYTHON": sys.executable,
           "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT), "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT),
           "AGENT_FABRIC_STATE_ROOT": str(tmp_path)}
    def route(*args):
        run = subprocess.run([str(SCRIPT), "resolve", *args, "--role", "worker"],
                             capture_output=True, text=True, env=env)
        assert run.returncode == 0, run.stderr
        return json.loads(run.stdout)
    agy = route("--adapter", "agy", "--alias", "senior")
    assert not any(candidate["adapter"] == "agy" and candidate["model"].startswith("gemini-3.8-flash")
                   for candidate in agy["fallback_candidates"])
    agy_explicit = route("--adapter", "agy", "--model", "gemini-3.8-flash")
    assert any("cooling" in warning for warning in agy_explicit["warnings"])
    opus = route("--adapter", "claude", "--alias", "opus")
    assert opus["resolved_model"] == "claude-opus-5-5"
    assert opus["requested_model"] == "opus"
    assert opus["model_selection"] == "explicit"
    assert opus["fallback_candidates"] == []
    assert any("cooling" in warning for warning in opus["warnings"])


def test_cooling_alias_without_alternative_has_note(tmp_path):
    until = (datetime.now(timezone.utc) + timedelta(hours=1)).isoformat()
    (tmp_path / "cooldowns.json").write_text(json.dumps({"cooldowns": {
        "agy/gemini-3.8-flash": {"cooling_until": until},
    }}))
    run = subprocess.run(
        [sys.executable, str(ROOT / "scripts" / "model_route.py"), "resolve",
         "--adapter", "agy", "--alias", "senior", "--role", "worker"],
        capture_output=True, text=True,
        env={**os.environ, "PATH": str(tmp_path), "AGENT_FABRIC_STATE_ROOT": str(tmp_path),
             "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT), "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT)},
    )
    assert run.returncode == 0, run.stderr
    route = json.loads(run.stdout)
    assert any("cooling" in note and "no available alternative" in note for note in route["notes"])


def test_account_wide_cooldown_warns_on_explicit_model(tmp_path):
    until = (datetime.now(timezone.utc) + timedelta(hours=1)).isoformat()
    (tmp_path / "cooldowns.json").write_text(json.dumps({"cooldowns": {
        "codex/*": {"cooling_until": until},
    }}))
    run = subprocess.run([str(SCRIPT), "resolve", "--adapter", "codex", "--model", "gpt-6.1-sol",
                          "--role", "worker"], capture_output=True, text=True,
                         env={**os.environ, "HARNESS_PYTHON": sys.executable,
                              "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT), "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT),
                              "AGENT_FABRIC_STATE_ROOT": str(tmp_path)})
    assert run.returncode == 0, run.stderr
    assert any("cooling" in warning for warning in json.loads(run.stdout)["warnings"])


def test_fallback_false_and_default_explicit_routes_have_no_candidates():
    for args in (("--alias", "senior", "--fallback", "false"),
                 ("--model", "gpt-6.1-sol", "--fallback", "false"),
                 ("--model", "gpt-6.1-sol")):
        result, route = resolve("--adapter", "codex", *args, "--role", "worker")
        assert result.returncode == 0, result.stderr
        assert route["fallback_candidates"] == []


def test_fixed_adapter_cross_family_is_warned_and_default_cooling_does_not_crash(tmp_path):
    result, wrong = resolve("--adapter", "codex", "--model", "claude-opus-5-5", "--role", "worker")
    assert result.returncode == 0, result.stderr
    assert any("claude" in warning and "fix:" in warning for warning in wrong["warnings"])
    until = (datetime.now(timezone.utc) + timedelta(hours=1)).isoformat()
    (tmp_path / "cooldowns.json").write_text(json.dumps({"cooldowns": {
        "opencode/opencode-go/deepseek-v4.1-flash": {"cooling_until": until},
    }}))
    run = subprocess.run([str(SCRIPT), "resolve", "--adapter", "opencode", "--alias", "principal",
                          "--task-class", "critical-review", "--role", "worker"],
                         capture_output=True, text=True,
                         env={**os.environ, "HARNESS_PYTHON": sys.executable,
                              "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT),
                              "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT),
                              "AGENT_FABRIC_STATE_ROOT": str(tmp_path)})
    assert run.returncode != 1 or "NameError" not in run.stderr


def test_kiro_is_enabled_for_both_routes():
    compatibility = yaml.safe_load((ROOT / "config/adapter-compatibility.yaml").read_text())
    assert compatibility["dispatch_registry"]["kiro"]["write_modes"] == ["worktree_write"]
    assert compatibility["dispatch_registry"]["kiro"]["read_only_guarantee"] == "prompt_only"
    result, route = resolve("--adapter", "kiro", "--model", "deepseek-3.2",
                            "--alias", "junior", "--role", "worker")
    assert result.returncode == 0, route
    assert route["adapter_enabled"] is True
    assert route["status"] == "ok"


def test_agy_write_mode_is_registered():
    compatibility = yaml.safe_load((ROOT / "config/adapter-compatibility.yaml").read_text())
    registry = compatibility["dispatch_registry"]["agy"]
    assert registry["write_modes"] == ["worktree_write"]
    assert registry["read_only_guarantee"] == "prompt_only"


def test_kiro_route_carries_cached_negative_probe_evidence(tmp_path):
    cli = tmp_path / "kiro-cli"
    cli.write_text("#!/bin/sh\necho 1.2.3\n")
    cli.chmod(0o755)
    checked = datetime.now(timezone.utc).isoformat()
    negative = {"cli_version": "1.2.3", "checked_at": checked,
                "attempted_write": True, "permission_denied": True, "file_created": False}
    (tmp_path / "capabilities.json").write_text(json.dumps({"kiro": {
        "version": "1.2.3", "executable": str(cli),
        "observed_at": checked, "read_only_probe": negative,
    }}))
    run = subprocess.run([sys.executable, str(ROOT / "scripts" / "model_route.py"),
                          "resolve", "--adapter", "kiro", "--alias", "junior",
                          "--model", "deepseek-3.2", "--role", "worker"], capture_output=True, text=True,
                         env={**os.environ, "PATH": str(tmp_path),
                              "AGENT_FABRIC_STATE_ROOT": str(tmp_path),
                              "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT), "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT)})
    assert run.returncode == 0, run.stderr
    route = json.loads(run.stdout)
    assert route["cli_version"] == "1.2.3"
    assert route["read_only_probe"] == negative


def test_kiro_route_discards_probe_after_cli_version_changes(tmp_path):
    cli = tmp_path / "kiro-cli"
    cli.write_text("#!/bin/sh\necho 1.2.4\n")
    cli.chmod(0o755)
    checked = datetime.now(timezone.utc).isoformat()
    (tmp_path / "capabilities.json").write_text(json.dumps({"kiro": {
        "version": "1.2.3", "executable": str(cli), "observed_at": checked,
        "read_only_probe": {"cli_version": "1.2.3", "checked_at": checked,
                            "attempted_write": True, "permission_denied": True,
                            "file_created": False},
    }}))
    run = subprocess.run(
        [sys.executable, str(ROOT / "scripts" / "model_route.py"), "resolve",
         "--adapter", "kiro", "--alias", "junior", "--model", "deepseek-3.2", "--role", "worker"],
        capture_output=True, text=True,
        env={**os.environ, "PATH": str(tmp_path), "AGENT_FABRIC_STATE_ROOT": str(tmp_path),
             "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT), "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT)},
    )
    assert run.returncode == 0, run.stderr
    route = json.loads(run.stdout)
    assert route.get("cli_version") != "1.2.3"
    assert "read_only_probe" not in route


def test_kiro_capability_probe_records_observed_write_denial(tmp_path):
    cli = tmp_path / "kiro-cli"
    cli.write_text("""#!/usr/bin/env python3
import json, sys
args = sys.argv[1:]
if args == ['--version']:
    print('1.2.3')
elif '--list-models' in args:
    print(json.dumps({'models': ['auto']}))
elif args == ['--help']:
    print('--trust-tools')
else:
    target = args[-1].split('FILE=', 1)[1].split()[0]
    print(json.dumps({'jsonrpc': '2.0', 'params': {'update': {
        'sessionUpdate': 'tool_call', 'toolCallId': 'write-1',
        'title': 'fs_write', 'rawInput': {'path': target}}}}))
    print(json.dumps({'jsonrpc': '2.0', 'params': {'update': {
        'sessionUpdate': 'tool_call_update', 'toolCallId': 'write-1',
        'status': 'failed', 'content': [{'type': 'text', 'text': 'permission denied'}]}}}))
""")
    cli.chmod(0o755)
    run = subprocess.run([str(SCRIPT), "probe", "--adapter", "kiro", "--executable", str(cli), "--json"],
                         capture_output=True, text=True,
                         env={**os.environ, "HARNESS_PYTHON": sys.executable,
                              "AGENT_FABRIC_STATE_ROOT": str(tmp_path)})
    assert run.returncode == 0, run.stderr
    evidence = json.loads(run.stdout)["read_only_probe"]
    assert evidence["attempted_write"] is True
    assert evidence["permission_denied"] is True
    assert evidence["file_created"] is False


def test_probe_bare_executable_records_binary_found_on_path(tmp_path, monkeypatch):
    router = load_router()
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path))
    monkeypatch.setenv("PATH", str(tmp_path))
    cli = tmp_path / "kiro-cli"
    cli.write_text("#!/bin/sh\nif [ \"$1\" = --version ]; then echo 1.2.3; exit; fi\n"
                   "if [ \"$1\" = --help ]; then exit; fi\necho '{\"models\":[\"auto\"]}'\n")
    cli.chmod(0o755)
    record, code = router.model_route_probe.probe_capabilities("kiro", "kiro-cli")
    assert code == 0
    assert record["executable"] == str(cli)


def test_explicit_kiro_probe_allows_model_response_budget(tmp_path, monkeypatch):
    router = load_router()
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path))
    budgets = []

    def fake_run(argv, **kwargs):
        if "--no-interactive" in argv:
            budgets.append(kwargs["timeout"])
        output = "1.2.3" if argv[-1] == "--version" else '{"models":["auto"]}'
        return subprocess.CompletedProcess(argv, 0, output, "")

    monkeypatch.setattr(router.subprocess, "run", fake_run)
    record, code = router.model_route_probe.probe_capabilities("kiro", "/fake/kiro-cli")
    assert code == 0
    assert "read_only_probe" in record
    assert budgets == [60.0]


def test_compatibility_drift_rejection_names_a_fix(tmp_path):
    router = load_router()
    catalog = product_catalog()
    catalog["adapters"]["codex"].pop("default_model", None)
    path = tmp_path / "model-routing.json"
    path.write_text(json.dumps(catalog))
    compatibility = write_codex_compatibility(tmp_path, requires_explicit_model=False)
    result = subprocess.run([str(SCRIPT), "resolve", "--adapter", "codex", "--alias",
                             "senior", "--role", "worker", "--catalog", str(path),
                             "--adapter-compatibility", str(compatibility)],
                            capture_output=True, text=True,
                            env={**os.environ, "HARNESS_PYTHON": sys.executable,
                                 "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT),
                                 "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT)})
    route = json.loads(result.stdout)
    assert route["status"] == "account_default_conflicts_with_compatibility"
    assert "fix:" in route["message"]


@pytest.mark.parametrize("arguments,model,effort", [
    (["--adapter", "codex", "--alias", "luna"], "gpt-6-luna", ""),
    (["--adapter", "agy", "--alias", "flash", "--effort", "xhigh"], "gemini-3.8-flash-high", "high"),
    (["--adapter", "cursor", "--alias", "grok"], "grok-4.7", ""),
    (["--adapter", "opencode", "--alias", "glm"], "opencode/glm-5.3-flash-free", ""),
    (["--adapter", "opencode", "--alias", "glm-paid"], "opencode-go/glm-5.3-flash", ""),
    (["--model", "gpt-5.6-luna"], "gpt-6-luna", ""),
])
def test_ordinary_registry_routes_names_and_nearest_effort(arguments, model, effort):
    result, route = resolve(*arguments, "--role", "worker")
    assert result.returncode == 0, route
    assert route["status"] == "ok"
    assert route["resolved_model"] == model
    assert route["effort_applied"] == effort


@pytest.mark.parametrize("model,asked,resolved,used", [
    ("sonnet", "low", "claude-sonnet-5-5", "medium"),
])
def test_tier_alias_clamps_effort_to_the_registry_efforts(model, asked, resolved, used):
    result, route = resolve("--adapter", "claude", "--alias", "senior", "--model", model,
                            "--effort", asked, "--role", "worker")
    assert result.returncode == 0, route
    assert route["status"] == "ok"
    assert route["resolved_model"] == resolved
    assert route["effort"] == used
    assert route["effort_substitution"] == f"{asked} unsupported by {resolved}; ran at {used}"
    assert asked not in route["supported_efforts"]


def test_ordinary_unknown_model_passes_through_and_conflict_prefers_model():
    result, route = resolve("--adapter", "opencode", "--alias", "junior", "--model",
                            "example/new-model", "--role", "worker")
    assert result.returncode == 0, route
    assert route["resolved_model"] == "example/new-model"
    assert route["model_family"] == "generic-open"
    assert route["provider"] == "example"
    assert route["notes"]


def test_unregistered_agy_model_passes_explicit_effort_unverified():
    result, route = resolve("--adapter", "agy", "--model", "gemini-3.8-pro",
                            "--effort", "medium", "--role", "worker")
    assert result.returncode == 0, route
    assert route["effort"] == route["effort_applied"] == "medium"
    assert route["effort_capability_source"] == "provider-unverified"
    assert route["notes"][0] == (
        "gemini-3.8-pro is not in the agy registry "
        "(registered: gemini-3.8-flash, claude-opus-4-6-thinking, claude-sonnet-4-6; "
        "closest: gemini-3.8-flash); "
        "passed through as given"
    )


def test_opencode_training_warning_and_paid_fallback_excludes_free():
    free_models = [item["id"] for item in CATALOG["adapters"]["opencode"]["models"]
                   if item["id"].endswith("-free")]
    assert free_models
    for model in free_models:
        result, free_route = resolve("--adapter", "opencode", "--model", model, "--role", "worker")
        assert result.returncode == 0, free_route
        zero_retention = next(item for item in CATALOG["adapters"]["opencode"]["models"]
                              if item["id"] == model).get("trains_on_prompts") is False
        assert free_route["trains_on_prompts"] is (not zero_retention)
        assert bool(free_route["warnings"]) is (not zero_retention)
    result, route = resolve("--adapter", "opencode", "--model",
                            "opencode/mimo-v2.6-flash-free", "--role", "worker")
    assert result.returncode == 0, route
    assert route["warnings"]
    result, route = resolve("--adapter", "opencode", "--alias", "senior", "--role", "worker")
    assert result.returncode == 0, route
    assert [candidate["model"] for candidate in route["fallback_candidates"]] == [
        "opencode/glm-5.3-flash-free", "opencode-go/deepseek-v4.1-flash"]
    assert all(candidate["trains_on_prompts"] is False for candidate in route["fallback_candidates"])
    result, unregistered = resolve("--adapter", "opencode", "--model",
                                   "opencode/unregistered-contributor-free", "--role", "worker")
    assert result.returncode == 0, unregistered
    assert unregistered["trains_on_prompts"] is None
    assert unregistered["warnings"]


def test_opencode_has_no_principal_tier_and_says_so():
    assert "principal" not in CATALOG["adapters"]["opencode"]["aliases"]
    result, route = resolve("--adapter", "opencode", "--alias", "principal", "--role", "worker")
    assert result.returncode == 0, route
    assert route["resolved_model"] == CATALOG["adapters"]["opencode"]["default_model"]
    assert any("no principal tier" in warning for warning in route["warnings"])


def test_cooling_alias_skips_candidate_but_explicit_model_warns(tmp_path):
    until = (datetime.now(timezone.utc) + timedelta(hours=1)).isoformat()
    (tmp_path / "cooldowns.json").write_text(json.dumps({"schema": "fabric.cooldowns.v1", "cooldowns": {
        "opencode/opencode-go/glm-5.3-flash": {"cooling_until": until},
        "opencode/opencode/deepseek-v4.1-flash-free": {"cooling_until": until},
        "opencode/opencode/glm-5.3-flash-free": {"cooling_until": until},
    }}))
    env = {**os.environ, "HARNESS_PYTHON": sys.executable,
           "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT), "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT),
           "AGENT_FABRIC_STATE_ROOT": str(tmp_path)}
    def routed(*arguments):
        process = subprocess.run([str(SCRIPT), "resolve", *arguments, "--role", "worker"],
                                 capture_output=True, text=True, env=env)
        return process, json.loads(process.stdout)
    result, alias = routed("--adapter", "opencode", "--alias", "senior")
    assert result.returncode == 0
    assert alias["resolved_model"] == "opencode-go/deepseek-v4.1-flash"
    assert any("cooling" in note for note in alias["notes"])
    result, shorthand = routed("--adapter", "opencode", "--alias", "glm-paid")
    assert result.returncode == 0
    assert shorthand["resolved_model"] == "opencode-go/glm-5.3-flash"
    assert any("cooling" in warning for warning in shorthand["warnings"])
    result, explicit = routed("--adapter", "opencode", "--model", "glm-paid")
    assert result.returncode == 0
    assert explicit["resolved_model"] == "opencode-go/glm-5.3-flash"
    assert any("cooling" in warning for warning in explicit["warnings"])


def test_malformed_cooldown_time_cannot_break_routing(tmp_path):
    (tmp_path / "cooldowns.json").write_text(json.dumps({"cooldowns": {
        "opencode/opencode-go/glm-5.3-flash": {"cooling_until": datetime.now(timezone.utc).replace(tzinfo=None).isoformat()},
    }}))
    result = subprocess.run([str(SCRIPT), "resolve", "--adapter", "opencode", "--alias",
                             "principal", "--role", "worker"], capture_output=True, text=True,
                            env={**os.environ, "HARNESS_PYTHON": sys.executable,
                                 "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT),
                                 "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT),
                                 "AGENT_FABRIC_STATE_ROOT": str(tmp_path)})
    assert result.returncode == 0, result.stderr


def test_codex_senior_has_no_luna_fallback_when_sol_cools(tmp_path):
    until = (datetime.now(timezone.utc) + timedelta(hours=1)).isoformat()
    (tmp_path / "cooldowns.json").write_text(json.dumps({"cooldowns": {
        "codex/gpt-6.1-sol": {"cooling_until": until},
    }}))
    capability = write_codex_capability_snapshot(tmp_path)
    result = subprocess.run([str(SCRIPT), "resolve", "--adapter", "codex", "--alias",
                             "senior", "--role", "worker", "--capabilities-file", str(capability)],
                            capture_output=True, text=True,
                            env={**os.environ, "HARNESS_PYTHON": sys.executable,
                                 "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT),
                                 "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT),
                                 "AGENT_FABRIC_STATE_ROOT": str(tmp_path)})
    route = json.loads(result.stdout)
    assert result.returncode == 0, route
    assert route["resolved_model"] == "gpt-6-luna"
    assert any("cooling" in note for note in route["notes"])
    assert any(candidate["adapter"] == "opencode" for candidate in route["fallback_candidates"])


def test_ordinary_tier_maps_unsupported_effort_to_nearest_probed_level(tmp_path):
    capability = write_codex_capability_snapshot(tmp_path, models={
        "gpt-6-luna": {"resolved_model": "gpt-6-luna", "supported_efforts": ["low", "high"]},
    })
    result, route = resolve("--adapter", "codex", "--alias", "junior", "--role", "worker",
                            "--effort", "xhigh", "--capabilities-file", str(capability))
    assert result.returncode == 0, route
    assert route["effort"] == route["effort_applied"] == "high"
    assert route["effort_note"]


def test_unknown_effort_on_tier_routes_as_nearest_supported(tmp_path):
    capability = write_codex_capability_snapshot(tmp_path, models={
        "gpt-6-luna": {"resolved_model": "gpt-6-luna", "supported_efforts": ["low", "high"]},
    })
    result, route = resolve("--adapter", "codex", "--alias", "junior", "--role", "worker",
                            "--effort", "balanced", "--capabilities-file", str(capability))
    assert result.returncode == 0, route
    assert route["requested_effort"] == "balanced"
    assert route["effort_applied"] == "low"
    assert route["effort_note"]


def test_tier_alias_and_explicit_model_keeps_model_with_note(tmp_path):
    capability = write_codex_capability_snapshot(tmp_path)
    result, route = resolve("--adapter", "codex", "--alias", "junior", "--model",
                            "gpt-6.1-sol", "--role", "worker", "--capabilities-file", str(capability))
    assert result.returncode == 0, route
    assert route["resolved_model"] == "gpt-6.1-sol"
    assert any("model won" in note for note in route["notes"])


def test_fallback_any_may_include_free_and_training_models():
    result, route = resolve("--adapter", "opencode", "--alias", "principal",
                            "--fallback", "any", "--role", "worker")
    assert result.returncode == 0, route
    assert any(candidate["plan_cap_usd"] == 0 for candidate in route["fallback_candidates"])
    assert any(candidate["trains_on_prompts"] for candidate in route["fallback_candidates"])


def test_capability_probe_caches_model_list_by_cli_version(tmp_path):
    cli = tmp_path / "opencode"
    calls = tmp_path / "calls"
    cli.write_text("#!/bin/sh\nif [ \"$1\" = --version ]; then echo 1.2.3; exit; fi\n"
                   "if [ \"$1\" = --help ]; then echo --variant; exit; fi\n"
                   f"echo call >> '{calls}'\necho opencode-go/deepseek-v4.1-flash\n")
    cli.chmod(0o755)
    env = {**os.environ, "HARNESS_PYTHON": sys.executable,
           "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT), "AGENT_FABRIC_STATE_ROOT": str(tmp_path)}
    def probe():
        result = subprocess.run([str(SCRIPT), "probe", "--adapter", "opencode",
                                 "--executable", str(cli), "--json"],
                                capture_output=True, text=True, env=env)
        return result, json.loads(result.stdout)
    first, observed = probe()
    second, cached = probe()
    assert first.returncode == second.returncode == 0
    assert observed["models"] == ["opencode-go/deepseek-v4.1-flash"]
    assert observed["version"] == "1.2.3"
    assert cached["cache_hit"] is True
    assert calls.read_text().splitlines() == ["call"]
    cli.write_text("#!/bin/sh\nif [ \"$1\" = --version ]; then echo 1.2.4; exit; fi\n"
                   "if [ \"$1\" = --help ]; then echo --variant; exit; fi\n"
                   f"echo call >> '{calls}'\necho opencode-go/glm-5.3-flash\n")
    cli.chmod(0o755)
    third, refreshed = probe()
    assert third.returncode == 0
    assert refreshed["cache_hit"] is False
    assert refreshed["version"] == "1.2.4"
    assert calls.read_text().splitlines() == ["call", "call"]


def test_capability_probe_lists_agy_and_codex_models_for_discovery(tmp_path, monkeypatch):
    router = load_router()
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path))
    agy = tmp_path / "agy"
    agy.write_text("#!/bin/sh\nif [ \"$1\" = --version ]; then echo 2.0; exit; fi\n"
                   "if [ \"$1\" = --help ]; then exit; fi\n"
                   "echo 'Fetching available models...'\n"
                   "printf 'gemini-3.8-flash-high\\tGemini 3.8 Flash (High)\\nclaude-sonnet-4-6\\tClaude Sonnet 4.6\\n'\n")
    agy.chmod(0o755)
    record, code = router.model_route_probe.probe_capabilities("agy", str(agy))
    assert code == 0
    assert record["models"] == ["gemini-3.8-flash-high", "claude-sonnet-4-6"]
    assert router.model_route_probe._listed_models(json.dumps({"models": [{"slug": "gpt-6-luna"}, {"slug": "gpt-6.1-sol"}]})) == [
        "gpt-6-luna", "gpt-6.1-sol"]


@pytest.mark.parametrize("adapter,expected", [
    ("cursor", ["auto", "gpt-5.3-codex-low", "gpt-5.3-codex-low-fast"]),
    ("opencode", ["opencode-go/beam-free", "opencode-go/claude-haiku-5-5",
                  "openrouter/~anthropic/claude-fable-latest", "openrouter/~anthropic/claude-haiku-latest"]),
    ("kiro", ["auto", "claude-sonnet-4.5"]),
])
def test_discovery_accepts_captured_cli_listings(tmp_path, adapter, expected):
    router = load_router()
    suffix = "json" if adapter == "kiro" else "txt"
    fixture = ROOT / "tests/fixtures/model-listings" / f"{adapter}-models.{suffix}"
    cli = tmp_path / adapter
    cli.write_text(f"#!{sys.executable}\nimport pathlib, sys\n"
                   "if sys.argv[1:] == ['--version']: print('1.0')\n"
                   "elif sys.argv[1:] == ['--help']: pass\n"
                   f"else: print(pathlib.Path({str(fixture)!r}).read_text())\n")
    cli.chmod(0o755)
    record, code = router.model_route_probe.probe_capabilities(adapter, str(cli), listing_only=True)
    assert code == 0
    assert record["models"] == expected
    cached = json.loads((Path(os.environ["AGENT_FABRIC_STATE_ROOT"]) / "capabilities.json").read_text())
    assert cached[adapter]["models"] == expected


@pytest.mark.parametrize("adapter", ["cursor", "agy", "codex", "kiro", "opencode"])
@pytest.mark.parametrize("failure", ["listing", "version"])
@pytest.mark.parametrize("cache_failures", [True, False])
def test_routing_discovery_failed_refresh_preserves_last_good_listing(tmp_path, adapter, failure, cache_failures):
    router = load_router()
    cli = tmp_path / adapter
    cli.write_text("#!/bin/sh\n" + ("if [ \"$1\" = --version ]; then echo 1.0; exit; fi\n"
                                  if failure == "listing" else "") + "exit 1\n")
    cli.chmod(0o755)
    state = Path(os.environ["AGENT_FABRIC_STATE_ROOT"]) / "capabilities.json"
    previous = {adapter: {"listing_format": 1, "version": "1.0", "executable": str(cli),
                          "models": ["last-good"], "observed_at": (datetime.now(timezone.utc) - timedelta(days=3650)).isoformat()}}
    state.write_text(json.dumps(previous))
    record, code = router.model_route_probe.probe_capabilities(adapter, str(cli), listing_only=True, refresh=True,
                                             cache_failures=cache_failures)
    if cache_failures:
        assert code == 0 and record["models"] == ["last-good"] and record["refresh_failed"]
    else:
        assert code == 1 and record["status"] == "probe_unavailable" and not record.get("models")
    cached = json.loads(state.read_text())
    if adapter == "opencode" and cache_failures and failure == "listing":
        assert cached[adapter].pop("refresh_failed_at", None)
        assert cached[adapter] == previous[adapter]
    else:
        assert cached == previous


def test_discovery_keeps_colon_variants_beside_their_base_id():
    router = load_router()
    assert router.model_route_probe._listed_models("openrouter/vendor/model\nopenrouter/vendor/model:free\n") == [
        "openrouter/vendor/model", "openrouter/vendor/model:free"]
    assert router.model_route_probe._listed_models("gpt-6-luna  Luna\ngpt-6-luna:beta  Luna beta\n") == ["gpt-6-luna", "gpt-6-luna:beta"]


@pytest.mark.parametrize("adapter,failure", [
    ("agy", "hang"), ("agy", "version"), ("agy", "bad_version"), ("agy", "junk"),
    ("agy", "mixed"), ("codex", "text"), ("kiro", "text"),
    ("codex", "malformed_json"),
    ("codex", "error_map"), ("kiro", "error_data_map"),
    ("agy", "unexpected_json"), ("cursor", "unexpected_json"), ("opencode", "unexpected_json"),
])
def test_unreliable_listing_is_unavailable_without_replacing_cache(tmp_path, adapter, failure):
    router = load_router()
    cli = tmp_path / adapter
    cli.write_text(
        f"#!{sys.executable}\nimport sys, time\n"
        f"failure = {failure!r}\n"
        "if sys.argv[1:] == ['--version']:\n"
        "    print('OK' if failure == 'bad_version' else '1.0'); sys.exit(1 if failure == 'version' else 0)\n"
        "if failure == 'hang': time.sleep(60)\n"
        "if failure == 'junk': print('Usage: agy models\\nfallback')\n"
        "elif failure == 'mixed': print('gemini-3.8-flash-low\\nError: sign in again')\n"
        "elif failure == 'malformed_json': print('{\"models\":[{\"id\":null},\"gpt-6-luna\"]}')\n"
        "elif failure == 'error_map': print('{\"models\":{\"error\":\"not authenticated\"}}')\n"
        "elif failure == 'error_data_map': print('{\"data\":{\"error\":\"login failed\"}}')\n"
        "elif failure == 'unexpected_json': print('{\"models\":[\"fallback\"]}')\n"
        "else: print('gpt-6-luna')\n"
    )
    cli.chmod(0o755)
    state = Path(os.environ["AGENT_FABRIC_STATE_ROOT"]) / "capabilities.json"
    previous = json.dumps({adapter: {"models": ["last-good"]}})
    state.write_text(previous)
    started = time.monotonic()
    record, code = router.model_route_probe.probe_capabilities(adapter, str(cli), listing_only=True,
                                            deadline=started + 0.3, refresh=True, cache_failures=False)
    assert code == 1 and record["status"] == "probe_unavailable"
    assert not record.get("models")
    assert time.monotonic() - started < 2
    assert state.read_text() == previous


def test_capability_probe_uses_supplied_environment(tmp_path):
    router = load_router()
    cli = tmp_path / "agy"
    cli.write_text(
        f"#!{sys.executable}\nimport os, sys\n"
        "assert os.environ['HOME'] == '/staged-home'\n"
        "assert os.environ['CODEX_HOME'] == '/capability-home'\n"
        "assert os.environ['ADAPTER_ENV'] == 'attempt'\n"
        "if sys.argv[1:] == ['--version']: print('agy v1.0')\n"
        "elif sys.argv[1:] == ['--help']: print('--model')\n"
        "else: print('gemini-3.8-flash-low')\n"
    )
    cli.chmod(0o755)
    record, code = router.model_route_probe.probe_capabilities("agy", str(cli), listing_only=True, env={
        **os.environ, "HOME": "/staged-home", "CODEX_HOME": "/capability-home", "ADAPTER_ENV": "attempt"})
    assert code == 0 and record["models"] == ["gemini-3.8-flash-low"]


def test_failed_capability_probe_reuses_negative_cache(tmp_path):
    router = load_router()
    calls = tmp_path / "listing-calls"
    cli = tmp_path / "agy"
    cli.write_text("#!/bin/sh\nif [ \"$1\" = --version ]; then echo 1.0; exit; fi\n"
                   f"echo call >> '{calls}'\nexit 1\n")
    cli.chmod(0o755)
    first, code = router.model_route_probe.probe_capabilities("agy", str(cli), listing_only=True)
    assert code == 1 and first["status"] == "probe_unavailable"
    second, code = router.model_route_probe.probe_capabilities("agy", str(cli), listing_only=True)
    assert code == 1 and second["cache_hit"] is True
    assert calls.read_text().splitlines() == ["call"]


def test_listing_only_probe_skips_the_kiro_safety_chat_and_keeps_its_evidence(tmp_path, monkeypatch):
    router = load_router()
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path))
    chats = []

    def fake_run(argv, **kwargs):
        if "--no-interactive" in argv:
            chats.append(argv)
        output = "1.2.3" if argv[-1] == "--version" else '{"models":["auto"]}'
        return subprocess.CompletedProcess(argv, 0, output, "")

    monkeypatch.setattr(router.subprocess, "run", fake_run)
    listed, code = router.model_route_probe.probe_capabilities("kiro", "/fake/kiro-cli", listing_only=True)
    assert code == 0 and listed["models"] == ["auto"]
    assert "read_only_probe" not in listed and chats == []
    full, code = router.model_route_probe.probe_capabilities("kiro", "/fake/kiro-cli")
    assert code == 0 and "read_only_probe" in full and len(chats) == 1
    again, _ = router.model_route_probe.probe_capabilities("kiro", "/fake/kiro-cli", listing_only=True)
    assert again["cache_hit"] is True and "read_only_probe" in again and len(chats) == 1


def _fake_kiro(router, monkeypatch, chats):
    def fake_run(argv, **kwargs):
        if "--no-interactive" in argv:
            chats.append(argv)
        output = "1.2.3" if argv[-1] == "--version" else '{"models":["auto"]}'
        return subprocess.CompletedProcess(argv, 0, output, "")
    monkeypatch.setattr(router.subprocess, "run", fake_run)


def test_listing_only_probe_drops_kiro_evidence_from_another_executable(tmp_path, monkeypatch):
    router = load_router()
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path))
    chats = []
    _fake_kiro(router, monkeypatch, chats)
    full, _ = router.model_route_probe.probe_capabilities("kiro", "/fake/a/kiro-cli")
    assert "read_only_probe" in full
    replaced, code = router.model_route_probe.probe_capabilities("kiro", "/fake/b/kiro-cli", listing_only=True)
    assert code == 0 and replaced["cache_hit"] is False
    assert "read_only_probe" not in replaced


def test_full_kiro_probe_rechecks_safety_older_than_a_day_despite_a_fresh_listing(tmp_path, monkeypatch):
    router = load_router()
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path))
    chats = []
    _fake_kiro(router, monkeypatch, chats)
    router.model_route_probe.probe_capabilities("kiro", "/fake/kiro-cli")
    path = tmp_path / "capabilities.json"
    cache = json.loads(path.read_text())
    stale = (datetime.now(timezone.utc) - timedelta(days=2)).isoformat()
    cache["kiro"]["read_only_probe"].update(
        {"checked_at": stale, "attempted_write": True, "permission_denied": True, "file_created": False})
    path.write_text(json.dumps(cache))
    listed, _ = router.model_route_probe.probe_capabilities("kiro", "/fake/kiro-cli", listing_only=True)
    assert listed["cache_hit"] is True and len(chats) == 1
    full, _ = router.model_route_probe.probe_capabilities("kiro", "/fake/kiro-cli")
    assert full["cache_hit"] is False and len(chats) == 2


def test_failed_capability_probe_retries_after_one_hour(tmp_path, monkeypatch):
    router = load_router()
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path))
    cli = tmp_path / "opencode"
    cli.write_text("#!/bin/sh\nif [ \"$1\" = --version ]; then echo 1.2.3; exit; fi\n"
                   "if [ \"$1\" = --help ]; then exit; fi\n"
                   "echo opencode-go/glm-5.3-flash\n")
    cli.chmod(0o755)
    (tmp_path / "capabilities.json").write_text(json.dumps({"opencode": {
        "status": "probe_unavailable", "version": "1.2.3", "executable": str(cli),
        "observed_at": (datetime.now(timezone.utc) - timedelta(hours=2)).isoformat(),
        "models": [],
    }}))
    record, code = router.model_route_probe.probe_capabilities("opencode", str(cli))
    assert code == 0
    assert record["cache_hit"] is False
    assert record["models"] == ["opencode-go/glm-5.3-flash"]


def test_snapshot_exposes_fresh_stale_alias_warning(tmp_path, monkeypatch):
    router = load_router()
    monkeypatch.setattr(router, "PRODUCT_CATALOG_PATH", ROOT / "config" / "model-routing.json")
    monkeypatch.setattr(router, "CATALOG_PATH", ROOT / "config" / "model-routing.json")
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path))
    (tmp_path / "capabilities.json").write_text(json.dumps({"codex": {
        "version": "1.2.3", "observed_at": datetime.now(timezone.utc).isoformat(),
        "models": ["gpt-7-luna"], "probed_flags": [],
    }}))
    snapshot = router.catalogue_snapshot()
    assert any("gpt-7-luna" in note and "fix:" in note
               for note in snapshot["stale_alias_warnings"])


def test_snapshot_reads_cached_probe_without_running_clis(tmp_path):
    cli = tmp_path / "codex"
    calls = tmp_path / "calls"
    cli.write_text(f"#!/bin/sh\necho called >> '{calls}'\necho gpt-7-luna\n")
    cli.chmod(0o755)
    kiro = tmp_path / "kiro-cli"
    kiro.write_text(f"#!/bin/sh\necho \"$@\" >> '{calls}'\necho auto\n")
    kiro.chmod(0o755)
    (tmp_path / "capabilities.json").write_text(json.dumps({"codex": {
        "version": "1.2.2", "observed_at": (datetime.now(timezone.utc) - timedelta(days=3650)).isoformat(),
        "models": ["gpt-6-luna"]}}))
    before = (tmp_path / "capabilities.json").read_bytes()
    run = subprocess.run([sys.executable, str(ROOT / "scripts" / "model_route.py"), "snapshot", "--json"],
                         capture_output=True, text=True,
                         env={**os.environ, "PATH": str(tmp_path), "AGENT_FABRIC_STATE_ROOT": str(tmp_path),
                              "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT), "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT)})
    assert run.returncode == 0, run.stderr
    snapshot = json.loads(run.stdout)
    assert snapshot["stale_alias_warnings"] == []
    assert (tmp_path / "capabilities.json").read_bytes() == before
    assert not calls.exists()


def test_concurrent_capability_probes_keep_both_adapters(tmp_path):
    executables = {}
    for adapter, model in (("codex", "gpt-7-luna"), ("opencode", "opencode-go/glm-5.3-flash")):
        executable = tmp_path / adapter
        listing = json.dumps({"models": [model]}) if adapter == "codex" else model
        executable.write_text("#!/bin/sh\nif [ \"$1\" = --version ]; then echo 1.0; exit; fi\n"
                              "if [ \"$1\" = --help ]; then exit; fi\nsleep 0.1\n"
                              f"echo '{listing}'\n")
        executable.chmod(0o755)
        executables[adapter] = executable
    env = {**os.environ, "HARNESS_PYTHON": sys.executable,
           "AGENT_FABRIC_STATE_ROOT": str(tmp_path)}
    def probe(adapter):
        return subprocess.run([str(SCRIPT), "probe", "--adapter", adapter,
                               "--executable", str(executables[adapter]), "--json"],
                              capture_output=True, text=True, env=env)
    with ThreadPoolExecutor(max_workers=2) as executor:
        results = list(executor.map(probe, executables))
    assert all(result.returncode == 0 for result in results), [result.stderr for result in results]
    cache = json.loads((tmp_path / "capabilities.json").read_text())
    assert cache["codex"]["models"] == ["gpt-7-luna"]
    assert cache["opencode"]["models"] == ["opencode-go/glm-5.3-flash"]
    assert not list(tmp_path.glob("capabilities-*.tmp"))


@pytest.mark.parametrize("adapter,executable,listing_body,models", [
    pytest.param("cursor", "cursor-agent", "echo 'Available models:'\necho '  auto'\necho '  grok-4.7'\necho '  composer-2.5  Composer'\n",
                 ["auto", "grok-4.7", "composer-2.5"], id="cursor-text"),
    pytest.param("codex", "codex", 'echo \'{"models":{"gpt-6-luna":{},"gpt-6.1-sol":{}}}\'\n',
                 ["gpt-6-luna", "gpt-6.1-sol"], id="codex-json-map"),
])
def test_model_probe_records_listed_ids(tmp_path, adapter, executable, listing_body, models):
    cli = tmp_path / executable
    cli.write_text('#!/bin/sh\nif [ "$1" = --version ]; then echo 1.0; exit; fi\n'
                   'if [ "$1" = --help ]; then exit; fi\n' + listing_body)
    cli.chmod(0o755)
    result = subprocess.run([str(SCRIPT), "probe", "--adapter", adapter,
                             "--executable", str(cli), "--json"], capture_output=True, text=True,
                            env={**os.environ, "HARNESS_PYTHON": sys.executable,
                                 "AGENT_FABRIC_STATE_ROOT": str(tmp_path)})
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout)["models"] == models


def test_cursor_plain_text_model_probe_ignores_reworded_footer(tmp_path):
    router = load_router()
    listing = (ROOT / "tests/fixtures/model-listings/cursor-models.txt").read_text()
    listing = listing.replace(
        "Tip: use --model <id>",
        "Choose one of these models with the --model option",
    )
    assert router.model_route_probe._listed_models(listing, "cursor") == [
        "auto", "gpt-5.3-codex-low", "gpt-5.3-codex-low-fast",
    ]


def test_ambiguous_name_chooses_newest_when_no_default(tmp_path):
    catalog = product_catalog()
    catalog["adapters"]["agy"]["models"].extend([
        {"id": "claude-sonnet-4-6", "names": ["verse"]},
        {"id": "claude-sonnet-5-1", "names": ["verse"]},
    ])
    path = tmp_path / "model-routing.json"
    path.write_text(json.dumps(catalog))
    result = subprocess.run([str(SCRIPT), "resolve", "--adapter", "agy", "--alias",
                             "verse", "--role", "worker", "--catalog", str(path)],
                            capture_output=True, text=True,
                            env={**os.environ, "HARNESS_PYTHON": sys.executable,
                                 "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT)})
    route = json.loads(result.stdout)
    assert result.returncode == 0, route
    assert route["resolved_model"] == "claude-sonnet-5-1"
    assert any("ambiguous" in note for note in route["notes"])


def test_suffix_model_id_keeps_or_overrides_its_effort():
    result, route = resolve("--adapter", "cursor", "--model", "grok-4.7-high", "--role", "worker")
    assert result.returncode == 0, route
    assert route["resolved_model"] == "grok-4.7-high"
    assert route["effort_applied"] == "high"
    result, route = resolve("--adapter", "cursor", "--model", "grok-4.7-high",
                            "--effort", "low", "--role", "worker")
    assert result.returncode == 0, route
    assert route["resolved_model"] == "grok-4.7-low"
    assert route["effort_applied"] == "low"


@pytest.mark.parametrize(
    "removed_args",
    (
        ("--adapter-gate", "direct-cli"),
        ("--fabric-config", "unused.yaml"),
    ),
)
def test_removed_fabric_activation_arguments_are_rejected(removed_args):
    result = subprocess.run(
        [
            str(SCRIPT),
            "resolve",
            "--adapter",
            "copilot",
            "--model",
            "gemini-3.8-flash",
            "--alias",
            "principal",
            "--role",
            "worker",
            *removed_args,
        ],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    assert result.returncode == 2
    assert result.stdout == ""
    assert "unrecognized arguments" in result.stderr


def capability_snapshot(models, source="codex debug models"):
    return {
        "schema_version": 1,
        "source": source,
        "observed_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "models": models,
    }


def write_codex_capability_snapshot(tmp_path, *, observed_at=None, models=None):
    if models is None:
        models = {
            "gpt-6-astra": {
                "resolved_model": "gpt-6-astra",
                "supported_efforts": ["low", "medium", "high", "xhigh", "max", "ultra"],
            },
            "gpt-5.6-terra": {
                "resolved_model": "gpt-5.6-terra",
                "supported_efforts": ["low", "medium", "high", "xhigh", "max", "ultra"],
            },
            "gpt-5.6-luna": {
                "resolved_model": "gpt-5.6-luna",
                "supported_efforts": ["low", "medium", "high", "xhigh", "max"],
            },
            "gpt-6.1-sol": {
                "resolved_model": "gpt-6.1-sol",
                "supported_efforts": ["low", "medium", "high", "xhigh", "max", "ultra"],
            },
            "gpt-6-luna": {
                "resolved_model": "gpt-6-luna",
                "supported_efforts": ["low", "medium", "high", "xhigh", "max"],
            },
        }
    value = capability_snapshot(models)
    if observed_at is not None:
        value["observed_at"] = observed_at
    snapshot = tmp_path / "codex-capabilities.json"
    snapshot.write_text(json.dumps(value))
    return snapshot


def write_account_default_catalog(tmp_path):
    catalog = product_catalog()
    catalog["adapters"]["account-default-fixture"] = {
        "endpoint_provider": "fixture",
        "fixed_model_family": "openai",
        "effort_transport": "flag",
        "model_selection": "account-default",
    }
    path = tmp_path / "account-default-model-routing.json"
    path.write_text(json.dumps(catalog))
    return path


def write_codex_compatibility(tmp_path, *, requires_explicit_model):
    text = (ROOT / "config" / "adapter-compatibility.yaml").read_text()
    start = text.index("  codex-app-server:")
    end = text.index("\n  herdr:", start)
    block = text[start:end].replace(
        "requires_explicit_model: true",
        f"requires_explicit_model: {str(requires_explicit_model).lower()}",
    )
    path = tmp_path / "adapter-compatibility.yaml"
    path.write_text(text[:start] + block + text[end:])
    return path


def load_router():
    return load_script(ROOT / "scripts" / "model_route.py", "model_route_under_test", register=True)


def use_catalog(router, monkeypatch, tmp_path, catalog):
    path = tmp_path / "model-routing.json"
    path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", path)
    monkeypatch.setattr(router, "PRODUCT_CATALOG_PATH", path)
    return path


def product_catalog():
    return json.loads(json.dumps(CATALOG))


def principal(family, **kw):
    adapter, model = {"anthropic": ("claude", "opus"), "openai": ("codex", "gpt-5.6-sol")}[family]
    return route_candidate(f"{family}-principal", adapter=adapter, family=family, model=model, **kw)


def route_candidate(
    candidate_id,
    *,
    adapter,
    family,
    model,
    status="ok",
    alias="principal",
    effort="high",
    availability=None,
):
    route = {
        "schema_version": 1,
        "status": status,
        "adapter": adapter,
        "task_class": "critical-review",
        "route_source": "task-class",
        "role": "critical-review",
        "alias": alias,
        "requested_effort": effort,
        "effort": effort,
        "model_family": family,
        "resolved_model": model,
    }
    return {
        "candidate_id": candidate_id,
        "route": route,
        "availability": availability or {
            "observation": "Observed",
            "value": "available",
        },
    }


def select_route(
    tmp_path,
    candidates,
    preferences,
    *,
    state_name="spread-state.json",
    candidates_name="candidates.json",
    catalog=None,
):
    candidates_path = tmp_path / candidates_name
    preferences_path = tmp_path / f"{candidates_name}.preferences.json"
    state_path = tmp_path / state_name
    candidates_path.write_text(json.dumps({"schema_version": 1, "candidates": candidates}))
    preferences_path.write_text(
        preferences if isinstance(preferences, str) else json.dumps(preferences)
    )
    result = subprocess.run(
        [
            str(SCRIPT),
            "select",
            "--task-class",
            "critical-review",
            "--role",
            "critical-review",
            "--candidates-file",
            str(candidates_path),
            "--preferences-file",
            str(preferences_path),
            "--spread-state-file",
            str(state_path),
            *(["--catalog", str(catalog)] if catalog else []),
        ],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    return result, json.loads(result.stdout) if result.stdout else None, state_path


@pytest.mark.parametrize("effort", ["medium", "high"])
def test_selector_uses_configured_task_class_tier(tmp_path, effort):
    catalog = product_catalog()
    catalog['task_class_routes']['critical-review'].update(
        alias='senior', effort=effort, minimum_effort='medium')
    path = tmp_path / 'catalog.json'
    path.write_text(json.dumps(catalog))
    candidate = principal('anthropic', alias='senior', effort='medium')
    result, selection, _ = select_route(tmp_path, [candidate], {}, catalog=path)
    assert result.returncode == 0, result.stderr
    assert selection['chosen_candidate_id'] == candidate['candidate_id']


def test_critical_review_preference_cannot_lower_tier_or_effort(tmp_path):
    candidates = [
        principal("anthropic"),
        route_candidate(
            "openai-cheaper",
            adapter="codex",
            family="openai",
            model="gpt-5.6-terra",
            status="task_class_effort_below_floor",
            alias="senior",
            effort="medium",
        ),
    ]
    result, receipt, _ = select_route(tmp_path, candidates, {
        "schema_version": 1,
        "task_classes": {
            "critical-review": {"family_affinity": ["openai"]},
        },
        "spreading": {"policy": "fair-round-robin"},
    })

    assert result.returncode == 0
    assert receipt["chosen_candidate_id"] == "anthropic-principal"
    assert receipt["chosen_route"]["alias"] == "principal"
    assert receipt["chosen_route"]["effort"] == "high"
    assert {
        "kind": "family_affinity",
        "scope": "task_class",
        "value": "openai",
        "reason": "no_admissible_candidate",
    } in receipt["preference"]["not_honoured"]


def test_disabled_adapter_and_outside_model_preferences_are_reported(tmp_path):
    candidates = [
        principal("anthropic"),
        route_candidate(
            "disabled-google",
            adapter="agy",
            family="google",
            model="gemini-pro",
            status="adapter_disabled",
        ),
    ]
    result, receipt, _ = select_route(tmp_path, candidates, {
        "schema_version": 1,
        "roles": {
            "critical-review": {
                "adapter_affinity": ["agy"],
                "model_affinity": ["gemini-pro"],
            },
        },
        "avoid": {
            "adapters": ["agy"],
            "models": ["gemini-pro"],
        },
    })

    assert result.returncode == 0
    assert receipt["chosen_candidate_id"] == "anthropic-principal"
    assert {
        "kind": "adapter_affinity",
        "scope": "role",
        "value": "agy",
        "reason": "adapter_disabled",
    } in receipt["preference"]["not_honoured"]
    assert {
        "kind": "model_affinity",
        "scope": "role",
        "value": "gemini-pro",
        "reason": "outside_admissible_set",
    } in receipt["preference"]["not_honoured"]
    assert {
        "kind": "adapter_deprioritise",
        "scope": "avoid",
        "value": "agy",
    } in receipt["preference"]["applied"]
    assert {
        "kind": "model_deprioritise",
        "scope": "avoid",
        "value": "gemini-pro",
    } in receipt["preference"]["applied"]


def test_fair_spreading_is_reproducible_from_persisted_state(tmp_path):
    candidates = [
        principal("anthropic"),
        principal("openai"),
    ]
    preferences = {
        "schema_version": 1,
        "task_classes": {
            "critical-review": {"family_affinity": ["openai", "anthropic"]},
        },
        "spreading": {"policy": "fair-round-robin"},
    }

    first = []
    for index in range(8):
        result, receipt, state_path = select_route(
            tmp_path,
            candidates,
            preferences,
            state_name="first-state.json",
            candidates_name=f"first-{index}.json",
        )
        assert result.returncode == 0
        first.append(receipt["chosen_route"]["model_family"])
    persisted = json.loads(state_path.read_text())

    second = []
    for index in range(8):
        result, receipt, second_state_path = select_route(
            tmp_path,
            list(reversed(candidates)),
            preferences,
            state_name="second-state.json",
            candidates_name=f"second-{index}.json",
        )
        assert result.returncode == 0
        second.append(receipt["chosen_route"]["model_family"])

    assert first == second
    assert first.count("anthropic") == 4
    assert first.count("openai") == 4
    assert persisted == json.loads(second_state_path.read_text())
    assert persisted["assignments"] == {"anthropic": 4, "openai": 4}


def test_concurrent_fan_out_serialises_persisted_spread_state(tmp_path):
    candidates = [
        principal("anthropic"),
        principal("openai"),
    ]
    preferences = {
        "schema_version": 1,
        "spreading": {"policy": "fair-round-robin"},
    }

    def choose(index):
        result, receipt, state_path = select_route(
            tmp_path,
            candidates,
            preferences,
            state_name="concurrent-state.json",
            candidates_name=f"concurrent-{index}.json",
        )
        return result, receipt, state_path

    with ThreadPoolExecutor(max_workers=8) as executor:
        selections = list(executor.map(choose, range(16)))

    assert all(result.returncode == 0 for result, _, _ in selections)
    families = [receipt["chosen_route"]["model_family"] for _, receipt, _ in selections]
    assert families.count("anthropic") == families.count("openai") == 8
    persisted = json.loads(selections[-1][2].read_text())
    assert persisted == {
        "schema_version": 1,
        "assignments": {"anthropic": 8, "openai": 8},
        "selection_count": 16,
    }


def test_unknown_availability_is_distinct_from_observed_unavailable(tmp_path):
    candidates = [
        principal("anthropic"),
        route_candidate(
            "openai-unobserved",
            adapter="codex",
            family="openai",
            model="gpt-5.6-sol",
            availability={
                "observation": "Unknown",
                "reason": "AvailabilityNotObserved",
            },
        ),
        route_candidate(
            "google-unavailable",
            adapter="agy",
            family="google",
            model="gemini-pro",
            availability={
                "observation": "Observed",
                "value": "unavailable",
            },
        ),
    ]
    result, receipt, _ = select_route(tmp_path, candidates, {
        "schema_version": 1,
        "task_classes": {
            "critical-review": {"family_affinity": ["openai"]},
        },
    })

    assert result.returncode == 0
    by_id = {candidate["candidate_id"]: candidate for candidate in receipt["candidates"]}
    assert by_id["openai-unobserved"]["availability"] == {
        "observation": "Unknown",
        "reason": "AvailabilityNotObserved",
    }
    assert by_id["openai-unobserved"]["admissible"] is True
    assert by_id["openai-unobserved"]["disposition"] == "selected"
    assert by_id["google-unavailable"]["availability"] == {
        "observation": "Observed",
        "value": "unavailable",
    }
    assert by_id["google-unavailable"]["disposition"] == "observed_unavailable"


@pytest.mark.parametrize(
    "availability",
    [
        {"observation": "Observed", "value": "Unavailable"},
        {"observation": "observed", "value": "unavailable"},
    ],
)
def test_malformed_observed_availability_rejects_candidate_set(
    tmp_path,
    availability,
):
    result, receipt, state_path = select_route(
        tmp_path,
        [
            route_candidate(
                "malformed-observation",
                adapter="claude",
                family="anthropic",
                model="opus",
                availability=availability,
            ),
        ],
        {"schema_version": 1},
    )

    assert result.returncode == 2
    assert receipt["status"] == "candidate_set_invalid"
    assert not state_path.exists()


def test_unknown_preference_entries_load_without_changing_route(tmp_path):
    candidates = [
        principal("anthropic"),
        principal("openai"),
    ]
    baseline_result, baseline, _ = select_route(
        tmp_path,
        candidates,
        {"schema_version": 1},
        state_name="baseline-state.json",
        candidates_name="baseline.json",
    )
    result, receipt, _ = select_route(
        tmp_path,
        list(reversed(candidates)),
        {
            "schema_version": 1,
            "task_classes": {
                "critical-review": {
                    "family_affinity": ["future-provider"],
                    "model_affinity": ["future-model"],
                    "adapter_affinity": ["future-adapter"],
                    "future_key": {"anything": True},
                },
            },
            "future_top_level_key": "ignored",
        },
        state_name="unknown-state.json",
        candidates_name="unknown.json",
    )

    assert baseline_result.returncode == result.returncode == 0
    assert receipt["chosen_candidate_id"] == baseline["chosen_candidate_id"]
    assert {item["reason"] for item in receipt["preference"]["ignored"]} == {
        "unknown_adapter",
        "unknown_family",
        "unknown_key",
        "unknown_model",
    }
    assert {
        "kind": "future_key",
        "scope": "task_class",
        "value": "future_key",
        "reason": "unknown_key",
    } in receipt["preference"]["ignored"]
    assert {
        "kind": "future_top_level_key",
        "scope": "top_level",
        "value": "future_top_level_key",
        "reason": "unknown_key",
    } in receipt["preference"]["ignored"]


def test_unknown_preference_keys_are_recorded_in_ignored(tmp_path):
    result, receipt, _ = select_route(
        tmp_path,
        [
            principal("anthropic"),
        ],
        {
            "schema_version": 1,
            "task_classes": {
                "critical-review": {"family_affinty": ["openai"]},
            },
            "depriotitise": {"families": ["anthropic"]},
        },
    )

    assert result.returncode == 0
    assert receipt["preference"] == {
        "file_status": "loaded",
        "applied": [],
        "not_honoured": [],
        "ignored": [
            {
                "kind": "depriotitise",
                "scope": "top_level",
                "value": "depriotitise",
                "reason": "unknown_key",
            },
            {
                "kind": "family_affinty",
                "scope": "task_class",
                "value": "family_affinty",
                "reason": "unknown_key",
            },
        ],
    }


@pytest.mark.parametrize("preferences,corrupt", [
    pytest.param('{"schema_version":1,,,', True, id="corrupt-file"),
    pytest.param({"schema_version": 1, "spreading": {"policy": "weighted-by-cost"}}, False, id="unsupported-policy"),
])
def test_invalid_preferences_report_their_reason(tmp_path, preferences, corrupt):
    result, receipt, _ = select_route(tmp_path, [principal("anthropic")], preferences)
    assert result.returncode == 0
    if corrupt:
        assert receipt["preference"]["file_status"] == "corrupt"
    else:
        assert receipt["spreading"]["policy"] == "fair-round-robin"
        assert {"kind": "spreading_policy", "scope": "spreading", "value": "weighted-by-cost",
                "reason": "unsupported_policy"} in receipt["preference"]["ignored"]


def test_deprioritise_preference_changes_tie_break_and_is_recorded(tmp_path):
    candidates = [
        principal("anthropic"),
        principal("openai"),
    ]
    result, receipt, _ = select_route(tmp_path, candidates, {
        "schema_version": 1,
        "deprioritise": {"families": ["anthropic"]},
    })

    assert result.returncode == 0
    assert receipt["chosen_candidate_id"] == "openai-principal"
    assert {
        "kind": "family_deprioritise",
        "scope": "deprioritise",
        "value": "anthropic",
    } in receipt["preference"]["applied"]


def test_not_honoured_reason_distinguishes_higher_preference(tmp_path):
    candidates = [
        principal("anthropic"),
        principal("openai"),
    ]
    result, receipt, _ = select_route(tmp_path, candidates, {
        "schema_version": 1,
        "task_classes": {
            "critical-review": {"family_affinity": ["openai", "anthropic"]},
        },
    })

    assert result.returncode == 0
    assert receipt["chosen_candidate_id"] == "openai-principal"
    assert {
        "kind": "family_affinity",
        "scope": "task_class",
        "value": "anthropic",
        "reason": "lost_to_higher_preference",
    } in receipt["preference"]["not_honoured"]


def test_not_honoured_reason_distinguishes_fair_spreading(tmp_path):
    openai = principal("openai")
    initial_result, _, _ = select_route(
        tmp_path,
        [openai],
        {"schema_version": 1},
        state_name="reason-state.json",
        candidates_name="reason-initial.json",
    )
    result, receipt, _ = select_route(
        tmp_path,
        [
            principal("anthropic"),
            openai,
        ],
        {
            "schema_version": 1,
            "task_classes": {
                "critical-review": {"family_affinity": ["openai"]},
            },
        },
        state_name="reason-state.json",
        candidates_name="reason-spread.json",
    )

    assert initial_result.returncode == result.returncode == 0
    assert receipt["chosen_candidate_id"] == "anthropic-principal"
    assert {
        "kind": "family_affinity",
        "scope": "task_class",
        "value": "openai",
        "reason": "fair_spreading",
    } in receipt["preference"]["not_honoured"]


def test_deprioritise_only_admissible_candidate_reports_why_not_honoured(tmp_path):
    result, receipt, _ = select_route(
        tmp_path,
        [
            principal("anthropic"),
        ],
        {
            "schema_version": 1,
            "deprioritise": {"families": ["anthropic"]},
        },
    )

    assert result.returncode == 0
    assert {
        "kind": "family_deprioritise",
        "scope": "deprioritise",
        "value": "anthropic",
        "reason": "sole_admissible_candidate",
    } in receipt["preference"]["not_honoured"]


def test_selector_rejects_forged_ok_route_below_hard_floor(tmp_path):
    candidates = [
        principal("anthropic"),
        route_candidate(
            "forged-cheap",
            adapter="codex",
            family="openai",
            model="gpt-5.6-terra",
            alias="senior",
            effort="medium",
        ),
    ]
    result, receipt, _ = select_route(tmp_path, candidates, {
        "schema_version": 1,
        "task_classes": {
            "critical-review": {"family_affinity": ["openai"]},
        },
    })

    assert result.returncode == 0
    assert receipt["chosen_candidate_id"] == "anthropic-principal"
    by_id = {candidate["candidate_id"]: candidate for candidate in receipt["candidates"]}
    assert by_id["forged-cheap"]["admissible"] is False
    assert by_id["forged-cheap"]["disposition"] == "hard_policy_mismatch"


def test_repeated_router_loads_share_path_loaded_sibling_modules():
    # `load_router` executes scripts/model_route.py once per test, so the
    # catalogue module it loads by path must be reused rather than re-executed.
    # Creating a fresh module on every load while `sys.modules` keeps the first
    # leaves the router bound to functions whose globals belong to a different
    # object than the cached one: rebinding a name on one is then invisible to
    # callers resolving it through the other, which a normal `import` never does.
    first = load_router()
    second = load_router()
    cached = sys.modules["model_route_catalog"]
    cached_preferences = sys.modules["model_route_preferences"]

    assert first.infer_family is cached.infer_family
    assert second.infer_family is cached.infer_family
    assert first.override_scan_families is second.override_scan_families
    assert first.ALIAS_ORDER is second.ALIAS_ORDER
    assert first.EFFORT_ORDER is cached.EFFORT_ORDER
    assert first._preferences is cached_preferences
    assert second._preferences is cached_preferences
    assert first._preferences.select is second._preferences.select


def test_risk_tier_override_catalogue_retargets_single_model(
    tmp_path, monkeypatch, capsys
):
    router = load_router()
    catalog = product_catalog()
    override = catalog["families"]["anthropic"]["risk_tier_overrides"]["crucial"]
    override["models"] = ["claude-retargeted-model"]
    override["roles"] = ["reviewer-one", "reviewer-two"]
    override["alias"] = "senior"
    override["default_effort"] = "low"
    override["maximum_effort"] = "high"
    catalog_path = use_catalog(router, monkeypatch, tmp_path, catalog)

    result = router.main([
        "resolve", "--adapter", "claude", "--alias", override["alias"],
        "--role", override["roles"][0], "--model-override-tier", "crucial",
        "--model", override["models"][0], "--effort", "high",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 0
    assert route["status"] == "ok"
    assert route["resolved_model"] == "claude-retargeted-model"
    assert route["route_source"] == "model-override"
    assert route["policy_override"] == "crucial-claude-retargeted-model-reviewer-one-reviewer-two"
    assert route["effort"] == "high"


def test_special_model_override_uses_an_explicit_non_lifecycle_input():
    override = CRUCIAL_RISK_OVERRIDE
    model = override["models"][0]

    result, route = resolve(
        "--adapter", "claude", "--alias", override["alias"],
        "--role", override["roles"][0], "--model-override-tier", "crucial",
        "--model", model, "--effort", override["maximum_effort"],
    )

    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["model_override_tier"] == "crucial"
    assert "risk_tier" not in route
    assert route["resolved_model"] == model


@pytest.mark.parametrize("model", ("fable", "claude-fable-5-0"))
def test_versioned_fable_override_reserves_generic_and_older_model_ids(model):
    result, route = resolve(
        "--adapter", "claude", "--alias", "principal", "--role", "worker",
        "--model", model, "--available-model", model, "--effort", "high",
    )

    assert result.returncode == 1
    assert route["status"] == "risk_tier_override_required"


@pytest.mark.parametrize("tiers,model,code,status,resolved", [
    pytest.param(("crucial",), "claude-newcomer", 1, "risk_tier_override_required", None, id="new-occupant"),
    pytest.param(tuple(CATALOG["families"]["anthropic"]["risk_tier_overrides"]), "fable", 0, "ok",
                 "claude-fable-5-1", id="old-occupant-ungated"),
    pytest.param(("crucial",), CATALOG["families"]["anthropic"]["risk_tier_overrides"]["terminal"]["models"][0],
                 1, "risk_tier_override_required", None, id="other-tier-still-reserved"),
])
def test_retargeted_override_reservations(tmp_path, monkeypatch, capsys, tiers, model, code, status, resolved):
    router = load_router()
    catalog = product_catalog()
    for tier in tiers:
        catalog["families"]["anthropic"]["risk_tier_overrides"][tier]["models"] = ["claude-newcomer"]
    use_catalog(router, monkeypatch, tmp_path, catalog)
    result = router.main([
        "resolve", "--adapter", "claude", "--alias", "principal", "--role", "worker", "--model", model,
    ])
    route = json.loads(capsys.readouterr().out)
    assert result == code
    assert route["status"] == status
    if resolved is not None:
        assert route["resolved_model"] == resolved


def test_foreign_family_risk_override_does_not_gate_explicit_model(
    tmp_path, monkeypatch, capsys
):
    router = load_router()
    catalog = product_catalog()
    catalog["families"]["openai"]["risk_tier_overrides"] = {
        "crucial": {
            "roles": ["synthesis", "adjudication"],
            "alias": "principal",
            "models": ["opus"],
            "default_effort": "medium",
            "maximum_effort": "medium",
        },
    }
    catalog_path = use_catalog(router, monkeypatch, tmp_path, catalog)

    result = router.main([
        "resolve", "--adapter", "claude", "--alias", "principal",
        "--role", "worker", "--model", "claude-opus-4-6",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 0
    assert route["status"] == "ok"
    assert route["resolved_model"] == "claude-opus-4-6"


@pytest.mark.parametrize("occupant,aliases,roles", [
    pytest.param("principal", None, None, id="tier-name"),
    pytest.param("opus", None, None, id="generic-model"),
    pytest.param("opus", ["claude-opus-4-6"], {}, id="versioned-alias"),
    pytest.param(None, ["claude-fable-5-2"], {}, id="different-fable-version"),
    pytest.param("claude-role-model", None, {"worker": {"principal": ["claude-role-model"]}}, id="role-alias"),
])
def test_override_occupant_cannot_route_through_aliases(tmp_path, monkeypatch, capsys, occupant, aliases, roles):
    router = load_router()
    catalog = product_catalog()
    family = catalog["families"]["anthropic"]
    if occupant is not None:
        family["risk_tier_overrides"]["crucial"]["models"] = [occupant]
    if aliases is not None:
        family["aliases"]["principal"] = aliases
    if roles:
        family["role_overrides"].update(roles)
    elif roles is not None:
        family["role_overrides"] = roles
    use_catalog(router, monkeypatch, tmp_path, catalog)
    result = router.main(["resolve", "--adapter", "claude", "--alias", "principal", "--role", "worker"])
    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "risk_tier_config_invalid"


@pytest.mark.parametrize(
    ("occupant", "explicit_model"),
    (
        ("opus-4", "claude-opus-4-6"),
        ("claude", "claude-opus-4-6"),
        ("anthropic", "anthropic/claude-opus-4-6"),
        ("4", "claude-opus-4-6"),
    ),
)
def test_risk_tier_override_occupant_cannot_partially_match_explicit_model(
    tmp_path, monkeypatch, capsys, occupant, explicit_model
):
    router = load_router()
    catalog = product_catalog()
    catalog["families"]["anthropic"]["risk_tier_overrides"]["crucial"]["models"] = [
        occupant
    ]
    catalog_path = use_catalog(router, monkeypatch, tmp_path, catalog)

    result = router.main([
        "resolve", "--adapter", "claude", "--alias", "principal",
        "--role", "worker", "--model", explicit_model,
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "risk_tier_config_invalid"


def test_foreign_family_override_collision_does_not_reject_adapter_route(
    tmp_path, monkeypatch, capsys
):
    router = load_router()
    catalog = product_catalog()
    catalog["families"]["anthropic"]["risk_tier_overrides"]["crucial"]["models"] = [
        "opus"
    ]
    catalog_path = use_catalog(router, monkeypatch, tmp_path, catalog)
    snapshot = write_codex_capability_snapshot(tmp_path)

    result = router.main([
        "resolve", "--adapter", "codex", "--alias", "senior",
        "--role", "worker", "--capabilities-file", str(snapshot),
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 0
    assert route["status"] == "ok"
    assert route["model_family"] == "openai"


def test_missing_route_input_precedes_risk_tier_configuration_validation(
    tmp_path, monkeypatch, capsys
):
    router = load_router()
    catalog = product_catalog()
    catalog["families"]["anthropic"]["risk_tier_overrides"]["crucial"]["models"] = [
        "opus"
    ]
    catalog_path = use_catalog(router, monkeypatch, tmp_path, catalog)

    result = router.main([
        "resolve", "--adapter", "claude", "--role", "worker",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "route_input_missing"


@pytest.mark.parametrize("role,check_family", [
    pytest.param("lead", True, id="lead"),
    pytest.param("critical-review", True, id="critical-review"),
    pytest.param("other-primary", False, id="other-primary"),
])
def test_claude_principal_roles_default_to_opus(role, check_family):
    result, route = resolve("--adapter", "claude", "--alias", "principal", "--role", role)
    assert result.returncode == 0
    assert route["resolved_model"] == "opus"
    if check_family:
        assert route["model_family"] == "anthropic"


def test_opus_unavailable_has_no_implicit_risk_override_fallback():
    result, route = resolve(
        "--adapter",
        "claude",
        "--alias",
        "principal",
        "--role",
        "lead",
        "--available-model",
        RISK_OVERRIDE_MODEL,
    )
    assert result.returncode == 1
    assert route["status"] == "no_candidate_available"


@pytest.mark.parametrize(
    "model_template",
    (
        "{occupant}", "claude-{occupant}-5", "anthropic/claude-{occupant}-5",
        "claude.{occupant}.5", "Claude {occupant} 5", "claude:{occupant}:5",
        "{occupant}5", "claude{occupant}5",
    ),
)
def test_explicit_override_occupant_identifiers_require_the_bounded_risk_override(
    model_template
):
    model = model_template.format(occupant=RISK_OVERRIDE_MODEL)
    result, route = resolve(
        "--adapter", "claude", "--alias", "principal", "--role", "worker",
        "--model", model, "--effort", "high",
    )
    assert result.returncode == 1
    assert route["status"] == "risk_tier_override_required"


def test_explicit_override_occupant_stays_reserved_for_broker_adapters():
    result, route = resolve(
        "--adapter", "cursor", "--alias", "principal", "--role", "worker",
        "--model", RISK_OVERRIDE_MODEL, "--effort", "high",
    )
    assert result.returncode == 1
    assert route["status"] == "risk_tier_override_required"


@pytest.mark.parametrize(
    "route",
    (
        ("--alias", "principal", "--role", "worker"),
        ("--task-class", "critical-review", "--role", "critical-review"),
    ),
    ids=("alias", "task-class"),
)
def test_capability_resolved_override_occupant_requires_explicit_risk_tier(
    tmp_path, monkeypatch, capsys, route
):
    router = load_router()
    catalog_path = Path(ROOT / "config" / "model-routing.json")
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)
    monkeypatch.setattr(router, "PRODUCT_CATALOG_PATH", catalog_path)
    snapshot = capability_snapshot({
        "opus": {
            "resolved_model": f"claude-opus-4-6-{RISK_OVERRIDE_MODEL}",
            "requested_effort": "high",
            "effort_verified": False,
        },
    }, source="claude subscription canary")
    snapshot["provenance"] = {
        "kind": "subscription_runtime_canary",
        "auth_method": "claude.ai",
        "subscription_type": "pro",
    }
    snapshot_path = tmp_path / "claude-caps.json"
    snapshot_path.write_text(json.dumps(snapshot))

    result = router.main([
        "resolve", "--adapter", "claude", *route,
        "--capabilities-file", str(snapshot_path),
    ])

    receipt = json.loads(capsys.readouterr().out)
    assert router.CATALOG_PATH == catalog_path
    assert result == 1
    assert receipt["status"] == (
        "no_candidate_available" if route[0] == "--task-class" else "risk_tier_override_required"
    )


def test_capability_resolved_override_occupant_records_explicit_risk_tier(
    tmp_path, monkeypatch, capsys
):
    router = load_router()
    catalog_path = Path(ROOT / "config" / "model-routing.json")
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)
    monkeypatch.setattr(router, "PRODUCT_CATALOG_PATH", catalog_path)
    resolved_model = RISK_OVERRIDE_MODEL
    snapshot = capability_snapshot({
        RISK_OVERRIDE_MODEL: {
            "resolved_model": resolved_model,
            "requested_effort": CRUCIAL_RISK_OVERRIDE["default_effort"],
            "effort_verified": False,
        },
    }, source="claude subscription canary")
    snapshot["provenance"] = {
        "kind": "subscription_runtime_canary",
        "auth_method": "claude.ai",
        "subscription_type": "pro",
    }
    snapshot_path = tmp_path / "claude-caps.json"
    snapshot_path.write_text(json.dumps(snapshot))

    result = router.main([
        "resolve", "--adapter", "claude", "--alias", CRUCIAL_RISK_OVERRIDE["alias"],
        "--role", CRUCIAL_RISK_OVERRIDE["roles"][0], "--model-override-tier", "crucial",
        "--capabilities-file", str(snapshot_path),
    ])

    receipt = json.loads(capsys.readouterr().out)
    assert router.CATALOG_PATH == catalog_path
    assert result == 0
    assert receipt["status"] == "ok"
    assert receipt["resolved_model"] == resolved_model
    assert receipt["model_override_tier"] == "crucial"
    assert receipt["route_source"] == "model-override"


def test_enabled_kiro_accepts_inferred_family_model():
    result, route = resolve(
        "--adapter", "kiro", "--alias", "principal", "--role", "worker",
        "--model", "deepseek-v3.2",
    )

    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["adapter_enabled"] is True


@pytest.mark.parametrize(
    ("risk_tier", "role_index", "effort"),
    (("crucial", 0, "medium"), ("terminal", 1, "low")),
)
def test_override_occupant_requires_explicit_bounded_risk_route(
    risk_tier, role_index, effort
):
    override = CATALOG["families"]["anthropic"]["risk_tier_overrides"][risk_tier]
    role = override["roles"][role_index]
    model = override["models"][0]
    result, route = resolve(
        "--adapter", "claude", "--alias", override["alias"], "--role", role,
        "--model-override-tier", risk_tier, "--effort", effort, "--available-model", model,
    )
    assert result.returncode == 0
    assert route["resolved_model"] == model
    assert route["model_override_tier"] == risk_tier
    assert route["route_source"] == "model-override"
    assert route["policy_override"] == (
        f"{risk_tier}-{model}-{'-'.join(override['roles'])}"
    )
    assert route["effort"] == effort


@pytest.mark.parametrize("risk_tier", ("crucial", "terminal"))
def test_fable_5_1_is_the_only_medium_capped_anthropic_override_model(risk_tier):
    override = CATALOG["families"]["anthropic"]["risk_tier_overrides"][risk_tier]
    assert override["models"] == ["claude-fable-5-1"]
    assert override["default_effort"] == override["maximum_effort"] == "medium"

    accepted, accepted_route = resolve(
        "--adapter", "claude", "--alias", override["alias"], "--role", override["roles"][0],
        "--model-override-tier", risk_tier, "--model", "claude-fable-5-1",
        "--effort", "medium", "--available-model", "claude-fable-5-1",
    )
    rejected, rejected_route = resolve(
        "--adapter", "claude", "--alias", override["alias"], "--role", override["roles"][0],
        "--model-override-tier", risk_tier, "--model", "fable",
        "--effort", "medium", "--available-model", "fable",
    )

    assert accepted.returncode == 0
    assert accepted_route["resolved_model"] == "claude-fable-5-1"
    assert rejected.returncode == 1
    assert rejected_route["status"] == "risk_tier_model_mismatch"


def test_fable_5_1_override_rejects_a_different_version():
    result, route = resolve(
        "--adapter", "claude", "--alias", "principal", "--role", "synthesis",
        "--model-override-tier", "crucial", "--model", "claude-fable-5-10",
        "--effort", "medium", "--available-model", "claude-fable-5-10",
    )

    assert result.returncode == 1
    assert route["status"] == "risk_tier_model_mismatch"


@pytest.mark.parametrize(
    ("arguments", "expected_status"),
    (
        (
            ("--alias", "principal", "--role", "synthesis", "--model-override-tier", "substantial"),
            "risk_tier_override_unavailable",
        ),
        (
            ("--alias", "principal", "--role", "worker", "--model-override-tier", "crucial"),
            "risk_tier_role_mismatch",
        ),
        (
            ("--alias", "senior", "--role", "synthesis", "--model-override-tier", "crucial"),
            "risk_tier_alias_mismatch",
        ),
        (
            (
                "--alias", "principal", "--role", "synthesis", "--model-override-tier", "crucial",
                "--effort", "high",
            ),
            "risk_tier_effort_above_ceiling",
        ),
    ),
)
def test_risk_override_fails_closed_outside_bounded_role_tier_and_effort(
    arguments, expected_status
):
    result, route = resolve(
        "--adapter", "claude", *arguments,
        "--available-model", RISK_OVERRIDE_MODEL,
    )
    assert result.returncode != 0
    assert route["status"] == expected_status


@pytest.mark.parametrize("model", NON_OCCUPANT_MODELS)
def test_risk_override_rejects_non_occupant_explicit_models(model):
    result, route = resolve(
        "--adapter", "claude", "--alias", "principal", "--role", "synthesis",
        "--model-override-tier", "crucial", "--model", model, "--effort", "medium",
    )
    assert result.returncode == 1
    assert route["status"] == "risk_tier_model_mismatch"


@pytest.mark.parametrize(
    ("field", "value"),
    (
        ("models", RISK_OVERRIDE_MODEL),
        ("models", [RISK_OVERRIDE_MODEL, "opus"]),
        ("models", []),
        ("models", [""]),
        ("models", ["  fable  "]),
        ("roles", CRUCIAL_RISK_OVERRIDE["roles"][0]),
        ("roles", [*CRUCIAL_RISK_OVERRIDE["roles"], "third-role"]),
        ("roles", ["synthesis", "adjudication", "synthesis"]),
        ("roles", []),
        ("roles", [CRUCIAL_RISK_OVERRIDE["roles"][0]]),
        ("roles", [CRUCIAL_RISK_OVERRIDE["roles"][0]] * 2),
        ("roles", [CRUCIAL_RISK_OVERRIDE["roles"][0], ""]),
        ("roles", ["synthesis", " synthesis"]),
        ("default_effort", "high"),
        ("default_effort", "unknown"),
        ("maximum_effort", "unknown"),
        ("alias", "unknown"),
    ),
)
def test_risk_tier_override_configuration_is_closed_and_bounded(
    tmp_path, monkeypatch, capsys, field, value
):
    router = load_router()
    catalog = product_catalog()
    catalog["families"]["anthropic"]["risk_tier_overrides"]["crucial"][field] = value
    catalog_path = use_catalog(router, monkeypatch, tmp_path, catalog)

    result = router.main([
        "resolve", "--adapter", "claude", "--alias", CRUCIAL_RISK_OVERRIDE["alias"],
        "--role", CRUCIAL_RISK_OVERRIDE["roles"][0], "--model-override-tier", "crucial",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "risk_tier_config_invalid"


@pytest.mark.parametrize("present,status", [
    pytest.param(True, "risk_tier_config_invalid", id="present-but-malformed"),
    pytest.param(False, "risk_tier_override_unavailable", id="absent"),
])
def test_malformed_and_absent_risk_tiers_remain_distinct(tmp_path, monkeypatch, capsys, present, status):
    router = load_router()
    catalog = product_catalog()
    overrides = catalog["families"]["anthropic"]["risk_tier_overrides"]
    if present:
        overrides["crucial"] = []
    else:
        overrides.pop("crucial")
    use_catalog(router, monkeypatch, tmp_path, catalog)
    result = router.main([
        "resolve", "--adapter", "claude", "--alias", "principal",
        "--role", "synthesis", "--model-override-tier", "crucial",
    ])
    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == status


@pytest.mark.parametrize(
    "malformed_models",
    [
        pytest.param("fable", id="string-instead-of-list"),
        pytest.param(["fable", "opus"], id="two-element-list"),
        pytest.param([], id="empty-list"),
        pytest.param([["fable"]], id="nested-list"),
        pytest.param(None, id="null"),
    ],
)
def test_malformed_override_models_never_unreserve_the_occupant(
    tmp_path, monkeypatch, capsys, malformed_models
):
    # Regression: a malformed shape used to be skipped by the catalogue-wide pass,
    # which left the tier unusable AND its occupant freely dispatchable — strictly
    # worse than either failure alone. It must fail closed instead.
    router = load_router()
    catalog = product_catalog()
    catalog["families"]["anthropic"]["risk_tier_overrides"]["crucial"]["models"] = malformed_models
    catalog_path = use_catalog(router, monkeypatch, tmp_path, catalog)

    result = router.main([
        "resolve", "--adapter", "claude", "--alias", "principal",
        "--role", "worker", "--model", "fable",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "risk_tier_config_invalid"


def test_malformed_override_in_one_family_does_not_reject_another_family(
    tmp_path, monkeypatch, capsys
):
    # The catalogue-wide pass is scoped to the adapter's own family, so an
    # Anthropic misconfiguration must not take OpenAI routing down with it.
    router = load_router()
    catalog = product_catalog()
    catalog["families"]["anthropic"]["risk_tier_overrides"]["crucial"]["models"] = "fable"
    snapshot = write_codex_capability_snapshot(tmp_path)
    catalog_path = use_catalog(router, monkeypatch, tmp_path, catalog)

    result = router.main([
        "resolve", "--adapter", "codex", "--alias", "senior",
        "--role", "worker", "--capabilities-file", str(snapshot),
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 0
    assert route["status"] == "ok"


@pytest.mark.parametrize(
    "adapter", ["claude", "codex", "opencode", "cursor", "agy"]
)
@pytest.mark.parametrize(
    "families", [None, [], "anthropic", {"anthropic": None}], ids=str
)
def test_unusable_families_table_fails_closed(
    tmp_path, monkeypatch, capsys, families, adapter
):
    """An unusable families table must reject, not route the reserved occupant.

    The reservation derives entirely from the families table, so a table that is
    not a usable mapping reserves nothing and the occupant routes freely. That is
    fail-open on malformed configuration, the same class of defect this issue
    exists to close, so the catalogue is rejected instead.

    Parametrised over adapters with and without a ``fixed_model_family``: the two
    take different validation paths, and the first fix guarded only the second.
    """
    router = load_router()
    catalog = product_catalog()
    catalog["families"] = families
    catalog_path = use_catalog(router, monkeypatch, tmp_path, catalog)

    result = router.main([
        "resolve", "--adapter", adapter, "--model", "fable",
        "--alias", "principal", "--role", "worker",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "risk_tier_config_invalid"
    assert route.get("resolved_model") is None


@pytest.mark.parametrize("adapter,alias,expected", [
    pytest.param("opencode", "principal", {"status": "ok", "resolved_model": "opencode/deepseek-v4.1-flash-free",
                                          "model_family": "deepseek"}, id="opencode-default"),
    pytest.param("cursor", "senior", {"resolved_model": "auto", "model_selection": "adapter-default",
                                     "model_family": "generic-open"}, id="cursor-auto"),
])
def test_adapter_without_model_uses_its_default(capsys, monkeypatch, adapter, alias, expected):
    if adapter == "opencode":
        compatibility = yaml.safe_load((ROOT / "config" / "adapter-compatibility.yaml").read_text())
        assert compatibility["adapters"]["opencode-acp"]["model_family_constraints"]["requires_explicit_model"] is False
    router = load_router()
    monkeypatch.setattr(router, "CATALOG_PATH", ROOT / "config" / "model-routing.json")
    monkeypatch.setattr(router, "PRODUCT_CATALOG_PATH", ROOT / "config" / "model-routing.json")
    result = router.main([
        "resolve", "--adapter", adapter, "--alias", alias, "--role", "worker",
        "--adapter-compatibility", str(ROOT / "config" / "adapter-compatibility.yaml"),
    ])
    route = json.loads(capsys.readouterr().out)
    assert result == 0
    for key, value in expected.items():
        assert route[key] == value


@pytest.mark.parametrize(
    "mutate, status",
    [
        (lambda families: families.pop("openai"), "model_required_for_broker"),
        # A family present but not a mapping is caught earlier still, by the
        # fixed-family override validation, which cannot read it either.
        (lambda families: families.__setitem__("openai", None), "risk_tier_config_invalid"),
        (lambda families: families["openai"].pop("aliases"), "model_required_for_broker"),
        (
            lambda families: families["openai"].__setitem__("aliases", []),
            "model_required_for_broker",
        ),
    ],
    ids=["family-absent", "family-null", "aliases-absent", "aliases-not-a-mapping"],
)
def test_unroutable_pinned_family_rejects_with_a_receipt(
    tmp_path, monkeypatch, capsys, mutate, status
):
    """A pinned family that cannot serve an alias rejects, and says so in JSON.

    These lookups were unguarded, so the router crashed part-way down with no
    output at all. A caller cannot act on a traceback: every rejection has to
    arrive as a receipt, whatever the catalogue looks like.
    """
    router = load_router()
    catalog = product_catalog()
    mutate(catalog["families"])
    catalog_path = use_catalog(router, monkeypatch, tmp_path, catalog)

    result = router.main([
        "resolve", "--adapter", "codex", "--alias", "principal",
        "--role", "worker",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == status
    assert route.get("resolved_model") is None


def test_empty_routed_family_validates_every_family_the_scan_consults(
    tmp_path, monkeypatch, capsys
):
    """An empty routed family narrows nothing, so validation must widen with the scan.

    ``override_scan_families`` only narrows to the model's own family when that
    family actually configures something. When it is empty the reservation scan
    falls back to every family, so a malformed override anywhere is one the scan
    reads. Validating the empty family alone would put the two back out of
    agreement, which is the disagreement this issue exists to close.
    """
    router = load_router()
    catalog = product_catalog()
    catalog["families"]["openai"] = {}
    catalog["families"]["anthropic"]["risk_tier_overrides"]["crucial"]["models"] = "fable"
    catalog_path = use_catalog(router, monkeypatch, tmp_path, catalog)

    # Every family the catalogue defines, named from the catalogue rather than
    # repeated here: the invariant is that the scan widens to all of them, which
    # a hardcoded pair silently converts into a count that breaks whenever a
    # family is added.
    assert list(router.override_scan_families("gpt-5.6-sol", catalog)) == list(catalog["families"])

    result = router.main([
        "resolve", "--adapter", "cursor", "--alias", "principal", "--role", "worker",
        "--model", "gpt-5.6-sol",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "risk_tier_config_invalid"


@pytest.mark.parametrize("aliases", ["x", [], None], ids=str)
def test_unusable_alias_table_rejects_on_an_explicit_model_route(
    tmp_path, monkeypatch, capsys, aliases
):
    """The explicit-model path reads the alias table too, and must survive a bad one.

    Guarding only the alias-only route left this one dereferencing the same
    unusable table, so it still crashed without JSON. Both readers are now fed
    from one normalised load site.
    """
    router = load_router()
    catalog = product_catalog()
    catalog["adapters"]["account-default-fixture"] = {
        "endpoint_provider": "fixture",
        "fixed_model_family": "openai",
        "effort_transport": "flag",
        "model_selection": "account-default",
    }
    catalog["families"]["openai"]["aliases"] = aliases
    catalog_path = use_catalog(router, monkeypatch, tmp_path, catalog)

    result = router.main([
        "resolve", "--adapter", "account-default-fixture", "--alias", "principal",
        "--role", "worker", "--model", "gpt-5.6-sol",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 1
    assert route["status"] == "adapter_account_default_only"


@pytest.mark.parametrize(
    "field, value",
    [
        ("roles", "ab"),
        ("alias", ["principal"]),
        ("default_effort", ["high"]),
        ("maximum_effort", {"high": True}),
    ],
    ids=["roles-string", "alias-list", "default-effort-list", "maximum-effort-dict"],
)
def test_override_field_of_the_wrong_type_fails_closed(
    tmp_path, monkeypatch, capsys, field, value
):
    """Each type check in the override validator is load-bearing on its own.

    Without them these shapes do not merely slip through, they duck the whole
    validator: ``roles`` as a two-character string satisfies every downstream
    length, membership and uniqueness check, and an unhashable alias or effort
    raises out of the ``in`` test rather than failing closed.
    """
    router = load_router()
    catalog = product_catalog()
    catalog["families"]["anthropic"]["risk_tier_overrides"]["crucial"][field] = value
    catalog_path = use_catalog(router, monkeypatch, tmp_path, catalog)

    result = router.main([
        "resolve", "--adapter", "claude", "--alias", "principal", "--role", "a",
        "--model-override-tier", "crucial", "--model", "fable", "--available-model", "fable",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "risk_tier_config_invalid"
    assert route.get("resolved_model") is None


def test_fixed_family_adapter_fails_closed_on_an_alias_route(
    tmp_path, monkeypatch, capsys
):
    """The fixed-family guard is what covers alias routes, which name no model.

    Mutation testing found this guard unpinned: deleting it left every test
    green. An alias route never reaches the model-family validation, because
    there is no model to infer a family from, so only this guard stands between
    a malformed catalogue and a routed alias.
    """
    router = load_router()
    catalog = product_catalog()
    for tier in catalog["families"]["anthropic"]["risk_tier_overrides"].values():
        tier["models"] = "fable"
    catalog_path = use_catalog(router, monkeypatch, tmp_path, catalog)

    result = router.main([
        "resolve", "--adapter", "claude", "--alias", "principal", "--role", "worker",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "risk_tier_config_invalid"


@pytest.mark.parametrize("adapter", ["cursor", "agy", "opencode"])
def test_malformed_override_fails_closed_without_a_fixed_model_family(
    tmp_path, monkeypatch, capsys, adapter
):
    """The occupant stays reserved for adapters that declare no fixed family.

    The catalogue validation keyed off the adapter's ``fixed_model_family`` while
    the reservation scan keys off the family inferred from the requested model.
    An adapter without a fixed family therefore validated nothing at all, yet
    still scanned the model's own family, so a malformed ``models`` value
    unreserved the occupant and resolved it at exit 0.
    """
    router = load_router()
    catalog = product_catalog()
    for tier in catalog["families"]["anthropic"]["risk_tier_overrides"].values():
        tier["models"] = "fable"
    catalog_path = use_catalog(router, monkeypatch, tmp_path, catalog)

    result = router.main([
        "resolve", "--adapter", adapter, "--model", "fable",
        "--alias", "principal", "--role", "worker",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "risk_tier_config_invalid"
    assert route.get("resolved_model") is None


def test_account_default_aliases_resolve_to_account_default_dispatch(tmp_path):
    expected = {
        "principal": "gpt-6-astra",
        "senior": "gpt-6.1-sol",
        "junior": "gpt-6-luna",
    }
    catalog = write_account_default_catalog(tmp_path)
    for alias, model in expected.items():
        result, route = resolve(
            "--adapter", "account-default-fixture", "--alias", alias,
            "--role", "worker", "--catalog", str(catalog),
        )
        assert result.returncode == 0
        assert route["resolved_model"] == ""
        assert route["catalog_model"] == model
        assert route["model_selection"] == "account-default"
        assert route["identity_source"] == "account-default"
        assert route["model_family"] == "openai"


def test_account_default_adapter_ignores_runtime_selectable_model_list(tmp_path):
    catalog = write_account_default_catalog(tmp_path)
    result, route = resolve(
        "--adapter", "account-default-fixture", "--alias", "principal",
        "--role", "worker", "--available-model", "gpt-5.6-luna",
        "--catalog", str(catalog),
    )
    assert result.returncode == 0
    assert route["resolved_model"] == ""
    assert route["catalog_model"] == "gpt-6-astra"
    assert route["model_selection"] == "account-default"


@pytest.mark.parametrize(
    ("catalog_account_default", "requires_explicit_model"),
    ((True, True), (False, False)),
)
def test_catalog_and_compatibility_mismatch_fails_closed(
    tmp_path, monkeypatch, capsys, catalog_account_default, requires_explicit_model
):
    router = load_router()
    catalog = product_catalog()
    adapter = catalog["adapters"]["codex"]
    if catalog_account_default:
        adapter["model_selection"] = "account-default"
    else:
        adapter.pop("model_selection", None)
    catalog_path = use_catalog(router, monkeypatch, tmp_path, catalog)
    compatibility_path = write_codex_compatibility(
        tmp_path, requires_explicit_model=requires_explicit_model
    )

    result = router.main([
        "resolve", "--adapter", "codex", "--alias", "junior", "--role", "worker",
        "--adapter-compatibility", str(compatibility_path),
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "account_default_conflicts_with_compatibility"


def test_codex_aliases_supply_proportionate_default_effort(tmp_path):
    # senior and junior are deliberately not proportionate on OpenAI. Luna is
    # cheap enough that the family raises both worker aliases to high in
    # role_effort_defaults, so the tier difference against principal is the
    # model, not the effort. See docs/model-dossier.md.
    expected = {"principal": "high", "senior": "high", "junior": "high"}
    snapshot = write_codex_capability_snapshot(tmp_path)
    for alias, effort in expected.items():
        result, route = resolve(
            "--adapter", "codex", "--alias", alias, "--role", "worker",
            "--capabilities-file", str(snapshot),
        )
        assert result.returncode == 0
        assert route["effort"] == effort


@pytest.mark.parametrize(
    ("task_class", "alias", "effort", "resolved_model"),
    (
        ("mechanical", "junior", "low", "gpt-6-luna"),
        ("legwork", "senior", "medium", "gpt-6.1-sol"),
        ("critical-review", "principal", "high", "gpt-6-astra"),
        ("orchestration", "principal", "high", "gpt-6-astra"),
    ),
)
def test_task_classes_bind_codex_runtime_identity(
    tmp_path, task_class, alias, effort, resolved_model
):
    snapshot = tmp_path / f"{task_class}.json"
    snapshot.write_text(json.dumps(capability_snapshot({
        resolved_model: {
            "resolved_model": resolved_model,
            "supported_efforts": [effort],
        },
    })))
    result, route = resolve(
        "--adapter", "codex", "--task-class", task_class,
        "--role", "orchestrator" if task_class == "orchestration" else "critical-review" if task_class == "critical-review" else "worker",
        "--capabilities-file", str(snapshot),
    )
    assert result.returncode == 0
    assert route["task_class"] == task_class
    assert route["route_source"] == "task-class"
    assert route["alias"] == alias
    assert route["requested_effort"] == effort
    assert route["effort"] == effort
    assert route["resolved_model"] == resolved_model
    assert route["identity_source"] == "runtime-capability+catalog"
    assert "catalog_model" not in route
    assert "model_selection" not in route


def test_task_class_uses_ordered_exact_models_from_catalogue(tmp_path):
    catalog = product_catalog()
    catalog["task_class_routes"]["implementation"] = {
        "alias": "senior", "effort": "high", "role": "worker",
        "models": {"codex": ["gpt-6-astra", "gpt-6.1-sol"]},
    }
    catalog_path = tmp_path / "catalog.json"
    catalog_path.write_text(json.dumps(catalog))
    capabilities = tmp_path / "caps.json"
    capabilities.write_text(json.dumps(capability_snapshot({
        "gpt-6.1-sol": {"resolved_model": "gpt-6.1-sol", "supported_efforts": ["high"]},
    })))
    result, route = resolve(
        "--adapter", "codex", "--task-class", "implementation", "--role", "worker",
        "--catalog", str(catalog_path), "--capabilities-file", str(capabilities),
    )
    assert result.returncode == 0
    assert route["resolved_model"] == "gpt-6.1-sol"
    assert route["configured_models"] == ["gpt-6-astra", "gpt-6.1-sol"]


@pytest.mark.parametrize(
    ("task_class", "expected_model"),
    [
        (name, route["models"]["claude"][0])
        for name, route in CATALOG["task_class_routes"].items()
        if route.get("models", {}).get("claude")
    ],
)
def test_claude_task_classes_resolve_the_first_catalogued_model(
    tmp_path, task_class, expected_model
):
    task_route = CATALOG["task_class_routes"][task_class]
    effort = task_route["effort"]
    models = {
        model: {
            "resolved_model": model,
            "requested_effort": effort,
            "effort_verified": False,
        }
        for model in task_route["models"]["claude"]
    }
    snapshot_value = capability_snapshot(models, source="claude subscription canary")
    snapshot_value["provenance"] = {
        "kind": "subscription_runtime_canary",
        "auth_method": "claude.ai",
        "subscription_type": "pro",
    }
    snapshot = tmp_path / f"{task_class}-claude-caps.json"
    snapshot.write_text(json.dumps(snapshot_value))

    result, route = resolve(
        "--adapter", "claude", "--task-class", task_class,
        "--role", task_route["role"], "--capabilities-file", str(snapshot),
    )

    assert result.returncode == 0, route
    assert route["task_class"] == task_class
    assert route["resolved_model"] == expected_model


def test_task_class_rejects_adapter_alias_instead_of_exact_model_id(tmp_path):
    catalog = product_catalog()
    catalog["task_class_routes"]["implementation"]["models"]["codex"] = ["sol"]
    catalog_path = tmp_path / "catalog.json"
    catalog_path.write_text(json.dumps(catalog))
    result, route = resolve(
        "--adapter", "codex", "--task-class", "implementation", "--role", "worker",
        "--catalog", str(catalog_path),
    )
    assert result.returncode == 2
    assert route["status"] == "task_class_config_invalid"


def test_failed_review_route_gets_one_exact_alternate(tmp_path, monkeypatch):
    catalog = product_catalog()
    catalog["task_class_routes"]["critical-review"]["models"]["codex"] = ["gpt-6-astra", "gpt-6.1-sol"]
    catalog_path = tmp_path / "catalog.json"
    catalog_path.write_text(json.dumps(catalog))
    state = tmp_path / "state"
    state.mkdir()
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(state))
    caps = tmp_path / "caps.json"
    caps.write_text(json.dumps(capability_snapshot({
        "gpt-6-astra": {"resolved_model": "gpt-6-astra", "supported_efforts": ["high"]},
        "gpt-6.1-sol": {"resolved_model": "gpt-6.1-sol", "supported_efforts": ["high"]},
    })))
    first, initial = resolve(
        "--adapter", "codex", "--task-class", "critical-review", "--role", "critical-review",
        "--catalog", str(catalog_path), "--capabilities-file", str(caps),
    )
    assert first.returncode == 0
    assert initial["resolved_model"] == "gpt-6-astra"
    assert initial["fallback_candidates"] == [{"adapter": "codex", "model": "gpt-6.1-sol", "effort": "high"}]
    (state / "route-health.json").write_text(json.dumps({
        "schema": "fabric.route-health.v1",
        "routes": {"codex|gpt-6-astra|critical-review": {
            "adapter": "codex", "model": "gpt-6-astra", "task_class": "critical-review",
            # Relative to now: route health only counts failures from the last seven days.
            "recent": [{"status": "empty_output",
                        "at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")}],
        }},
    }))
    result, route = resolve(
        "--adapter", "codex", "--task-class", "critical-review", "--role", "critical-review",
        "--catalog", str(catalog_path), "--capabilities-file", str(caps),
    )
    assert result.returncode == 0
    assert route["resolved_model"] == "gpt-6.1-sol"
    assert route["configured_models"] == ["gpt-6-astra", "gpt-6.1-sol"]


@pytest.mark.parametrize(
    ("adapter", "models"),
    [
        ("codex", ["gpt-6-astra", "gpt-6.1-sol"]),
        ("agy", ["gemini-3.8-flash"]),
    ],
)
def test_exhausted_review_roster_fails_closed(tmp_path, monkeypatch, adapter, models):
    state = tmp_path / "state"
    state.mkdir()
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(state))
    now = datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")
    (state / "route-health.json").write_text(json.dumps({
        "schema": "fabric.route-health.v1",
        "routes": {
            f"{adapter}|{model}|review": {
                "adapter": adapter, "model": model, "task_class": "review",
                "recent": [{"status": "failed", "at": now}],
            }
            for model in models
        },
    }))
    capability_models = {
        model: {"resolved_model": model, "supported_efforts": ["high"]}
        for model in models
    }
    snapshot = (
        write_agy_capability_snapshot(tmp_path, models=capability_models)
        if adapter == "agy"
        else write_codex_capability_snapshot(tmp_path, models=capability_models)
    )

    result, route = resolve(
        "--adapter", adapter, "--task-class", "review", "--role", "reviewer",
        "--capabilities-file", str(snapshot),
    )

    assert result.returncode == 1
    assert route["status"] == "no_candidate_available"
    assert route["effort"] == ""


@pytest.mark.parametrize("models,trusted,status,empty_effort", [
    pytest.param({"opus": {"resolved_model": "opus", "supported_efforts": ["high"]}}, False,
                 "capability_snapshot_untrusted", False, id="caller-authored"),
    pytest.param({"opus": {"resolved_model": "claude-opus-4-8", "requested_effort": "high", "effort_verified": False}},
                 True, "no_candidate_available", False, id="probed-unverified"),
    pytest.param({"opus": {"resolved_model": "claude-haiku-4-5-opus", "requested_effort": "high", "effort_verified": False}},
                 True, "capability_discovery_failed", True, id="unanchored-alias"),
    pytest.param({"opus": {"resolved_model": "claude-opus-4-8-fable", "requested_effort": "high", "effort_verified": False},
                  "fable": {"resolved_model": "claude-opus-4-8-fable", "requested_effort": "low", "effort_verified": False}},
                 True, "capability_discovery_failed", True, id="alias-key-collision"),
    pytest.param({"opus": {"resolved_model": "claude-opus-4-8", "requested_effort": "low", "effort_verified": False}},
                 True, "no_candidate_available", False, id="different-probed-effort"),
])
def test_claude_task_class_capability_evidence(tmp_path, models, trusted, status, empty_effort):
    snapshot = tmp_path / "claude-caps.json"
    value = capability_snapshot(models, source="claude subscription canary")
    if trusted:
        value["provenance"] = {"kind": "subscription_runtime_canary", "auth_method": "claude.ai", "subscription_type": "pro"}
    snapshot.write_text(json.dumps(value))
    result, route = resolve(
        "--adapter", "claude", "--task-class", "critical-review",
        "--role", "critical-review", "--capabilities-file", str(snapshot),
    )
    assert result.returncode == 1
    assert route["status"] == status
    if empty_effort:
        assert route["effort"] == ""


@pytest.mark.parametrize("effort,code,status,applied", [
    pytest.param("high", 0, "ok", "high", id="same-probed-effort"),
    pytest.param("medium", 1, "effort_capability_unverified", "", id="different-effort"),
])
def test_claude_alias_route_preserves_unverified_effort(tmp_path, effort, code, status, applied):
    snapshot = tmp_path / "claude-caps.json"
    value = capability_snapshot({
        "opus": {"resolved_model": "claude-opus-4-8", "requested_effort": "high", "effort_verified": False},
    }, source="claude subscription canary")
    value["provenance"] = {"kind": "subscription_runtime_canary", "auth_method": "claude.ai", "subscription_type": "pro"}
    snapshot.write_text(json.dumps(value))
    result, route = resolve(
        "--adapter", "claude", "--alias", "principal", "--role", "critical-review",
        "--effort", effort, "--capabilities-file", str(snapshot),
    )
    assert result.returncode == code
    assert route["status"] == status
    assert route["effort"] == applied
    assert route["effort_capability_source"] == "provider-unverified"
    if status == "ok":
        assert route["resolved_model"] == "claude-opus-4-8"
        assert route["requested_effort"] == route["effort"] == "high"


def test_task_class_without_trusted_capability_evidence_fails_closed():
    result, route = resolve(
        "--adapter", "claude", "--task-class", "critical-review",
        "--role", "critical-review", "--available-model", "opus",
    )
    assert result.returncode == 1
    assert route["status"] == "no_candidate_available"


def test_critical_review_task_class_keeps_worker_role_as_a_label():
    result, route = resolve(
        "--adapter", "claude", "--task-class", "critical-review", "--role", "worker"
    )
    assert route["status"] != "task_class_role_mismatch"
    assert route["role"] == "worker"
    assert route["task_class"] == "critical-review"


def test_legacy_alias_route_does_not_claim_task_class_binding(tmp_path):
    snapshot = write_codex_capability_snapshot(tmp_path)
    result, route = resolve(
        "--adapter", "codex", "--alias", "junior", "--role", "worker",
        "--capabilities-file", str(snapshot),
    )
    assert result.returncode == 0
    assert "task_class" not in route
    assert "route_source" not in route


@pytest.mark.parametrize(
    "arguments,expected",
    (
        (("--alias", "junior", "--task-class", "mechanical"), "route_input_conflict"),
        (("--task-class", "mechanical", "--effort", "medium"), "task_class_effort_conflict"),
        (("--task-class", "unknown"), "unknown_task_class"),
    ),
)
def test_task_class_rejects_ambiguous_or_unknown_routing_inputs(arguments, expected):
    result, route = resolve("--adapter", "codex", *arguments, "--role", "worker")
    assert result.returncode == 2
    assert route["schema_version"] == 1
    assert route["adapter"] == "codex"
    assert route["role"] == "worker"
    assert route["status"] == expected


def test_task_class_rejects_explicit_model_override():
    result, route = resolve(
        "--adapter", "claude", "--task-class", "critical-review",
        "--role", "critical-review", "--model", "haiku",
    )
    assert result.returncode == 2
    assert route["status"] == "task_class_model_conflict"
    assert route["alias"] == "principal"


def test_task_class_rejects_effective_effort_below_policy_floor(tmp_path):
    snapshot = tmp_path / "caps.json"
    snapshot.write_text(json.dumps(capability_snapshot({
        "gpt-6-astra": {
            "resolved_model": "gpt-6-astra",
            "supported_efforts": ["low"],
        },
    })))

    result, route = resolve(
        "--adapter", "codex", "--task-class", "critical-review",
        "--role", "critical-review", "--capabilities-file", str(snapshot),
    )

    assert result.returncode == 1
    assert route["status"] == "task_class_effort_below_floor"
    assert route["requested_effort"] == "high"
    assert route["effort"] == ""


@pytest.mark.parametrize("field", ["effort", "minimum_effort"])
def test_invalid_task_class_effort_vocabulary_fails_closed(tmp_path, monkeypatch, capsys, field):
    router = load_router()
    catalog = product_catalog()
    catalog["task_class_routes"]["critical-review"][field] = "hgh"
    catalog_path = use_catalog(router, monkeypatch, tmp_path, catalog)

    result = router.main([
        "resolve", "--adapter", "claude", "--task-class", "critical-review",
        "--role", "critical-review",
    ])

    route = json.loads(capsys.readouterr().out)
    assert route["status"] == "task_class_config_invalid"
    assert result == 2
    assert route["effort"] == ""


def test_task_class_effort_below_configured_minimum_fails_closed(tmp_path, monkeypatch, capsys):
    router = load_router()
    catalog = product_catalog()
    catalog['task_class_routes']['research']['minimum_effort'] = 'high'
    use_catalog(router, monkeypatch, tmp_path, catalog)
    result = router.main(['resolve', '--adapter', 'codex', '--task-class', 'research', '--role', 'worker'])
    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route['status'] == 'task_class_config_invalid'
    assert route['effort'] == ''


def test_task_class_role_label_does_not_change_class_tier(tmp_path, monkeypatch, capsys):
    router = load_router()
    catalog = product_catalog()
    catalog["task_class_routes"]["critical-review"]["role"] = "worker"
    catalog_path = use_catalog(router, monkeypatch, tmp_path, catalog)

    result = router.main([
        "resolve", "--adapter", "claude", "--task-class", "critical-review",
        "--role", "worker",
    ])

    route = json.loads(capsys.readouterr().out)
    assert route["status"] != "task_class_config_invalid"
    assert result in (0, 1)


@pytest.mark.parametrize(
    ("field", "value"),
    (("alias", "junior"), ("effort", "low")),
)
def test_task_class_route_tier_is_configurable(
    tmp_path, monkeypatch, capsys, field, value
):
    router = load_router()
    catalog = product_catalog()
    catalog["task_class_routes"]["critical-review"][field] = value
    if field == "effort":
        catalog["task_class_routes"]["critical-review"]["minimum_effort"] = value
        catalog["task_class_routes"]["critical-review"]["models"]["claude"] = ["claude-haiku-5-5"]
    catalog_path = use_catalog(router, monkeypatch, tmp_path, catalog)

    result = router.main([
        "resolve", "--adapter", "claude", "--task-class", "critical-review", "--role", "critical-review",
    ])
    route = json.loads(capsys.readouterr().out)
    assert route["status"] != "task_class_config_invalid"
    assert result == 1
    assert route["alias"] == ("junior" if field == "alias" else "principal")
    assert route["requested_effort"] == ("low" if field == "effort" else "high")


def test_task_class_effort_overlay_may_raise_effort_above_policy_floor(
    tmp_path, monkeypatch, capsys
):
    router = load_router()
    catalog = product_catalog()
    catalog["task_class_routes"]["critical-review"]["effort"] = "xhigh"
    catalog_path = use_catalog(router, monkeypatch, tmp_path, catalog)

    result = router.main([
        "resolve", "--adapter", "claude", "--task-class", "critical-review",
        "--role", "critical-review",
    ])

    route = json.loads(capsys.readouterr().out)
    assert route["status"] != "task_class_config_invalid"
    assert route["requested_effort"] == "xhigh"
    assert result == 1


def test_role_default_cannot_lower_task_class_effort(tmp_path, monkeypatch, capsys):
    router = load_router()
    catalog = product_catalog()
    catalog["families"]["openai"]["role_effort_defaults"]["orchestrator"]["principal"] = "low"
    snapshot = tmp_path / "caps.json"
    snapshot.write_text(json.dumps(capability_snapshot({
        "gpt-6-astra": {"resolved_model": "gpt-6-astra", "supported_efforts": ["high"]},
    })))
    catalog_path = use_catalog(router, monkeypatch, tmp_path, catalog)

    result = router.main([
        "resolve", "--adapter", "codex", "--task-class", "orchestration",
        "--role", "orchestrator", "--capabilities-file", str(snapshot),
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 0
    assert route["requested_effort"] == "high"
    assert route["effort_source"] == "task-class"


def test_explicit_ultra_without_runtime_snapshot_uses_registry(
    monkeypatch, capsys
):
    router = load_router()
    monkeypatch.setattr(router, "CATALOG_PATH", ROOT / "config" / "model-routing.json")
    monkeypatch.setattr(router, "PRODUCT_CATALOG_PATH", ROOT / "config" / "model-routing.json")

    result = router.main([
        "resolve", "--adapter", "codex", "--alias", "principal", "--role", "lead",
        "--effort", "ultra",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 0
    assert route["status"] == "ok"
    assert route["effort"] == "ultra"
    assert route["requested_effort"] == "ultra"


def test_explicit_ultra_with_stale_openai_capability_snapshot_fails_closed(
    tmp_path, monkeypatch, capsys
):
    router = load_router()
    monkeypatch.setattr(router, "CATALOG_PATH", ROOT / "config" / "model-routing.json")
    monkeypatch.setattr(router, "PRODUCT_CATALOG_PATH", ROOT / "config" / "model-routing.json")
    observed_at = (
        datetime.now(timezone.utc) - timedelta(minutes=6)
    ).isoformat().replace("+00:00", "Z")
    snapshot = write_codex_capability_snapshot(tmp_path, observed_at=observed_at)

    result = router.main([
        "resolve", "--adapter", "codex", "--alias", "principal", "--role", "lead",
        "--effort", "ultra",
        "--capabilities-file", str(snapshot),
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 1
    assert route["status"] == "capability_snapshot_stale"
    assert route["requested_effort"] == "ultra"
    assert route["effort"] == ""


def test_openai_catalog_declares_effort_policy_only():
    family = CATALOG["families"]["openai"]

    assert "supported_efforts" not in family
    assert "ultra_eligible_models" not in family
    assert family["ultra_eligible_roles"] == ["lead", "orchestrator"]
    assert family["role_effort_defaults"] == {
        "lead": {"principal": "xhigh"},
        "orchestrator": {"principal": "xhigh"},
        "critical-review": {"principal": "xhigh"},
        "worker": {"senior": "high", "junior": "high"},
    }
    assert family["effort_fallback_order"] == ["xhigh", "high", "medium", "low"]


def test_codex_capabilities_documents_historical_no_effort_status():
    reference = (
        ROOT / "skills" / "orchestrate" / "references" / "codex-capabilities.md"
    ).read_text()

    assert "`no_effort_available`" in reference, (
        "historical resolver guidance omits no_effort_available"
    )


def test_fresh_openai_snapshot_accepts_explicit_ultra_effort(
    tmp_path, monkeypatch, capsys
):
    router = load_router()
    monkeypatch.setattr(router, "CATALOG_PATH", ROOT / "config" / "model-routing.json")
    monkeypatch.setattr(router, "PRODUCT_CATALOG_PATH", ROOT / "config" / "model-routing.json")
    snapshot = write_codex_capability_snapshot(tmp_path)

    result = router.main([
        "resolve", "--adapter", "codex", "--alias", "principal", "--role", "lead",
        "--effort", "ultra",
        "--capabilities-file", str(snapshot),
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 0
    assert route["requested_effort"] == "ultra"
    assert route["effort"] == "ultra"
    assert route["effort_capability_source"] == "runtime-model-catalog"
    assert route["effort_substitution"] == ""


def test_noneligible_ultra_fallback_reports_runtime_capability_source(
    tmp_path, monkeypatch, capsys
):
    router = load_router()
    catalog = product_catalog()
    catalog["families"]["openai"]["role_effort_defaults"]["worker"] = {
        "principal": "ultra"
    }
    catalog_path = use_catalog(router, monkeypatch, tmp_path, catalog)
    snapshot = write_codex_capability_snapshot(
        tmp_path,
        models={
            "gpt-6-astra": {
                "resolved_model": "gpt-6-astra",
                "supported_efforts": ["xhigh", "max"],
            },
        },
    )

    result = router.main([
        "resolve", "--adapter", "codex", "--alias", "principal", "--role", "worker",
        "--capabilities-file", str(snapshot),
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 0
    assert route["requested_effort"] == "ultra"
    assert route["effort"] == "xhigh"
    assert route["effort_capability_source"] == "runtime-model-catalog"


def test_probe_snapshot_max_is_dropped_from_the_supported_efforts(tmp_path):
    snapshot = write_codex_capability_snapshot(
        tmp_path,
        models={"gpt-6-astra": {"resolved_model": "gpt-6-astra",
                                "supported_efforts": ["high", "max"]}},
    )
    result, route = resolve(
        "--adapter", "codex", "--alias", "principal", "--role", "worker",
        "--effort", "ultra", "--capabilities-file", str(snapshot),
    )
    assert result.returncode == 0
    assert route["effort"] == route["effort_applied"] == "high"
    assert "max" not in route["supported_efforts"]


@pytest.mark.parametrize("argv", [
    ["--adapter", "claude", "--alias", "principal", "--role", "worker", "--effort", "max"],
    ["--adapter", "claude", "--model", "sonnet", "--role", "worker", "--effort", "MAX"],
    ["--adapter", "codex", "--model", "gpt-6-luna", "--role", "worker", "--effort", "max"],
    ["--adapter", "opencode", "--model", "opencode-go/deepseek-v4.1-flash#max", "--role", "worker"],
    ["--adapter", "opencode", "--model", "opencode-go/deepseek-v4.1-flash#max", "--role", "worker",
     "--effort", "high"],
])
def test_a_request_for_max_effort_fails_and_suggests_xhigh(argv):
    result, route = resolve(*argv)
    assert result.returncode == 2
    assert route["status"] == "invalid_effort"
    assert "'max' was retired" in route["message"] and "xhigh" in route["message"]


def test_an_overlay_that_still_lists_max_fails_fast(tmp_path):
    catalog = product_catalog()
    catalog["families"]["openai"]["effort_fallback_order"].append("max")
    instance = tmp_path / "instance"
    (instance / "config").mkdir(parents=True)
    (instance / "config" / "model-routing.json").write_text(json.dumps({"families": catalog["families"]}))
    env = {**os.environ, "AGENT_FABRIC_INSTANCE_ROOT": str(instance),
           "AGENT_FABRIC_STATE_ROOT": str(tmp_path / "state")}
    run = subprocess.run([str(SCRIPT), "resolve", "--adapter", "claude", "--alias", "senior",
                          "--role", "worker"], text=True, capture_output=True, env=env, check=False)
    assert run.returncode == 2
    assert "effort 'max' was retired" in run.stderr and "xhigh" in run.stderr


def test_ultra_eligible_roles_must_be_a_list_of_role_names(
    tmp_path, monkeypatch, capsys
):
    router = load_router()
    catalog = product_catalog()
    catalog["families"]["openai"]["ultra_eligible_roles"] = "lead"
    snapshot = write_codex_capability_snapshot(tmp_path)
    catalog_path = use_catalog(router, monkeypatch, tmp_path, catalog)

    result = router.main([
        "resolve", "--adapter", "codex", "--alias", "principal", "--role", "lead",
        "--capabilities-file", str(snapshot),
    ])

    route = json.loads(capsys.readouterr().out)
    assert route["status"] == "effort_policy_config_invalid"
    assert result == 2
    assert "configuration error" in route["message"]
    assert "ultra_eligible_roles must be a list" in route["message"]


def test_explicit_effort_overrides_codex_xhigh_default(tmp_path):
    snapshot = write_codex_capability_snapshot(tmp_path)
    result, route = resolve(
        "--adapter", "codex", "--alias", "principal", "--role", "lead", "--effort", "high",
        "--capabilities-file", str(snapshot),
    )
    assert result.returncode == 0
    assert route["effort"] == "high"


def test_explicit_ultra_maps_to_supported_effort_on_ordinary_routes(tmp_path):
    snapshot = write_codex_capability_snapshot(tmp_path)
    cases = [
        (
            "--adapter", "codex", "--alias", "senior", "--role", "worker",
            "--capabilities-file", str(snapshot),
        ),
        ("--adapter", "claude", "--alias", "principal", "--role", "worker"),
    ]
    for route_args in cases:
        result, route = resolve(*route_args, "--effort", "ultra")
        assert result.returncode == 0
        assert route["status"] == "ok"
        assert route["requested_effort"] == "ultra"
        assert route["effort"] in {"ultra", "xhigh"}


@pytest.mark.parametrize("model_args,effort,model,identity,field,value", [
    pytest.param([], "ultra", "gpt-6-luna", "runtime-capability+catalog", "effort", "xhigh", id="runtime-alias"),
    pytest.param(["--model", "gpt-5.6-luna"], "low", "gpt-5.6-luna", "model-pattern", "model_family", "openai", id="explicit-model"),
])
def test_codex_runtime_identity_fields(tmp_path, model_args, effort, model, identity, field, value):
    snapshot = write_codex_capability_snapshot(tmp_path)
    result, route = resolve(
        "--adapter", "codex", "--alias", "junior", "--role", "worker", *model_args,
        "--effort", effort, "--capabilities-file", str(snapshot),
    )
    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["resolved_model"] == model
    assert route["identity_source"] == identity
    assert route[field] == value
    assert "catalog_model" not in route
    assert "model_selection" not in route


def test_caller_efforts_do_not_replace_openai_capability_snapshot():
    result, route = resolve(
        "--adapter",
        "codex",
        "--alias",
        "principal",
        "--role",
        "lead",
        "--available-effort",
        "max",
        "--available-effort",
        "high",
    )
    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["requested_effort"] == "xhigh"
    assert route["effort"] == "xhigh"


@pytest.mark.parametrize("model,efforts,code,expected", [
    pytest.param("gpt-6-astra", ["medium", "high", "max"], 0,
                 {"effort": "high", "effort_capability_source": "runtime-model-catalog"}, id="default-falls-back"),
    pytest.param("gpt-5.6-terra", ["high", "xhigh", "max"], 1,
                 {"status": "no_candidate_available", "candidates": ["gpt-6-astra"], "effort": ""}, id="no-alias-candidate"),
])
def test_runtime_snapshot_controls_alias_candidates(tmp_path, monkeypatch, capsys, model, efforts, code, expected):
    router = load_router()
    monkeypatch.setattr(router, "CATALOG_PATH", ROOT / "config" / "model-routing.json")
    monkeypatch.setattr(router, "PRODUCT_CATALOG_PATH", ROOT / "config" / "model-routing.json")
    snapshot = tmp_path / "caps.json"
    # max is retired, so the first case must step down from xhigh to high.
    snapshot.write_text(json.dumps(capability_snapshot({model: {"resolved_model": model, "supported_efforts": efforts}})))
    result = router.main([
        "resolve", "--adapter", "codex", "--alias", "principal", "--role", "lead", "--capabilities-file", str(snapshot),
    ])
    route = json.loads(capsys.readouterr().out)
    assert result == code
    assert route["requested_effort"] == "xhigh"
    for key, value in expected.items():
        assert route[key] == value


@pytest.mark.parametrize("model,efforts,selector,code,requested,applied,substitution,status", [
    pytest.param("gpt-6-astra", ["max", "medium"], ["--alias", "principal"], 0, "high", "medium",
                 "high unavailable (runtime/model capability); used medium", None, id="default-steps-down"),
    pytest.param("gpt-6-luna", ["max"], ["--task-class", "mechanical"], 1, "low", "", "",
                 "no_effort_available", id="task-class-never-escalates"),
])
def test_effort_fallback_never_exceeds_request(tmp_path, model, efforts, selector, code, requested, applied, substitution, status):
    snapshot = write_codex_capability_snapshot(
        tmp_path, models={model: {"resolved_model": model, "supported_efforts": efforts}},
    )
    result, route = resolve("--adapter", "codex", *selector, "--role", "worker", "--capabilities-file", str(snapshot))
    assert result.returncode == code
    assert route["requested_effort"] == requested
    assert route["effort"] == applied
    assert route["effort_substitution"] == substitution
    if status is not None:
        assert route["status"] == status


def test_explicit_unsupported_effort_maps_against_runtime_snapshot(tmp_path):
    snapshot = tmp_path / "caps.json"
    snapshot.write_text(json.dumps(capability_snapshot({
            "gpt-6-astra": {
                "resolved_model": "gpt-6-astra",
                "supported_efforts": ["high", "xhigh", "max"],
            }
        })))
    result, route = resolve(
        "--adapter", "codex", "--alias", "principal", "--role", "lead",
        "--effort", "ultra", "--capabilities-file", str(snapshot),
    )
    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["effort"] == "xhigh"


@pytest.mark.parametrize("kind,status", [
    pytest.param("malformed", "capability_discovery_failed", id="malformed"),
    pytest.param("untrusted", "capability_snapshot_untrusted", id="forged-source"),
    pytest.param("empty", "capability_discovery_failed", id="empty-models"),
])
def test_unusable_codex_capability_snapshot_fails_closed(tmp_path, kind, status):
    if kind == "malformed":
        value = {}
    elif kind == "untrusted":
        value = {"schema_version": 1, "source": "forged",
                 "observed_at": (datetime.now(timezone.utc) - timedelta(days=3650)).isoformat(),
                 "models": {"gpt-5.6-sol": {"resolved_model": "gpt-5.6-sol", "supported_efforts": ["ultra"]}}}
    else:
        value = capability_snapshot({})
    snapshot = tmp_path / "caps.json"
    snapshot.write_text(json.dumps(value))
    result, route = resolve(
        "--adapter", "codex", "--alias", "principal", "--role", "lead", "--capabilities-file", str(snapshot),
    )
    assert result.returncode == 1
    assert route["status"] == status


@pytest.mark.parametrize(
    "models",
    [
        {"gpt-unrelated": {"resolved_model": "gpt-unrelated", "supported_efforts": []}},
        {"gpt-unrelated": {"resolved_model": "gpt-unrelated", "supported_efforts": [" "]}},
        {"gpt-key": {"resolved_model": "gpt-other", "supported_efforts": ["high"]}},
        {"gpt-key": {"resolved_model": "", "supported_efforts": ["high"]}},
        {
            "GPT-Duplicate": {
                "resolved_model": "GPT-Duplicate",
                "supported_efforts": ["high"],
            },
            "gpt-duplicate": {
                "resolved_model": "gpt-duplicate",
                "supported_efforts": ["max"],
            },
        },
    ],
)
def test_capability_snapshot_rejects_incomplete_or_inconsistent_models(tmp_path, models):
    snapshot = tmp_path / "caps.json"
    snapshot.write_text(json.dumps(capability_snapshot(models)))
    result, route = resolve(
        "--adapter", "codex", "--alias", "principal", "--role", "lead",
        "--capabilities-file", str(snapshot),
    )
    assert result.returncode == 1
    assert route["status"] == "capability_discovery_failed"


@pytest.mark.parametrize(
    "duplicate_fragment",
    [
        '"schema_version":1,"schema_version":1',
        '"models":{"gpt-5.6-sol":{"resolved_model":"gpt-5.6-sol",'
        '"supported_efforts":["high"]},"gpt-5.6-sol":{"resolved_model":"gpt-5.6-sol",'
        '"supported_efforts":["max"]}}',
        '"models":{"gpt-5.6-sol":{"resolved_model":"gpt-5.6-sol",'
        '"resolved_model":"gpt-5.6-sol","supported_efforts":["high"]}}',
        '"models":{"gpt-5.6-sol":{"resolved_model":"gpt-5.6-sol",'
        '"supported_efforts":["high"],"supported_efforts":["max"]}}',
    ],
)
def test_persisted_capability_snapshot_rejects_duplicate_json_members(
    tmp_path, duplicate_fragment
):
    observed_at = datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")
    default_models = (
        '"models":{"gpt-5.6-sol":{"resolved_model":"gpt-5.6-sol",'
        '"supported_efforts":["high"]}}'
    )
    fields = [
        duplicate_fragment,
        '"source":"codex debug models"',
        f'"observed_at":"{observed_at}"',
    ]
    if not duplicate_fragment.startswith('"models"'):
        fields.append(default_models)
    if not duplicate_fragment.startswith('"schema_version"'):
        fields.append('"schema_version":1')
    snapshot = tmp_path / "caps.json"
    snapshot.write_text("{" + ",".join(fields) + "}")
    result, route = resolve(
        "--adapter", "codex", "--alias", "principal", "--role", "lead",
        "--capabilities-file", str(snapshot),
    )
    assert result.returncode == 1
    assert route["status"] == "capability_discovery_failed"


def test_model_id_effort_uses_last_token_and_explicit_unresolved_fails():
    result, route = resolve(
        "--adapter", "cursor", "--model", "cursor-grok-4.5-low", "--alias", "principal",
        "--role", "reviewer", "--lead-family", "anthropic", "--require-distinct",
    )
    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["effort"] == "low"
    # Agy used to be a model-id adapter, which made every bare google alias
    # undispatchable: `agy --model gemini-3.8-flash` exits 1 asking for --effort.
    # It now carries effort in its own flag, so without a runtime snapshot the
    # effort resolves but is not capability-evidenced.
    result, route = resolve(
        "--adapter", "agy", "--model", "gemini-3.8-flash", "--alias", "principal",
        "--role", "reviewer", "--effort", "high", "--lead-family", "openai", "--require-distinct",
    )
    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["effort"] == "high"
    assert route["effort_capability_source"] == "provider-unverified"
    result, route = resolve(
        "--adapter", "cursor", "--model", "composer-2-extra-high", "--alias", "principal",
        "--role", "reviewer", "--lead-family", "anthropic", "--require-distinct",
    )
    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["effort"] == "xhigh"


@pytest.mark.parametrize("adapter,model_args", [
    pytest.param("codex", [], id="same-native-family"),
    pytest.param("cursor", ["--model", "gpt-5.6-sol"], id="precedes-adapter-family-check"),
])
def test_same_family_requirement_fails_closed(adapter, model_args):
    result, route = resolve(
        "--adapter", adapter, *model_args, "--alias", "principal", "--role", "reviewer",
        "--lead-family", "openai", "--require-distinct",
    )
    assert result.returncode == 1
    assert route["status"] == "same_family_forbidden"


@pytest.mark.parametrize("model,role,family,field,value", [
    pytest.param("cursor-grok-4.5-high", "reviewer", "xai", "identity_source", "model-pattern", id="broker-xai"),
    pytest.param("composer-2-high", "worker", "cursor-composer", "effort", "high", id="composer"),
])
def test_cursor_route_preserves_endpoint_and_model_identity(model, role, family, field, value):
    result, route = resolve(
        "--adapter", "cursor", "--model", model, "--alias", "principal",
        "--role", role, "--lead-family", "openai", "--require-distinct",
    )
    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["endpoint_provider"] == "cursor"
    assert route["model_family"] == family
    assert route[field] == value
    assert route["distinct_from_lead"] is True


def test_agy_passes_unregistered_models_to_the_broker():
    allowed, allowed_route = resolve(
        "--adapter", "agy", "--model", "gemini-3.8-flash", "--alias", "principal", "--role", "worker",
    )
    forbidden, forbidden_route = resolve(
        "--adapter", "agy", "--model", "grok-4", "--alias", "principal", "--role", "worker"
    )

    assert allowed.returncode == 0
    assert allowed_route["status"] == "ok"
    assert allowed_route["model_family"] == "google"
    assert allowed_route["adapter_enabled"] is True
    assert forbidden.returncode == 0
    assert forbidden_route["status"] == "ok"
    assert forbidden_route["resolved_model"] == "grok-4"
    assert forbidden_route["notes"]


@pytest.mark.parametrize("selector,alias,effort", [
    ("--task-class", "mechanical", "low"),
    ("--alias", "junior", "low"),
    ("--alias", "senior", "medium"),
    ("--alias", "principal", "high"),
])
def test_agy_routes_use_fresh_preferred_family_capabilities(tmp_path, selector, alias, effort):
    snapshot = write_agy_capability_snapshot(tmp_path)

    result, route = resolve(
        "--adapter", "agy", selector, alias, "--role", "worker",
        "--lead-family", "openai", "--require-distinct",
        "--capabilities-file", str(snapshot),
    )

    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["alias"] == ("junior" if selector == "--task-class" else alias)
    assert route["resolved_model"] == "gemini-3.8-flash"
    assert route["model_family"] == "google"
    assert route["identity_source"] == "runtime-capability+catalog"
    assert route["effort"] == effort
    if selector == "--alias":
        assert "task_class" not in route
    assert route["effort_capability_source"] == "runtime-model-catalog"


def test_agy_task_class_does_not_substitute_outside_exact_model_roster(tmp_path):
    snapshot = write_agy_capability_snapshot(
        tmp_path,
        models={
            "haiku": {
                "resolved_model": "haiku",
                "supported_efforts": ["low"],
            },
        },
    )

    result, route = resolve(
        "--adapter", "agy", "--task-class", "mechanical", "--role", "worker",
        "--lead-family", "openai", "--require-distinct",
        "--capabilities-file", str(snapshot),
    )

    assert result.returncode == 1
    assert route["status"] == "no_candidate_available"
    assert route["effort"] == ""


def test_opencode_route_resolves_explicit_free_model():
    result, route = resolve(
        "--adapter", "opencode", "--model", "opencode/union-alpha",
        "--alias", "junior", "--role", "worker",
    )

    assert result.returncode == 0, result.stdout
    assert route["status"] == "ok"
    assert route["adapter_enabled"] is True
    assert route["compatibility_adapter"] == "opencode-acp"
    assert route["model_family"] == "generic-open"
    assert route["family_source"] == "broker-default"
    assert route["resolved_model"] == "opencode/union-alpha"
    assert route["endpoint_provider"] == "opencode"


@pytest.mark.parametrize(("adapter", "model"), [
    ("opencode", "mimo"),
    ("agy", "gemini-3.8-flash"),
])
def test_ordinary_route_drops_only_an_implied_alias_for_explicit_model(adapter, model):
    command = [sys.executable, str(ROOT / "scripts" / "model_route.py"), "resolve",
               "--adapter", adapter, "--alias", "principal", "--model", model,
               "--role", "worker"]
    env = {**os.environ, "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT),
           "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT), "FABRIC_ALIAS_IMPLIED": "1"}
    implied = subprocess.run(command, capture_output=True, text=True, env=env)
    assert implied.returncode == 0, implied.stderr
    assert json.loads(implied.stdout)["alias"] == ""

    env["FABRIC_ALIAS_IMPLIED"] = "0"
    explicit = subprocess.run(command, capture_output=True, text=True, env=env)
    assert explicit.returncode == 0, explicit.stderr
    assert json.loads(explicit.stdout)["alias"] == "principal"


def test_model_route_namespace_without_alias_supplied_keeps_explicit_alias(monkeypatch, capsys):
    router = load_router()
    monkeypatch.setenv("AGENT_FABRIC_INSTANCE_ROOT", str(ROOT))
    monkeypatch.setenv("AGENT_FABRIC_PRODUCT_ROOT", str(ROOT))
    args = router._sibling("model_route_cli").parser(router).parse_args([
        "resolve", "--adapter", "codex", "--alias", "senior",
        "--model", "gpt-6-luna", "--role", "worker",
    ])
    args.task_class_effort = None
    args.model_override = {}
    assert router.resolve(args, router.load_catalog(None)) == 0
    assert json.loads(capsys.readouterr().out)["alias"] == "senior"


@pytest.mark.parametrize("model", [
    "opencode/deepseek-v4.1-flash",
    "opencode-go/deepseek-v4.1-flash",
    "openrouter/deepseek/deepseek-v4.1-flash",
])
def test_opencode_nested_deepseek_attributes_upstream_family(model):
    result, route = resolve(
        "--adapter", "opencode", "--model", model,
        "--alias", "junior", "--role", "worker",
    )

    assert result.returncode == 0, result.stdout
    assert route["model_family"] == "deepseek"
    assert route["family_source"] == "slug-inferred"


def test_openrouter_anthropic_endpoint_infers_vendor_from_slug(monkeypatch):
    monkeypatch.setenv("OPENROUTER_API_KEY", "endpoint-token-fixture")
    result, route = resolve(
        "--adapter", "claude", "--endpoint", "openrouter-anthropic",
        "--model", "moonshotai/kimi-k3", "--alias", "junior", "--role", "worker",
        "--catalog", PRODUCT_CATALOG,
    )

    assert result.returncode == 0, result.stdout
    assert route["status"] == "ok"
    assert route["endpoint_profile"] == "openrouter-anthropic"
    assert route["model_family"] == "moonshot"
    assert route["family_source"] == "slug-inferred"
    assert route["resolved_model"] == "moonshotai/kimi-k3"
    assert route["effort"] == ""


def test_openrouter_stealth_slug_stays_non_assurance(monkeypatch):
    monkeypatch.setenv("OPENROUTER_API_KEY", "endpoint-token-fixture")
    result, route = resolve(
        "--adapter", "claude", "--endpoint", "openrouter-anthropic",
        "--model", "stealth/union-alpha", "--alias", "junior", "--role", "worker",
        "--catalog", PRODUCT_CATALOG,
    )

    assert result.returncode == 0, result.stdout
    assert route["model_family"] == "generic-open"
    assert route["family_source"] == "slug-inferred"
    blocked, blocked_route = resolve(
        "--adapter", "claude", "--endpoint", "openrouter-anthropic",
        "--model", "stealth/union-alpha", "--alias", "junior", "--role", "worker",
        "--catalog", PRODUCT_CATALOG,
        "--require-distinct", "--lead-family", "anthropic",
    )
    assert blocked.returncode != 0
    assert blocked_route["status"] == "family_not_assurance_eligible"


def test_openrouter_openai_endpoint_infers_deepseek(monkeypatch):
    monkeypatch.setenv("OPENROUTER_API_KEY", "endpoint-token-fixture")
    result, route = resolve(
        "--adapter", "codex", "--endpoint", "openrouter-openai",
        "--model", "deepseek/deepseek-v4.1-flash", "--alias", "junior", "--role", "worker",
        "--catalog", PRODUCT_CATALOG,
    )

    assert result.returncode == 0, result.stdout
    assert route["endpoint_profile"] == "openrouter-openai"
    assert route["endpoint_wire_api"] == "responses"
    assert route["model_family"] == "deepseek"
    assert route["family_source"] == "slug-inferred"


def test_kiro_model_slugs_attribute_upstream_families():
    """Kiro's catalogue attribution follows the slug, independent of execution."""
    from scripts import model_route_catalog as catalog_mod

    # Load via the same loader model_route uses.
    mod = load_script(ROOT / "scripts" / "model_route_catalog.py", "model_route_catalog")

    for model, family in (
        ("claude-sonnet-4.5", "anthropic"),
        ("deepseek-3.2", "deepseek"),
        ("glm-5", "zhipu"),
        ("qwen3-coder-next", "alibaba"),
        ("minimax-m2.5", "minimax"),
        ("auto", None),  # no vendor pattern; not assurance-eligible
    ):
        inferred = mod.infer_family(model, CATALOG)
        assert inferred == family, (model, inferred)
        if family:
            assert mod.family_is_assurance_eligible(family, "slug-inferred")
        else:
            assert not mod.family_is_assurance_eligible("open-weight", "broker-default")


@pytest.mark.parametrize("lead,allowed", [
    pytest.param("anthropic", False, id="same-family"),
    pytest.param("google", True, id="distinct-family"),
])
def test_agy_claude_distinctness_follows_model_family(lead, allowed):
    result, route = resolve(
        "--adapter", "agy", "--model", "claude-opus-4-6-thinking", "--alias", "junior",
        "--role", "worker", "--require-distinct", "--lead-family", lead,
    )
    assert route["model_family"] == "anthropic"
    if allowed:
        assert result.returncode == 0, result.stdout
        assert route["distinct_from_lead"] is True
    else:
        assert result.returncode != 0
        assert route["status"] == "same_family_forbidden"


def test_require_distinct_rejects_ineligible_lead_family(monkeypatch):
    monkeypatch.setenv("OPENROUTER_API_KEY", "endpoint-token-fixture")
    result, route = resolve(
        "--adapter", "claude", "--endpoint", "openrouter-anthropic",
        "--model", "deepseek/deepseek-v4.1-flash", "--alias", "junior", "--role", "worker",
        "--catalog", PRODUCT_CATALOG,
        "--require-distinct", "--lead-family", "generic-open",
    )
    assert result.returncode != 0
    assert route["status"] == "family_not_assurance_eligible"
    assert route["model_family"] == "deepseek"


def test_optional_adapter_preference_policy_is_ordered_and_native_first_for_fallbacks():
    catalog = product_catalog()

    agy = catalog["adapters"]["agy"]["model_family_preferences"]
    cursor = catalog["adapters"]["cursor"]["model_family_preferences"]

    assert agy == {"preferred": ["google", "anthropic"], "fallback": {}}
    assert cursor == {
        "preferred": ["xai", "cursor-composer"],
        "fallback": {"anthropic": "claude", "openai": "codex", "google": "agy"},
    }


def test_google_flash_defaults_track_the_current_agy_catalogue():
    catalog = product_catalog()
    google = catalog["families"]["google"]

    assert google["aliases"]["principal"] == ["gemini-3.8-flash"]
    assert google["aliases"]["senior"] == ["gemini-3.8-flash"]
    assert google["aliases"]["junior"] == ["gemini-3.8-flash"]
    assert google["per_model_efforts"]["gemini-3.8-flash"] == [
        "low",
        "medium",
        "high",
    ]


def test_split_root_defaults_use_instance_routing_and_product_compatibility(tmp_path):
    product_root = tmp_path / "product"
    instance_root = tmp_path / "instance"
    shutil.copytree(ROOT / "scripts", product_root / "scripts")
    # scripts/lib/roots.py re-exports the one product-root resolver, which lives
    # in the shared skill library so the installed skills catalogue stays
    # self-sufficient (#754). A product tree carrying model_route.py carries it.
    shutil.copytree(ROOT / "skills" / "_shared", product_root / "skills" / "_shared")
    # Routing and reset share the locked cooldown reader with dispatch.
    shutil.copytree(ROOT / "skills" / "orchestrate", product_root / "skills" / "orchestrate")
    (product_root / "config").mkdir()
    shutil.copy2(
        ROOT / "config/adapter-compatibility.yaml",
        product_root / "config/adapter-compatibility.yaml",
    )
    shutil.copy2(
        ROOT / "config/model-routing.json",
        product_root / "config/model-routing.json",
    )
    (instance_root / "config").mkdir(parents=True)
    catalogue = product_catalog()
    future_date = (datetime.now(timezone.utc) + timedelta(days=3650)).date().isoformat()
    catalogue["catalog_date"] = future_date
    (instance_root / "config/model-routing.json").write_text(json.dumps(catalogue))
    env = {
        **os.environ,
        "AGENT_FABRIC_PRODUCT_ROOT": str(product_root),
        "AGENT_FABRIC_INSTANCE_ROOT": str(instance_root),
    }

    result = subprocess.run(
        [
            sys.executable,
            str(product_root / "scripts/model_route.py"),
            "resolve",
            "--adapter",
            "claude",
            "--alias",
            "junior",
            "--role",
            "worker",
        ],
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout)["catalog_date"] == future_date


def test_cursor_accepts_preferred_and_supported_fallback_families_without_model_name_locks():
    for model, family in (
        ("composer-2-high", "cursor-composer"),
        ("cursor-grok-4.5-high", "xai"),
        ("gemini-3.8-flash", "google"),
        ("claude-opus", "anthropic"),
        ("gpt-5.6-sol", "openai"),
        ("grokish-high", "xai"),
    ):
        allowed, allowed_route = resolve(
            "--adapter", "cursor", "--model", model, "--alias", "principal", "--role", "worker",
        )
        assert allowed.returncode == 0
        assert allowed_route["status"] == "ok"
        assert allowed_route["model_family"] == family

    wrong_family, wrong_family_route = resolve(
        "--adapter", "cursor", "--model", "qwen3-coder", "--alias", "principal", "--role", "worker"
    )
    assert wrong_family.returncode == 1
    assert wrong_family_route["status"] == "adapter_family_forbidden"


@pytest.mark.parametrize(
    ("selector_args", "status"),
    (
        (("--alias", "not-a-route"), "ok"),
        (("--alias", "junior", "--task-class", "mechanical"), "route_input_conflict"),
        (("--task-class", "not-a-task"), "unknown_task_class"),
    ),
)
def test_enabled_kiro_handles_route_selectors(selector_args, status):
    result, route = resolve(
        "--adapter", "kiro", *selector_args, "--role", "worker",
    )

    assert route["status"] == status
    assert result.returncode == (0 if status == "ok" else 2)


def test_disabled_pi_precedes_provider_family_checks():
    for model in ("gpt-5.6-sol", "claude-opus-4.5", "gemini-3.8-flash"):
        result, route = resolve(
            "--adapter", "pi", "--model", model, "--alias", "junior", "--role", "worker"
        )
        assert result.returncode == 1
        assert route["status"] == "adapter_disabled"


@pytest.mark.parametrize("disabled_reason", [None, "", [], 7], ids=["missing", "empty", "list", "number"])
def test_disabled_adapter_requires_a_nonempty_typed_reason(tmp_path, disabled_reason):
    compatibility = yaml.safe_load(
        (ROOT / "config/adapter-compatibility.yaml").read_text(encoding="utf-8")
    )
    entry = compatibility["adapters"]["pi-rpc"]
    if disabled_reason is None:
        entry.pop("disabled_reason")
    else:
        entry["disabled_reason"] = disabled_reason
    compatibility_path = tmp_path / "adapter-compatibility.yaml"
    compatibility_path.write_text(yaml.safe_dump(compatibility), encoding="utf-8")

    result, route = resolve(
        "--adapter", "pi", "--model", "deepseek-v3", "--alias", "junior",
        "--role", "worker", "--adapter-compatibility", str(compatibility_path),
    )

    assert result.returncode == 2
    assert route["status"] == "adapter_compatibility_invalid"


def test_disabled_adapter_precedes_malformed_runtime_capability_snapshot(tmp_path):
    snapshot = tmp_path / "capabilities.json"
    snapshot.write_text("not-json", encoding="utf-8")

    result, route = resolve(
        "--adapter", "pi", "--model", "deepseek-v3", "--alias", "junior",
        "--role", "worker", "--capabilities-file", str(snapshot),
    )

    assert result.returncode == 1
    assert route["status"] == "adapter_disabled"
    assert route["reason"].startswith("No trusted open-weight Pi provider/model")


@pytest.mark.parametrize(
    ("adapter", "model", "family", "effort"),
    [
        ("agy", "Gemini 3.8 Flash (High)", "google", "high"),
        ("cursor", "cursor-grok-4.5-high", "xai", "high"),
    ],
)
def test_optional_reviewers_route_directly_with_exact_identity(
    adapter, model, family, effort
):
    result, route = resolve(
        "--adapter", adapter, "--model", model, "--alias", "principal",
        "--role", "reviewer", "--effort", effort,
        "--lead-family", "openai", "--require-distinct",
    )

    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["resolved_model"] == model
    assert route["model_family"] == family
    assert route["effort"] == effort
    assert route["adapter_enabled"] is True


def test_primary_adapters_route_directly_with_compatibility_metadata(tmp_path):
    snapshot = write_codex_capability_snapshot(tmp_path)
    for adapter in ("claude", "codex"):
        capability_args = (
            ("--capabilities-file", str(snapshot)) if adapter == "codex" else ()
        )
        result, route = resolve(
            "--adapter", adapter, "--alias", "principal", "--role", "lead",
            *capability_args,
        )

        assert result.returncode == 0
        assert route["status"] == "ok"
        assert route["adapter_enabled"] is True


def test_dormant_compatibility_adapter_fails_closed():
    # Copilot has a dispatcher arm but no verified safe invocation, so it is
    # registered in the compatibility contract with enabled: false. The route
    # must refuse it rather than pass it through as an unknown adapter.
    arguments = (
        "--adapter", "copilot", "--model", "gemini-3.8-flash",
        "--alias", "principal", "--role", "worker",
    )

    result, route = resolve(*arguments)

    assert result.returncode == 1
    assert route["status"] == "adapter_disabled"
    assert route["adapter_enabled"] is False
    assert route["compatibility_adapter"] == "copilot"
    assert "fix:" in route["message"]


def write_agy_capability_snapshot(tmp_path, models=None):
    if models is None:
        # Measured against agy 1.1.10 and re-verified on 1.1.11: efforts are
        # per model.
        models = {
            "gemini-3.7-flash": {
                "resolved_model": "gemini-3.7-flash",
                "supported_efforts": ["low", "medium", "high"],
            },
            "gemini-3.8-flash": {
                "resolved_model": "gemini-3.8-flash",
                "supported_efforts": ["low", "medium", "high"],
            },
        }
    snapshot = tmp_path / "agy-caps.json"
    snapshot.write_text(json.dumps(capability_snapshot(models, source="agy models")))
    return snapshot


def test_agy_effort_is_validated_against_the_runtime_model_catalogue(tmp_path):
    snapshot = write_agy_capability_snapshot(tmp_path, {
        "gemini-3.8-flash": {"resolved_model": "gemini-3.8-flash", "supported_efforts": ["low", "high"]},
        "gemini-3.7-flash": {"resolved_model": "gemini-3.7-flash", "supported_efforts": ["low", "medium", "high"]},
    })

    def route_for(model, effort):
        return resolve(
            "--adapter", "agy", "--model", model, "--alias", "principal",
            "--role", "reviewer", "--effort", effort, "--lead-family", "openai",
            "--require-distinct", "--capabilities-file", str(snapshot),
        )

    result, route = route_for("gemini-3.8-flash", "high")
    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["effort"] == "high"
    assert route["effort_capability_source"] == "runtime-model-catalog"

    # A model whose listing omits medium must reject it:
    # the snapshot above gives gemini-3.8-flash only low and high. It must fail closed rather than
    # silently dispatching at some other effort.
    result, route = route_for("gemini-3.8-flash", "medium")
    assert result.returncode != 0
    assert route["status"] == "effort_unsupported"

    result, route = route_for("gemini-3.7-flash", "medium")
    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["effort"] == "medium"

    result, route = route_for("gemini-9-does-not-exist", "high")
    assert result.returncode != 0
    assert route["status"] == "capability_model_unavailable"


ENDPOINT_MODELS = {
    "zai-glm": "glm-4.7",
    "moonshot-kimi": "kimi-k2",
    "deepseek": "deepseek-3.2",
}
PRODUCT_CATALOG = str(ROOT / "config" / "model-routing.json")


def endpoint_route(monkeypatch, name, *args, token="endpoint-token-fixture"):
    profile = CATALOG["endpoints"][name]
    if token is None:
        monkeypatch.delenv(profile["token_env"], raising=False)
    else:
        monkeypatch.setenv(profile["token_env"], token)
    return resolve(
        "--adapter", "claude", "--endpoint", name,
        "--model", ENDPOINT_MODELS[name], "--alias", "principal", "--role", "worker",
        "--catalog", PRODUCT_CATALOG, *args,
    )


@pytest.mark.parametrize("name", sorted(ENDPOINT_MODELS))
def test_endpoint_profile_carries_base_url_and_token_variable_name(monkeypatch, name):
    profile = CATALOG["endpoints"][name]
    result, route = endpoint_route(monkeypatch, name)

    assert result.returncode == 0, result.stdout
    assert route["status"] == "ok"
    assert route["endpoint_profile"] == name
    assert route["endpoint_base_url"] == profile["base_url"]
    assert route["endpoint_base_url"].startswith("https://")
    assert route["endpoint_token_env"] == profile["token_env"]
    assert route["model_family"] == profile["model_family"]
    assert route["resolved_model"] == ENDPOINT_MODELS[name]
    # The credential is named, never carried: no route field may hold its value.
    assert "endpoint-token-fixture" not in result.stdout


@pytest.mark.parametrize("name", sorted(ENDPOINT_MODELS))
def test_endpoint_route_reports_no_effort(monkeypatch, name):
    """These endpoints expose no effort control, so none may be claimed."""
    result, route = endpoint_route(monkeypatch, name)

    assert result.returncode == 0
    assert route["effort"] == ""
    assert route["effort_capability_source"] == "adapter-no-effort-control"

    # An explicit effort is ignored with a warning, never sent and never claimed.
    explicit, explicit_route = endpoint_route(monkeypatch, name, "--effort", "high")
    assert explicit.returncode == 0, explicit.stdout
    assert explicit_route["status"] == "ok"
    assert explicit_route["effort"] == ""
    assert f"effort high ignored: {ENDPOINT_MODELS[name]} has no effort control" in explicit_route["notes"]


def test_endpoint_route_without_its_token_dispatches_nothing(monkeypatch):
    result, route = endpoint_route(monkeypatch, "zai-glm", token=None)

    assert result.returncode == 2
    assert route["status"] == "endpoint_token_missing"
    assert route.get("resolved_model") is None
    assert route.get("endpoint_base_url") is None


@pytest.mark.parametrize(
    ("name", "adapter", "status"),
    (
        ("no-such-endpoint", "claude", "unknown_endpoint"),
        ("zai-glm", "codex", "endpoint_adapter_unsupported"),
    ),
)
def test_unusable_endpoint_names_fail_closed(monkeypatch, name, adapter, status):
    monkeypatch.setenv(CATALOG["endpoints"]["zai-glm"]["token_env"], "endpoint-token-fixture")
    result, route = resolve(
        "--adapter", adapter, "--endpoint", name, "--model", "glm-4.7",
        "--alias", "principal", "--role", "worker", "--catalog", PRODUCT_CATALOG,
    )

    assert result.returncode == 2
    assert route["status"] == status
    assert route.get("resolved_model") is None


def test_claude_without_an_endpoint_stays_pinned_to_anthropic(monkeypatch):
    """The compatibility allowlist admits endpoint families; the catalogue still pins."""
    monkeypatch.setenv(CATALOG["endpoints"]["zai-glm"]["token_env"], "endpoint-token-fixture")
    result, route = resolve(
        "--adapter", "claude", "--model", "glm-4.7", "--alias", "principal",
        "--role", "worker", "--catalog", PRODUCT_CATALOG,
    )

    assert result.returncode == 1
    assert route["status"] == "adapter_family_mismatch"
    assert route.get("endpoint_base_url") is None


@pytest.mark.parametrize(
    "damage",
    ({"base_url": "http://api.z.ai/api/anthropic"}, {"token_env": ""}, {"adapters": "claude"}),
)
def test_malformed_endpoint_profile_is_never_routed_against(monkeypatch, tmp_path, damage):
    catalog = product_catalog()
    catalog["endpoints"]["zai-glm"].update(damage)
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setenv(CATALOG["endpoints"]["zai-glm"]["token_env"], "endpoint-token-fixture")

    result, route = resolve(
        "--adapter", "claude", "--endpoint", "zai-glm", "--model", "glm-4.7",
        "--alias", "principal", "--role", "worker", "--catalog", str(catalog_path),
    )

    assert result.returncode == 2
    assert route["status"] == "endpoint_config_invalid"
    assert route.get("resolved_model") is None


def test_implied_alias_beside_a_named_model_adds_no_note(tmp_path):
    env = {**os.environ, "FABRIC_ALIAS_IMPLIED": "1", "AGENT_FABRIC_STATE_ROOT": str(tmp_path)}
    implied = subprocess.run(
        [sys.executable, str(ROOT / "scripts/model_route.py"), "resolve", "--adapter", "codex",
         "--role", "worker", "--alias", "principal", "--model", "gpt-6-luna"],
        capture_output=True, text=True, env=env, check=False,
    )
    assert implied.returncode == 0 and json.loads(implied.stdout)["status"] == "ok"
    assert "alias and model both supplied" not in implied.stdout
    env.pop("FABRIC_ALIAS_IMPLIED")
    explicit = subprocess.run(
        [sys.executable, str(ROOT / "scripts/model_route.py"), "resolve", "--adapter", "codex",
         "--role", "worker", "--alias", "principal", "--model", "gpt-6-luna"],
        capture_output=True, text=True, env=env, check=False,
    )
    assert "alias and model both supplied" in explicit.stdout


@pytest.mark.parametrize(
    "adapter,model,absent",
    [
        ("cursor", "auto", "family unknown"),
        ("kiro", "auto", "not in the kiro registry"),
        ("opencode", "deepseek-v4.1-flash", "routed to opencode-go/"),
    ],
)
def test_informational_routes_add_no_warning_notes(tmp_path, adapter, model, absent):
    env = {**os.environ, "AGENT_FABRIC_STATE_ROOT": str(tmp_path), "FABRIC_ALIAS_IMPLIED": "1"}
    result = subprocess.run(
        [sys.executable, str(ROOT / "scripts/model_route.py"), "resolve", "--adapter", adapter,
         "--role", "worker", "--alias", "principal", "--model", model],
        capture_output=True, text=True, env=env, check=False,
    )
    route = json.loads(result.stdout)
    assert result.returncode == 0 and route["status"] == "ok", result.stdout
    assert route["resolved_model"]
    assert not any(absent in note for note in route["notes"] + route["warnings"])


def _live_cache(state, adapter, models, age=timedelta(0)):
    (state / "capabilities.json").write_text(json.dumps({adapter: {
        "observed_at": (datetime.now(timezone.utc) - age).isoformat(), "models": models}}))


def test_family_keys_resolve_to_the_highest_catalogue_version(tmp_path, monkeypatch):
    router = load_router()
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path))
    catalog = router.load_catalog()
    for adapter, requested, expected in (
            ("codex", "gpt-sol", "gpt-6.1-sol"), ("codex", "sol", "gpt-6.1-sol"),
            ("claude", "claude-sonnet", "claude-sonnet-5-5"), ("claude", "claude-opus", "claude-opus-5-5"),
            ("claude", "sonnet", "claude-sonnet-5-5"), ("agy", "sonnet", "claude-sonnet-4-6")):
        entry, notes = router.model_route_live._registered_match(adapter, requested, catalog)
        assert entry and entry["id"] == expected and not notes, (adapter, requested)
    # Explicit versions stay exact, and adapters that do not opt in are untouched.
    assert router.model_route_live._registered_match("claude", "claude-sonnet-5", catalog)[0]["id"] == "claude-sonnet-5-5"
    catalog["adapters"]["codex"]["latest_aliases"] = False
    assert router.model_route_live._registered_match("codex", "gpt-sol", catalog)[0] is None


def test_family_alias_picks_a_newer_live_model_with_a_note_and_never_upgrades_explicit_ids(tmp_path, monkeypatch):
    router = load_router()
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path))
    _live_cache(tmp_path, "codex", ["gpt-6.2-sol", "gpt-6.1-sol", "gpt-6.2-luna"])
    catalog = router.load_catalog()
    for requested in ("gpt-sol", "sol"):
        entry, notes = router.model_route_live._registered_match("codex", requested, catalog)
        assert entry["id"] == "gpt-6.2-sol"
        assert notes == ["gpt-6.2-sol is newer than the catalogue; run provenant refresh-routing "
                         "or add it to config/model-routing.json"]
    base = next(item for item in catalog["adapters"]["codex"]["models"] if item["id"] == "gpt-6.1-sol")
    assert entry["efforts"] == base["efforts"]
    assert router.model_route_live._registered_match("codex", "gpt-6.1-sol", catalog)[0]["id"] == "gpt-6.1-sol"
    _live_cache(tmp_path, "codex", ["gpt-6.2-sol"], age=timedelta(days=2))
    assert router.model_route_live._registered_match("codex", "sol", catalog)[0]["id"] == "gpt-6.1-sol"
    _live_cache(tmp_path, "codex", ["gpt-6.1-sol"])
    assert router.model_route_live._registered_match("codex", "sol", catalog)[1] == []


def test_resolve_applies_effort_and_cooldown_gates_to_a_live_family_pick(tmp_path, monkeypatch):
    _live_cache(tmp_path, "codex", ["gpt-6.2-sol"])
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path))
    def route():
        result, resolved = resolve("--adapter", "codex", "--model", "gpt-sol",
                                   "--effort", "high", "--role", "worker")
        assert result.returncode == 0, result.stderr
        return resolved
    result = route()
    assert result["resolved_model"] == "gpt-6.2-sol", result
    assert result["requested_model"] == "gpt-sol"
    assert any("newer than the catalogue" in note for note in result["notes"]), result
    until = (datetime.now(timezone.utc) + timedelta(hours=1)).isoformat()
    (tmp_path / "cooldowns.json").write_text(json.dumps({"cooldowns": {"codex/gpt-6.2-sol": {"cooling_until": until}}}))
    assert any("cooling" in warning for warning in route()["warnings"])


def _resolve_env(state):
    return {**os.environ, "HARNESS_PYTHON": sys.executable, "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT),
            "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT), "AGENT_FABRIC_STATE_ROOT": str(state)}


def test_newer_catalogue_warning_is_daily_and_always_kept_in_notes(tmp_path, monkeypatch, capsys):
    router = load_router()
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path))
    note = router.model_route_live._NEWER_NOTE.format("gpt-6.2-sol")

    def emit():
        router.emit({"status": "ok", "warnings": [note], "notes": []}, 0)
        return json.loads(capsys.readouterr().out)

    assert emit()["warnings"] == [note]
    second = emit()
    assert second["warnings"] == [] and second["notes"] == [note]
    # A previous day's marker does not suppress today's warning.
    state = tmp_path / "newer-model-warnings.json"
    state.write_text(state.read_text().replace(datetime.now(timezone.utc).date().isoformat(),
                                   (datetime.now(timezone.utc).date() - timedelta(days=1)).isoformat()))
    assert emit()["warnings"] == [note]
    # State failures must not fail routing or lose the receipt note.
    state.unlink()
    state.mkdir()
    assert emit()["notes"] == [note]


ROUTE_LIVE_MODELS = {
    "codex_new": ["gpt-6.2-sol"], "codex_registered": ["gpt-6.1-sol"],
    "claude_new": ["claude-sonnet-5-6"],
    "agy_efforts": ["gemini-3.9-flash-low", "gemini-3.9-flash-high", "gemini-3.8-flash-high"],
}


@pytest.mark.parametrize(("tier", "alias", "adapter", "effort", "live_set", "model", "error", "no_newer_note"), [
    ("principal", "gpt-sol", "codex", "", "codex_new", "gpt-6.2-sol", "", False),
    ("principal", "sol", "codex", "", "codex_new", "gpt-6.2-sol", "", False),
    ("principal", "GPT_SOL", "codex", "", "codex_new", "gpt-6.2-sol", "", False),
    ("principal", "s_o_l", "codex", "", "codex_new", "gpt-6.2-sol", "", False),
    ("principal", "gpt-6.2-sol", "codex", "", "codex_new", "gpt-6.2-sol", "", False),
    ("senior", "gpt-6.1-sol", "codex", "", "codex_registered", "gpt-6.1-sol", "", True),
    ("principal", "claude-sonnet", "claude", "", "claude_new", "claude-sonnet-5-6", "", False),
    ("principal", "sonnet", "claude", "", "claude_new", "claude-sonnet-5-6", "", False),
    ("", "flash", "agy", "high", "agy_efforts", "gemini-3.9-flash-high", "", False),
    ("", "gemini-flash", "agy", "high", "agy_efforts", "gemini-3.9-flash-high", "", False),
    ("", "gemini-3.9-flash-high", "agy", "high", "agy_efforts", "gemini-3.9-flash-high", "", False),
    ("", "gemini-3.9-flash", "agy", "high", "agy_efforts", "gemini-3.9-flash-high", "", False),
    ("principal", "qwen3-coder", "cursor", "", "", "", "adapter_family_forbidden", False),
])
def test_resolver_model_route_table(tmp_path, tier, alias, adapter, effort, live_set, model, error, no_newer_note):
    if adapter in {"codex", "claude"}:
        _live_cache(tmp_path, adapter, ROUTE_LIVE_MODELS[live_set])
    if adapter == "agy":
        _agy_live(tmp_path, ROUTE_LIVE_MODELS[live_set])
    arguments = ["--adapter", adapter, "--model", alias, "--role", "worker"]
    if tier:
        arguments.extend(("--alias", tier))
    if effort:
        arguments.extend(("--effort", effort))
    environment = _resolve_env(tmp_path) | {"FABRIC_ALIAS_IMPLIED": "1"}
    with patch.dict(os.environ, environment):
        result, route = resolve(*arguments)
    if error:
        assert result.returncode == 1 and route["status"] == error, route
    else:
        assert result.returncode == 0 and route["resolved_model"] == model, route
        if no_newer_note:
            assert not any("newer than the catalogue" in note
                           for note in route.get("notes", []) + route.get("warnings", []))


def test_spelling_variants_of_a_family_name_resolve_like_gpt_sol(tmp_path, monkeypatch):
    router = load_router()
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path))
    catalog = router.load_catalog()
    for spelling in ("GPT_SOL", "s_o_l", "Gpt Sol", "SOL"):
        assert router.model_route_live._registered_match("codex", spelling, catalog)[0]["id"] == "gpt-6.1-sol", spelling


def _agy_live(state, models):
    (state / "capabilities.json").write_text(json.dumps({"agy": {
        "observed_at": datetime.now(timezone.utc).isoformat(), "models": models}}))


@pytest.mark.parametrize(("tier", "alias"), [("junior", "junior"), ("Senior", "senior"), ("principal", "principal")])
def test_route_tier_names_work_as_adapter_aliases(tmp_path, tier, alias):
    capability = write_codex_capability_snapshot(tmp_path)
    common = ("--adapter", "codex", "--role", "worker", "--capabilities-file", str(capability))
    _, named = resolve("--alias", tier, *common)
    _, original = resolve("--alias", alias, *common)
    assert named["resolved_model"] == original["resolved_model"]
    assert not any("model won" in note for note in named.get("notes", [])), "a tier name is not a model name"


@pytest.mark.parametrize("flag", ["--help", "-h"])
def test_dash_prefixed_positional_argument_falls_through_to_argparse(flag, monkeypatch, capsys):
    router = load_router()
    monkeypatch.setattr(router, "route_pick", lambda *_args, **_kwargs: pytest.fail("routed as a tier"))
    with pytest.raises(SystemExit) as raised:
        router.main([flag])
    assert raised.value.code == 0
    assert "usage:" in capsys.readouterr().out


@pytest.mark.parametrize("name,fix", [pytest.param("junior", False, id="tier"), pytest.param("legwork", True, id="synonym")])
def test_unavailable_positional_route_names(tmp_path, name, fix):
    instance = tmp_path / "instance"
    config = instance / "config"
    config.mkdir(parents=True)
    (config / "model-routing.json").write_text(json.dumps({"routes": {"junior": [
        {**entry, "weight": 0} for entry in CATALOG["routes"]["junior"]
    ]}}))
    env = {**os.environ, "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT),
           "AGENT_FABRIC_INSTANCE_ROOT": str(instance),
           "AGENT_FABRIC_STATE_ROOT": str(tmp_path / "state")}
    run = subprocess.run([sys.executable, str(ROOT / "scripts" / "model_route.py"), name],
                         capture_output=True, text=True, env=env)
    result = json.loads(run.stdout)
    assert run.returncode == 2
    assert result["results"][0]["status"] == "rejected"
    assert result["results"][0]["error"] == "route_unavailable"
    if fix:
        assert "junior" in result["results"][0]["fix"]


@pytest.mark.parametrize("tier,route", [("junior", "junior"), ("legwork", "junior")])
@pytest.mark.parametrize("state", ["ahead_of_pace", "deficit", "credit_only"])
def test_positional_route_tier_shorthand_picks_from_the_pool(tier, route, state, monkeypatch, capsys):
    router = load_router()
    calls = []

    def route_pick(requests):
        calls.extend(requests)
        return {"status": "ok", "results": [{"status": "ok", "route": route,
                "picks": [{"adapter": "codex", "model": "gpt-6-luna", "reason": f"{route} top"}],
                "warnings": []}]}

    monkeypatch.setattr(router, "route_pick", route_pick)
    monkeypatch.setattr(router.model_route_live, "_quota_states", lambda: {"codex": {
        "account": "work", "state": state, "summary": "window over pace", "observed_at": "now"
    }})
    assert router.main([tier]) == 0
    result = json.loads(capsys.readouterr().out)
    assert calls == [{"route": tier}]
    assert result["results"][0]["picks"][0]["model"] == "gpt-6-luna"
    assert result["results"][0]["picks"][0]["quota"]["summary"] == "window over pace"
    assert result["results"][0]["warnings"] == ["codex quota: window over pace"]


LIVE_OPENCODE = ["opencode/big-pickle", "opencode-go/qwen3.8-flash", "opencode-go/kimi-k3",
                 "opencode-go/kimi-k2.7-code", "opencode-go/mimo-v2.6-flash", "opencode/mimo-v2.6-flash-free"]


@pytest.mark.parametrize("name,model", [("qwen3.8-flash", "opencode-go/qwen3.8-flash"),
                                        ("QWEN3.8-FLASH", "opencode-go/qwen3.8-flash"),
                                        ("big-pickle", "opencode/big-pickle"),
                                        ("qwen", "opencode-go/qwen3.8-flash")])
def test_opencode_bare_name_resolves_against_live_listing(name, model):
    resolved, notes, refusal = load_router().model_route_live._resolve_opencode_name(name, LIVE_OPENCODE)
    assert (resolved, refusal) == (model, None) and notes


@pytest.mark.parametrize("name,status", [("kimi", "model_ambiguous"), ("mimo", "model_ambiguous"),
                                         ("zzzzzz", "model_not_found")])
def test_opencode_bare_name_refusals_are_typed_with_a_fix(name, status):
    resolved, _, refusal = load_router().model_route_live._resolve_opencode_name(name, LIVE_OPENCODE)
    assert resolved is None and refusal["status"] == status and "fix:" in refusal["message"]
    if status == "model_ambiguous":
        assert "opencode-go/kimi-k3" in refusal["message"] or "mimo-v2.6-flash" in refusal["message"]


def test_opencode_bare_name_passes_through_without_a_listing_or_with_a_provider_path():
    router = load_router()
    assert router.model_route_live._resolve_opencode_name("kimi", []) == (None, [], None)
    assert router.model_route_live._resolve_opencode_name("opencode/anything", LIVE_OPENCODE) == (None, [], None)


def test_opencode_resolve_end_to_end_with_a_fake_cli(tmp_path):
    cli = tmp_path / "bin" / "opencode"
    cli.parent.mkdir()
    cli.write_text("#!/bin/sh\n"
                   "if [ \"$1\" = --version ]; then echo 'opencode v2.0.30'; exit 0; fi\n"
                   "if [ \"$1\" = models ]; then printf '%s\\n' " + " ".join(LIVE_OPENCODE) + "; fi\n")
    cli.chmod(0o755)
    env = {**os.environ, "PATH": f"{cli.parent}:/usr/bin:/bin", "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT),
           "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT), "AGENT_FABRIC_STATE_ROOT": str(tmp_path / "state")}

    def run(model):
        out = subprocess.run([sys.executable, str(ROOT / "scripts" / "model_route.py"), "resolve", "--adapter",
                              "opencode", "--role", "worker", "--model", model], text=True, capture_output=True,
                             env=env)
        return out.returncode, json.loads(out.stdout)

    code, route = run("qwen3.8-flash")
    assert code == 0 and route["resolved_model"] == "opencode-go/qwen3.8-flash"
    code, route = run("kimi")
    assert code == 2 and route["status"] == "model_ambiguous"
    code, route = run("zzzzzz")
    assert code == 2 and route["status"] == "model_not_found"
    code, route = run("opencode-go/kimi-k3")
    assert code == 0 and any("live opencode listing" in note for note in route["notes"])
    code, route = run("opencode/deepseek-v4.1-flash-free#high")
    assert code == 0 and route["resolved_model"] == "opencode/deepseek-v4.1-flash-free"
    assert route["effort_applied"] == "high"
    code, route = run("deepseek")
    assert code == 0 and route["resolved_model"] == "opencode/deepseek-v4.1-flash-free"


def test_opencode_variant_matching_explicit_effort_still_sends_the_variant(tmp_path):
    env = {**os.environ, "PATH": "/usr/bin:/bin", "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT),
           "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT), "AGENT_FABRIC_STATE_ROOT": str(tmp_path / "state")}
    out = subprocess.run([sys.executable, str(ROOT / "scripts" / "model_route.py"), "resolve", "--adapter",
                          "opencode", "--role", "worker", "--model", "opencode/deepseek-v4.1-flash-free#high",
                          "--effort", "high"], text=True, capture_output=True, env=env)
    route = json.loads(out.stdout)
    assert out.returncode == 0, route
    assert route["effort_applied"] == "high"
    assert route["requested_model"] == "opencode/deepseek-v4.1-flash-free"
    assert route["requested_input"] == "opencode/deepseek-v4.1-flash-free#high"


def test_opencode_requested_input_keeps_the_callers_bare_token(tmp_path):
    cli = tmp_path / "bin" / "opencode"
    cli.parent.mkdir()
    cli.write_text("#!/bin/sh\n"
                   "if [ \"$1\" = --version ]; then echo 'opencode v2.0.30'; exit 0; fi\n"
                   "if [ \"$1\" = models ]; then printf '%s\\n' " + " ".join(LIVE_OPENCODE) + "; fi\n")
    cli.chmod(0o755)
    env = {**os.environ, "PATH": f"{cli.parent}:/usr/bin:/bin", "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT),
           "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT), "AGENT_FABRIC_STATE_ROOT": str(tmp_path / "state")}
    out = subprocess.run([sys.executable, str(ROOT / "scripts" / "model_route.py"), "resolve", "--adapter",
                          "opencode", "--role", "worker", "--model", "QWEN3.8-FLASH"],
                         text=True, capture_output=True, env=env)
    route = json.loads(out.stdout)
    assert route["resolved_model"] == "opencode-go/qwen3.8-flash"
    assert route["requested_input"] == "QWEN3.8-FLASH"


@pytest.mark.parametrize("trusted", [False, True])
def test_failed_opencode_refresh_keeps_the_last_good_listing(tmp_path, monkeypatch, trusted):
    router = load_router()
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path))
    cli = tmp_path / "opencode"
    cli.write_text("#!/bin/sh\nif [ \"$1\" = --version ]; then echo 2.0.30; exit; fi\n"
                   "if [ \"$1\" = --help ]; then exit; fi\nprintf 'a/one\\na/two\\n'\n")
    cli.chmod(0o755)
    record, code = router.model_route_probe.probe_capabilities("opencode", str(cli), listing_only=True)
    assert code == 0 and record["models"] == ["a/one", "a/two"]
    if not trusted:
        path = tmp_path / "capabilities.json"
        cache = json.loads(path.read_text())
        cache["opencode"].pop("listing_format")
        path.write_text(json.dumps(cache))
    cli.write_text("#!/bin/sh\nif [ \"$1\" = --version ]; then echo 2.0.30; exit; fi\nexit 1\n")
    failed, code = router.model_route_probe.probe_capabilities("opencode", str(cli), listing_only=True, refresh=True)
    if not trusted:
        assert code == 1 and failed["status"] == "probe_unavailable" and not failed["models"]
        return
    assert code == 0 and failed["models"] == ["a/one", "a/two"] and failed["refresh_failed"]
    cached, code = router.model_route_probe.probe_capabilities("opencode", str(cli), listing_only=True)
    assert code == 0 and cached["cache_hit"] and cached["models"] == ["a/one", "a/two"]


@pytest.mark.parametrize("raw", ["Error.\n", "Loading...\nDone.\n", "auto - Auto\nError: sign in again\n", "auto - Auto\n\ngpt-5 - G\nError: sign in again\n"])
def test_cursor_plain_text_probe_rejects_prose_without_a_listing(raw):
    assert load_router().model_route_probe._listed_models(raw, "cursor") == []


def test_opencode_resolve_caches_a_timed_out_refresh(tmp_path, monkeypatch):
    router = load_router()
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path))
    monkeypatch.setenv("PATH", f"{tmp_path}:/usr/bin:/bin")
    cli = tmp_path / "opencode"
    cli.write_text("#!/bin/sh\nif [ \"$1\" = --version ]; then echo 2.0.30; exit; fi\n"
                   "if [ \"$1\" = --help ]; then exit; fi\nprintf 'a/one\\na/two\\n'\n")
    cli.chmod(0o755)
    record, code = router.model_route_probe.probe_capabilities("opencode", str(cli), listing_only=True)
    assert code == 0 and record["models"] == ["a/one", "a/two"]

    original_run = router.subprocess.run
    refresh_calls = []

    def run(command, *args, **kwargs):
        if command[-1:] == ["models"]:
            refresh_calls.append(command)
            raise router.subprocess.TimeoutExpired(command, kwargs.get("timeout"))
        return original_run(command, *args, **kwargs)

    monkeypatch.setattr(router.subprocess, "run", run)
    for _ in range(2):
        with redirect_stdout(io.StringIO()):
            router.main(["resolve", "--adapter", "opencode", "--role", "worker",
                         "--model", "missing-model"])
    assert len(refresh_calls) == 1


def test_failed_opencode_refresh_does_not_replace_a_newer_listing(tmp_path, monkeypatch):
    router = load_router()
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path))
    cli = tmp_path / "opencode"
    cli.write_text("#!/bin/sh\nif [ \"$1\" = --version ]; then echo 2.0.30; exit; fi\n"
                   "if [ \"$1\" = --help ]; then exit; fi\nprintf 'a/one\\n'\n")
    cli.chmod(0o755)
    original, code = router.model_route_probe.probe_capabilities("opencode", str(cli), listing_only=True)
    assert code == 0 and original["models"] == ["a/one"]
    cache_path = tmp_path / "capabilities.json"
    original_run = router.subprocess.run

    def run(command, *args, **kwargs):
        if command[-1:] == ["models"]:
            cache = json.loads(cache_path.read_text())
            cache["opencode"].update(
                models=["a/new"], observed_at=(datetime.now(timezone.utc) + timedelta(seconds=1)).isoformat(),
            )
            cache_path.write_text(json.dumps(cache))
            raise router.subprocess.TimeoutExpired(command, kwargs.get("timeout"))
        return original_run(command, *args, **kwargs)

    monkeypatch.setattr(router.subprocess, "run", run)
    refreshed, code = router.model_route_probe.probe_capabilities("opencode", str(cli), listing_only=True, refresh=True)
    assert code == 0 and refreshed["models"] == ["a/new"]
    assert json.loads(cache_path.read_text())["opencode"]["models"] == ["a/new"]


def test_opencode_refresh_is_skipped_when_the_shared_budget_is_nearly_spent(monkeypatch):
    router = load_router()
    calls = []
    monkeypatch.setattr(router.model_route_probe, "probe_capabilities", lambda *a, **k: calls.append(k) or ({"models": ["x/y"]}, 0))
    assert router.model_route_live._opencode_live_models(refresh=True, deadline=router.model_route_live.time.monotonic() + 1) == ([], False)
    assert not calls
    assert router.model_route_live._opencode_live_models(refresh=True, deadline=router.model_route_live.time.monotonic() + 10)[0] == ["x/y"]


def test_effort_shift_never_climbs_to_retired_max_or_ultra():
    router = load_router()
    assert router.SHIFT_LADDER == ("low", "medium", "high", "xhigh")
    every = [{"efforts": ["low", "medium", "high", "xhigh", "max", "ultra"]}]
    profile = type("Profile", (), {"value": staticmethod(lambda *args, **kwargs: 1)})
    router._harness_profile = lambda: profile
    assert router.shifted_task_class_effort("review", "high", "low", every) == ("xhigh", 1)
    assert router.shifted_task_class_effort("review", "xhigh", "low", every) == ("xhigh", 0)


def _write_catalog(tmp_path, mutate):
    catalog = product_catalog()
    mutate(catalog)
    path = tmp_path / "custom-catalog.json"
    path.write_text(json.dumps(catalog))
    return path


@pytest.mark.parametrize("mutate,message,fix", [
    (lambda c: c["adapters"]["claude"]["models"][0].update(default_effort="max"),
     "effort 'max' was retired", "xhigh"),
    (lambda c: c["adapters"]["claude"]["models"][0].update(efforts=["max"]),
     "effort 'max' was retired", "xhigh"),
    (lambda c: c["families"]["anthropic"].update(default_effort="max"),
     "effort 'max' was retired", "xhigh"),
    (lambda c: c["routes"]["senior"][0].update(effort=["high", "max"]),
     "effort 'max' was retired", "xhigh"),
    (lambda c: c["task_class_routes"]["legwork"].update(effort="max"),
     "effort 'max' was retired", "xhigh"),
    (lambda c: c["families"]["openai"]["aliases"].update(flagship=c["families"]["openai"]["aliases"]["principal"]),
     "alias 'flagship' was renamed to 'principal'", ""),
    (lambda c: c["task_class_routes"]["legwork"].update(alias="workhorse"),
     "alias 'workhorse' was renamed to 'senior'", ""),
    (lambda c: c["routes"].update(strong=c["routes"]["senior"]),
     "route 'strong' was renamed to 'senior'", ""),
])
def test_catalogue_rejects_retired_efforts_and_routing_names(tmp_path, mutate, message, fix):
    catalog = _write_catalog(tmp_path, mutate)
    result = subprocess.run([str(SCRIPT), "resolve", "--catalog", str(catalog), "--adapter", "codex",
                             "--alias", "senior", "--role", "worker"], text=True, capture_output=True, check=False)
    assert result.returncode == 2
    assert message in result.stderr
    if fix:
        assert fix in result.stderr


def test_provider_quota_cooldown_skips_models_outside_the_plan_it_covers():
    router = load_router()
    catalog = router.catalogue_snapshot()
    until = (datetime.now(timezone.utc) + timedelta(days=3)).isoformat()
    cooldowns = {"opencode/*": {"status": "quota_exhausted", "cooling_until": until, "source": "provider quota",
                                "covers": ["opencode-go/"]}}
    assert router.model_route_live._cooling("opencode", "opencode-go/glm-5.3-flash", cooldowns, catalog) == until
    assert not router.model_route_live._cooling("opencode", "opencode/glm-5.3-flash-free", cooldowns, catalog)
    own = {**cooldowns, "opencode/opencode/glm-5.3-flash-free": {"cooling_until": until}}
    assert router.model_route_live._cooling("opencode", "opencode/glm-5.3-flash-free", own, catalog) == until
