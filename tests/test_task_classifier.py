import json
from pathlib import Path

from tests.support.modules import load_script

ROOT = Path(__file__).resolve().parents[1]
classifier = load_script(ROOT / "scripts/task_classifier.py", "task_classifier_test", register=True)
hook = load_script(ROOT / "scripts/routing_context_hook.py", "routing_context_hook_test", register=True)


def config():
    return {"routes": {"junior": [{"model": "codex/gpt-6-luna"}]}, "task_class_routes": {
        "implementation": {"alias": "senior", "effort": "high", "role": "worker", "description": "Build changes"},
        "review": {"alias": "principal", "effort": "high", "role": "reviewer", "description": "Review changes",
                   "models": {"claude": ["review"]}}},
        "task_classification": {"chain": ["explicit", "rules", "model", "default"], "shadow": ["rules", "model"],
            "rules": [{"when": {"mode": "read_only", "prompt_any": ["audit"], "min_files": 2}, "class": "review"}],
            "model": {"alias": "utility", "effort": "low", "timeout_s": 20, "min_confidence": 0.6},
            "role_defaults": {"scout": "junior"},
            "size_tiers": [{"name": "small", "min_files": 0, "min_lines": 0, "alias": "junior"},
                           {"name": "large", "min_files": 21, "min_lines": 1001, "alias": "principal"}],
            "default": "implementation"}}


def test_explicit_and_configured_rules_stages():
    no_model = lambda *a, **k: (_ for _ in ()).throw(TimeoutError("shadow disabled"))
    explicit = classifier.classify(config(), {"prompt": "audit", "mode": "read_only"},
                                   explicit="implementation", utility=no_model)
    assert explicit["classified_by"] == "explicit" and explicit["class"] == "implementation"
    rules = classifier.classify(config(), {"prompt": "audit this", "mode": "read_only", "size": {"files": 2}},
                                utility=no_model)
    assert rules["classified_by"] == "rules" and rules["class"] == "review"


def test_model_stage_falls_through_on_error_and_default_warns(capsys):
    cfg = config()
    cfg["task_classification"]["chain"] = ["model", "default"]
    result = classifier.classify(cfg, {"prompt": "new work"}, utility=lambda *a, **k: (_ for _ in ()).throw(TimeoutError("late")))
    assert result["class"] == "implementation" and result["classified_by"] == "default"
    assert "default class implementation" in capsys.readouterr().err


def test_shadow_records_other_stage_and_override_and_environment(tmp_path, monkeypatch):
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path))
    cfg = config()
    cfg["task_classification"]["chain"] = ["explicit", "default"]
    result = classifier.classify(cfg, {"prompt": "audit", "mode": "read_only", "size": {"files": 2}},
                                 explicit="implementation",
                                 utility=lambda *a, **k: (_ for _ in ()).throw(TimeoutError("shadow disabled")))
    assert result["shadow_answers"]["rules"]["class"] == "review"
    assert classifier.classify(cfg, explicit="review", override="default")["classified_by"] == "explicit"
    assert classifier.classify(cfg, override=None, environ={"PROVENANT_TASK_CLASSIFY": "explicit,default"},
                                explicit="review")["class"] == "review"
    sized = classifier.classify(cfg, {"size": {"files": 30}})
    assert sized["size_tier"] == "large"


def test_model_stage_uses_utility_and_prompt_hash_cache(tmp_path, monkeypatch):
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path))
    cfg = config()
    cfg["task_classification"]["chain"] = ["model", "default"]
    calls = []
    def fake(verb, source, **kwargs):
        calls.append((verb, json.loads(source), kwargs["schema"], kwargs["timeout_seconds"], kwargs["retry"]))
        return {"class": "review", "confidence": 0.92, "reason": "task is a review"}
    first = classifier.classify(cfg, {"prompt": "audit this"}, utility=fake)
    second = classifier.classify(cfg, {"prompt": "audit this"}, utility=fake)
    assert first["class"] == second["class"] == "review"
    assert first["classified_by"] == "model" and len(calls) == 1
    assert calls[0][0] == "extract" and calls[0][2]["properties"]["class"]["enum"] == ["implementation", "review"]
    assert calls[0][3:] == (20, False)
    payload = json.dumps(calls[0][1])
    assert "Build changes" in payload and "principal" not in payload


def test_hook_injects_context_and_warns_without_blocking():
    cfg = config()
    assert "class and size determine tier; role is a label" in hook.routing_context(cfg).casefold()
    warning = hook.soft_warning(cfg, {"tool_name": "Agent", "tool_input": {"role": "reviewer", "model": "other/model"}})
    assert warning and "outside the configured roster" in warning
    assert hook.soft_warning(cfg, {"tool_name": "Agent", "tool_input": {"role": "reviewer", "model": "claude/review"}}) is None
    assert "claude/review" in hook.routing_context(cfg)
    assert hook.soft_warning(cfg, {"tool_name": "mcp__fabric__fabric_dispatch",
                                  "tool_input": {"role": "scout", "model": "gpt-6-luna"}}) is None


def test_hook_accepts_legacy_role_against_canonical_roster():
    cfg = config()
    cfg['task_classification']['role_defaults'] = {'junior': 'junior'}
    assert hook.soft_warning(cfg, {'tool_name': 'Agent',
                                  'tool_input': {'role': 'scout', 'model': 'codex/gpt-6-luna'}}) is None
    assert hook.soft_warning(cfg, {'tool_name': 'Agent',
                                  'tool_input': {'role': 'scout', 'model': 'outside/model'}})
