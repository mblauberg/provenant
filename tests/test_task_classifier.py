from pathlib import Path

from tests.support.modules import load_script

ROOT = Path(__file__).resolve().parents[1]
classifier = load_script(ROOT / "scripts/task_classifier.py", "task_classifier_test", register=True)


def config():
    return {"task_class_routes": {
        "implementation": {"description": "Build changes"},
        "review": {"description": "Review changes"}},
        "task_classification": {"chain": ["explicit", "rules", "default"],
            "rules": [{"when": {"mode": "read_only", "prompt_any": ["audit"], "min_files": 2}, "class": "review"}],
            "size_tiers": [{"name": "small", "min_files": 0, "min_lines": 0},
                           {"name": "large", "min_files": 21, "min_lines": 1001}],
            "default": "implementation"}}


def test_explicit_and_configured_rules_stages():
    explicit = classifier.classify(config(), {"prompt": "audit", "mode": "read_only"}, explicit="implementation")
    assert explicit["classified_by"] == "explicit" and explicit["class"] == "implementation"
    rules = classifier.classify(config(), {"prompt": "audit this", "mode": "read_only", "size": {"files": 2}})
    assert rules["classified_by"] == "rules" and rules["class"] == "review"


def test_default_warns_without_matching_rules(capsys):
    result = classifier.classify(config(), {"prompt": "new work"})
    assert result["class"] == "implementation" and result["classified_by"] == "default"
    assert "default class implementation" in capsys.readouterr().err


def test_override_environment_and_size():
    cfg = config()
    assert classifier.classify(cfg, explicit="review", override="default")["classified_by"] == "explicit"
    assert classifier.classify(cfg, override=None, environ={"PROVENANT_TASK_CLASSIFY": "explicit,default"},
                                explicit="review")["class"] == "review"
    assert classifier.classify(cfg, {"size": {"files": 30}})["size_tier"] == "large"
