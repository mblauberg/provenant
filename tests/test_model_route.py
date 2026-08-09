import json
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timedelta, timezone
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import sys

import pytest


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "model-route"
CATALOG = json.loads((ROOT / "config" / "model-routing.json").read_text())
CRUCIAL_RISK_OVERRIDE = CATALOG["families"]["anthropic"]["risk_tier_overrides"]["crucial"]
RISK_OVERRIDE_MODEL = CRUCIAL_RISK_OVERRIDE["models"][0]
NON_OCCUPANT_MODELS = tuple(
    model
    for models in CATALOG["families"]["anthropic"]["aliases"].values()
    for model in models
    if RISK_OVERRIDE_MODEL.casefold() not in model.casefold()
)


def resolve(*args):
    arguments = [str(SCRIPT), "resolve", *args]
    result = subprocess.run(
        arguments,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    return result, json.loads(result.stdout) if result.stdout else None


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
            "gemini-3.1-pro",
            "--alias",
            "flagship",
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
            "gpt-5.6-sol": {
                "resolved_model": "gpt-5.6-sol",
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
        }
    value = capability_snapshot(models)
    if observed_at is not None:
        value["observed_at"] = observed_at
    snapshot = tmp_path / "codex-capabilities.json"
    snapshot.write_text(json.dumps(value))
    return snapshot


def write_account_default_catalog(tmp_path):
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
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
    path = ROOT / "scripts" / "model_route.py"
    spec = importlib.util.spec_from_file_location("model_route_under_test", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def route_candidate(
    candidate_id,
    *,
    adapter,
    family,
    model,
    status="ok",
    alias="flagship",
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
        ],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    return result, json.loads(result.stdout) if result.stdout else None, state_path


def test_critical_review_preference_cannot_lower_tier_or_effort(tmp_path):
    candidates = [
        route_candidate(
            "anthropic-flagship",
            adapter="claude",
            family="anthropic",
            model="opus",
        ),
        route_candidate(
            "openai-cheaper",
            adapter="codex",
            family="openai",
            model="gpt-5.6-terra",
            status="task_class_effort_below_floor",
            alias="workhorse",
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
    assert receipt["chosen_candidate_id"] == "anthropic-flagship"
    assert receipt["chosen_route"]["alias"] == "flagship"
    assert receipt["chosen_route"]["effort"] == "high"
    assert {
        "kind": "family_affinity",
        "scope": "task_class",
        "value": "openai",
        "reason": "no_admissible_candidate",
    } in receipt["preference"]["not_honoured"]


def test_disabled_adapter_and_outside_model_preferences_are_reported(tmp_path):
    candidates = [
        route_candidate(
            "anthropic-flagship",
            adapter="claude",
            family="anthropic",
            model="opus",
        ),
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
    assert receipt["chosen_candidate_id"] == "anthropic-flagship"
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
        route_candidate(
            "anthropic-flagship",
            adapter="claude",
            family="anthropic",
            model="opus",
        ),
        route_candidate(
            "openai-flagship",
            adapter="codex",
            family="openai",
            model="gpt-5.6-sol",
        ),
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
        route_candidate(
            "anthropic-flagship",
            adapter="claude",
            family="anthropic",
            model="opus",
        ),
        route_candidate(
            "openai-flagship",
            adapter="codex",
            family="openai",
            model="gpt-5.6-sol",
        ),
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
        route_candidate(
            "anthropic-flagship",
            adapter="claude",
            family="anthropic",
            model="opus",
        ),
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
        route_candidate(
            "anthropic-flagship",
            adapter="claude",
            family="anthropic",
            model="opus",
        ),
        route_candidate(
            "openai-flagship",
            adapter="codex",
            family="openai",
            model="gpt-5.6-sol",
        ),
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
            route_candidate(
                "anthropic-flagship",
                adapter="claude",
                family="anthropic",
                model="opus",
            ),
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


def test_corrupt_preferences_file_has_distinct_receipt_status(tmp_path):
    result, receipt, _ = select_route(
        tmp_path,
        [
            route_candidate(
                "anthropic-flagship",
                adapter="claude",
                family="anthropic",
                model="opus",
            ),
        ],
        '{"schema_version":1,,,',
    )

    assert result.returncode == 0
    assert receipt["preference"]["file_status"] == "corrupt"


def test_unsupported_spreading_policy_is_recorded_as_ignored(tmp_path):
    result, receipt, _ = select_route(
        tmp_path,
        [
            route_candidate(
                "anthropic-flagship",
                adapter="claude",
                family="anthropic",
                model="opus",
            ),
        ],
        {
            "schema_version": 1,
            "spreading": {"policy": "weighted-by-cost"},
        },
    )

    assert result.returncode == 0
    assert receipt["spreading"]["policy"] == "fair-round-robin"
    assert {
        "kind": "spreading_policy",
        "scope": "spreading",
        "value": "weighted-by-cost",
        "reason": "unsupported_policy",
    } in receipt["preference"]["ignored"]


def test_deprioritise_preference_changes_tie_break_and_is_recorded(tmp_path):
    candidates = [
        route_candidate(
            "anthropic-flagship",
            adapter="claude",
            family="anthropic",
            model="opus",
        ),
        route_candidate(
            "openai-flagship",
            adapter="codex",
            family="openai",
            model="gpt-5.6-sol",
        ),
    ]
    result, receipt, _ = select_route(tmp_path, candidates, {
        "schema_version": 1,
        "deprioritise": {"families": ["anthropic"]},
    })

    assert result.returncode == 0
    assert receipt["chosen_candidate_id"] == "openai-flagship"
    assert {
        "kind": "family_deprioritise",
        "scope": "deprioritise",
        "value": "anthropic",
    } in receipt["preference"]["applied"]


def test_not_honoured_reason_distinguishes_higher_preference(tmp_path):
    candidates = [
        route_candidate(
            "anthropic-flagship",
            adapter="claude",
            family="anthropic",
            model="opus",
        ),
        route_candidate(
            "openai-flagship",
            adapter="codex",
            family="openai",
            model="gpt-5.6-sol",
        ),
    ]
    result, receipt, _ = select_route(tmp_path, candidates, {
        "schema_version": 1,
        "task_classes": {
            "critical-review": {"family_affinity": ["openai", "anthropic"]},
        },
    })

    assert result.returncode == 0
    assert receipt["chosen_candidate_id"] == "openai-flagship"
    assert {
        "kind": "family_affinity",
        "scope": "task_class",
        "value": "anthropic",
        "reason": "lost_to_higher_preference",
    } in receipt["preference"]["not_honoured"]


def test_not_honoured_reason_distinguishes_fair_spreading(tmp_path):
    openai = route_candidate(
        "openai-flagship",
        adapter="codex",
        family="openai",
        model="gpt-5.6-sol",
    )
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
            route_candidate(
                "anthropic-flagship",
                adapter="claude",
                family="anthropic",
                model="opus",
            ),
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
    assert receipt["chosen_candidate_id"] == "anthropic-flagship"
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
            route_candidate(
                "anthropic-flagship",
                adapter="claude",
                family="anthropic",
                model="opus",
            ),
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
        route_candidate(
            "anthropic-flagship",
            adapter="claude",
            family="anthropic",
            model="opus",
        ),
        route_candidate(
            "forged-cheap",
            adapter="codex",
            family="openai",
            model="gpt-5.6-terra",
            alias="workhorse",
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
    assert receipt["chosen_candidate_id"] == "anthropic-flagship"
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
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    override = catalog["families"]["anthropic"]["risk_tier_overrides"]["crucial"]
    override["models"] = ["claude-retargeted-model"]
    override["roles"] = ["reviewer-one", "reviewer-two"]
    override["alias"] = "workhorse"
    override["default_effort"] = "low"
    override["maximum_effort"] = "high"
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "claude", "--alias", override["alias"],
        "--role", override["roles"][0], "--risk-tier", "crucial",
        "--model", override["models"][0], "--effort", "high",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 0
    assert route["status"] == "ok"
    assert route["resolved_model"] == "claude-retargeted-model"
    assert route["route_source"] == "risk-tier-override"
    assert route["policy_override"] == "crucial-claude-retargeted-model-reviewer-one-reviewer-two"
    assert route["effort"] == "high"


def test_retargeted_override_occupant_requires_explicit_risk_tier(
    tmp_path, monkeypatch, capsys
):
    router = load_router()
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    override = catalog["families"]["anthropic"]["risk_tier_overrides"]["crucial"]
    override["models"] = ["claude-newcomer"]
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "claude", "--alias", "flagship",
        "--role", "worker", "--model", "claude-newcomer",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 1
    assert route["status"] == "risk_tier_override_required"


def test_retargeting_override_occupant_ungates_previous_occupant(
    tmp_path, monkeypatch, capsys
):
    router = load_router()
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    overrides = catalog["families"]["anthropic"]["risk_tier_overrides"]
    for override in overrides.values():
        override["models"] = ["claude-newcomer"]
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "claude", "--alias", "flagship",
        "--role", "worker", "--model", "fable",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 0
    assert route["status"] == "ok"
    assert route["resolved_model"] == "fable"


def test_retargeting_one_tier_keeps_occupant_gated_by_another_tier(
    tmp_path, monkeypatch, capsys
):
    router = load_router()
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    catalog["families"]["anthropic"]["risk_tier_overrides"]["crucial"]["models"] = [
        "claude-newcomer"
    ]
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "claude", "--alias", "flagship",
        "--role", "worker", "--model", "fable",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 1
    assert route["status"] == "risk_tier_override_required"


def test_foreign_family_risk_override_does_not_gate_explicit_model(
    tmp_path, monkeypatch, capsys
):
    router = load_router()
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    catalog["families"]["openai"]["risk_tier_overrides"] = {
        "crucial": {
            "roles": ["synthesis", "adjudication"],
            "alias": "flagship",
            "models": ["opus"],
            "default_effort": "medium",
            "maximum_effort": "medium",
        },
    }
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "claude", "--alias", "flagship",
        "--role", "worker", "--model", "claude-opus-4-6",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 0
    assert route["status"] == "ok"
    assert route["resolved_model"] == "claude-opus-4-6"


@pytest.mark.parametrize("occupant", ("flagship", "opus"))
def test_risk_tier_override_occupant_cannot_be_reached_by_alias(
    tmp_path, monkeypatch, capsys, occupant
):
    router = load_router()
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    catalog["families"]["anthropic"]["risk_tier_overrides"]["crucial"]["models"] = [
        occupant
    ]
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "claude", "--alias", "flagship",
        "--role", "worker",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "risk_tier_config_invalid"


def test_risk_tier_override_occupant_cannot_match_versioned_alias_candidate(
    tmp_path, monkeypatch, capsys
):
    router = load_router()
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    family = catalog["families"]["anthropic"]
    family["aliases"]["flagship"] = ["claude-opus-4-6"]
    family["role_overrides"] = {}
    family["risk_tier_overrides"]["crucial"]["models"] = ["opus"]
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "claude", "--alias", "flagship",
        "--role", "worker",
    ])

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
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    catalog["families"]["anthropic"]["risk_tier_overrides"]["crucial"]["models"] = [
        occupant
    ]
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "claude", "--alias", "flagship",
        "--role", "worker", "--model", explicit_model,
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "risk_tier_config_invalid"


def test_risk_tier_override_occupant_cannot_be_reached_by_role_alias_override(
    tmp_path, monkeypatch, capsys
):
    router = load_router()
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    family = catalog["families"]["anthropic"]
    family["role_overrides"]["worker"] = {"flagship": ["claude-role-model"]}
    family["risk_tier_overrides"]["crucial"]["models"] = ["claude-role-model"]
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "claude", "--alias", "flagship",
        "--role", "worker",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "risk_tier_config_invalid"


def test_foreign_family_override_collision_does_not_reject_adapter_route(
    tmp_path, monkeypatch, capsys
):
    router = load_router()
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    catalog["families"]["anthropic"]["risk_tier_overrides"]["crucial"]["models"] = [
        "opus"
    ]
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    snapshot = write_codex_capability_snapshot(tmp_path)
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "codex", "--alias", "workhorse",
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
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    catalog["families"]["anthropic"]["risk_tier_overrides"]["crucial"]["models"] = [
        "opus"
    ]
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "claude", "--role", "worker",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "route_input_missing"


def test_claude_flagship_and_critical_review_default_to_opus():
    lead, lead_route = resolve("--adapter", "claude", "--alias", "flagship", "--role", "lead")
    review, review_route = resolve(
        "--adapter", "claude", "--alias", "flagship", "--role", "critical-review"
    )
    assert lead.returncode == review.returncode == 0
    assert lead_route["resolved_model"] == review_route["resolved_model"] == "opus"
    assert lead_route["model_family"] == review_route["model_family"] == "anthropic"


def test_claude_other_primary_uses_opus():
    result, route = resolve(
        "--adapter", "claude", "--alias", "flagship", "--role", "other-primary"
    )
    assert result.returncode == 0
    assert route["resolved_model"] == "opus"


def test_opus_unavailable_has_no_implicit_risk_override_fallback():
    result, route = resolve(
        "--adapter",
        "claude",
        "--alias",
        "flagship",
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
        "--adapter", "claude", "--alias", "flagship", "--role", "worker",
        "--model", model, "--effort", "high",
    )
    assert result.returncode == 1
    assert route["status"] == "risk_tier_override_required"


def test_explicit_override_occupant_stays_reserved_for_broker_adapters():
    result, route = resolve(
        "--adapter", "cursor", "--alias", "flagship", "--role", "worker",
        "--model", RISK_OVERRIDE_MODEL, "--effort", "high",
    )
    assert result.returncode == 1
    assert route["status"] == "risk_tier_override_required"


@pytest.mark.parametrize(
    "route",
    (
        ("--alias", "flagship", "--role", "worker"),
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
    assert receipt["status"] == "risk_tier_override_required"


def test_capability_resolved_override_occupant_records_explicit_risk_tier(
    tmp_path, monkeypatch, capsys
):
    router = load_router()
    catalog_path = Path(ROOT / "config" / "model-routing.json")
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)
    resolved_model = f"claude-opus-4-6-{RISK_OVERRIDE_MODEL}"
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
        "--role", CRUCIAL_RISK_OVERRIDE["roles"][0], "--risk-tier", "crucial",
        "--capabilities-file", str(snapshot_path),
    ])

    receipt = json.loads(capsys.readouterr().out)
    assert router.CATALOG_PATH == catalog_path
    assert result == 0
    assert receipt["status"] == "ok"
    assert receipt["resolved_model"] == resolved_model
    assert receipt["risk_tier"] == "crucial"
    assert receipt["route_source"] == "risk-tier-override"


def test_unconfigured_inferred_family_cannot_bypass_override_gate():
    result, route = resolve(
        "--adapter", "opencode", "--alias", "flagship", "--role", "worker",
        "--model", "opencode/fable",
    )

    assert result.returncode == 1
    assert route["status"] == "risk_tier_override_required"


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
        "--risk-tier", risk_tier, "--effort", effort, "--available-model", model,
    )
    assert result.returncode == 0
    assert route["resolved_model"] == model
    assert route["risk_tier"] == risk_tier
    assert route["route_source"] == "risk-tier-override"
    assert route["policy_override"] == (
        f"{risk_tier}-{model}-{'-'.join(override['roles'])}"
    )
    assert route["effort"] == effort


@pytest.mark.parametrize(
    ("arguments", "expected_status"),
    (
        (
            ("--alias", "flagship", "--role", "synthesis", "--risk-tier", "substantial"),
            "risk_tier_override_unavailable",
        ),
        (
            ("--alias", "flagship", "--role", "worker", "--risk-tier", "crucial"),
            "risk_tier_role_mismatch",
        ),
        (
            ("--alias", "workhorse", "--role", "synthesis", "--risk-tier", "crucial"),
            "risk_tier_alias_mismatch",
        ),
        (
            (
                "--alias", "flagship", "--role", "synthesis", "--risk-tier", "crucial",
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
        "--adapter", "claude", "--alias", "flagship", "--role", "synthesis",
        "--risk-tier", "crucial", "--model", model, "--effort", "medium",
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
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    catalog["families"]["anthropic"]["risk_tier_overrides"]["crucial"][field] = value
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "claude", "--alias", CRUCIAL_RISK_OVERRIDE["alias"],
        "--role", CRUCIAL_RISK_OVERRIDE["roles"][0], "--risk-tier", "crucial",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "risk_tier_config_invalid"


def test_non_dict_risk_tier_override_is_rejected_as_malformed(tmp_path, monkeypatch, capsys):
    # A tier that is present but not a mapping is a malformed catalogue, not an
    # absent tier: it must be reported as such rather than as `unavailable`.
    router = load_router()
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    catalog["families"]["anthropic"]["risk_tier_overrides"]["crucial"] = []
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "claude", "--alias", "flagship",
        "--role", "synthesis", "--risk-tier", "crucial",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "risk_tier_config_invalid"


def test_absent_risk_tier_is_still_reported_unavailable(tmp_path, monkeypatch, capsys):
    # The `unavailable` status stays reachable for the case it actually names.
    router = load_router()
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    catalog["families"]["anthropic"]["risk_tier_overrides"].pop("crucial")
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "claude", "--alias", "flagship",
        "--role", "synthesis", "--risk-tier", "crucial",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "risk_tier_override_unavailable"


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
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    catalog["families"]["anthropic"]["risk_tier_overrides"]["crucial"]["models"] = malformed_models
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "claude", "--alias", "flagship",
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
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    catalog["families"]["anthropic"]["risk_tier_overrides"]["crucial"]["models"] = "fable"
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    snapshot = write_codex_capability_snapshot(tmp_path)
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "codex", "--alias", "workhorse",
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
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    catalog["families"] = families
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", adapter, "--model", "fable",
        "--alias", "flagship", "--role", "worker",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "risk_tier_config_invalid"
    assert route.get("resolved_model") is None


def test_adapter_pinned_to_an_undefined_family_requires_an_explicit_model(capsys):
    """OpenCode has no alias table to route against, so an alias route rejects.

    ``generic-open`` is deliberately absent from the families table: OpenCode
    routes on explicit account-catalogue models. Resolving an alias against it
    dereferenced a family that was never there and crashed with no JSON at all,
    on the production catalogue, with no catalogue edit needed to reach it.
    """
    router = load_router()

    result = router.main([
        "resolve", "--adapter", "opencode", "--alias", "flagship",
        "--role", "worker",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "model_required_for_broker"
    assert route.get("resolved_model") is None


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
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    mutate(catalog["families"])
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "codex", "--alias", "flagship",
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
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    catalog["families"]["openai"] = {}
    catalog["families"]["anthropic"]["risk_tier_overrides"]["crucial"]["models"] = "fable"
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    # Every family the catalogue defines, named from the catalogue rather than
    # repeated here: the invariant is that the scan widens to all of them, which
    # a hardcoded pair silently converts into a count that breaks whenever a
    # family is added.
    assert list(router.override_scan_families("gpt-5.6-sol", catalog)) == list(catalog["families"])

    result = router.main([
        "resolve", "--adapter", "cursor", "--alias", "flagship", "--role", "worker",
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
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    catalog["adapters"]["account-default-fixture"] = {
        "endpoint_provider": "fixture",
        "fixed_model_family": "openai",
        "effort_transport": "flag",
        "model_selection": "account-default",
    }
    catalog["families"]["openai"]["aliases"] = aliases
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "account-default-fixture", "--alias", "flagship",
        "--role", "worker", "--model", "gpt-5.6-sol",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 1
    assert route["status"] == "adapter_account_default_only"


@pytest.mark.parametrize(
    "field, value",
    [
        ("roles", "ab"),
        ("alias", ["flagship"]),
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
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    catalog["families"]["anthropic"]["risk_tier_overrides"]["crucial"][field] = value
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "claude", "--alias", "flagship", "--role", "a",
        "--risk-tier", "crucial", "--model", "fable", "--available-model", "fable",
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
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    for tier in catalog["families"]["anthropic"]["risk_tier_overrides"].values():
        tier["models"] = "fable"
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "claude", "--alias", "flagship", "--role", "worker",
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
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    for tier in catalog["families"]["anthropic"]["risk_tier_overrides"].values():
        tier["models"] = "fable"
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", adapter, "--model", "fable",
        "--alias", "flagship", "--role", "worker",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "risk_tier_config_invalid"
    assert route.get("resolved_model") is None


def test_account_default_aliases_resolve_to_account_default_dispatch(tmp_path):
    expected = {
        "flagship": "gpt-5.6-sol",
        "workhorse": "gpt-5.6-luna",
        "scout": "gpt-5.6-luna",
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
        "--adapter", "account-default-fixture", "--alias", "flagship",
        "--role", "worker", "--available-model", "gpt-5.6-luna",
        "--catalog", str(catalog),
    )
    assert result.returncode == 0
    assert route["resolved_model"] == ""
    assert route["catalog_model"] == "gpt-5.6-sol"
    assert route["model_selection"] == "account-default"


@pytest.mark.parametrize(
    ("catalog_account_default", "requires_explicit_model"),
    ((True, True), (False, False)),
)
def test_catalog_and_compatibility_mismatch_fails_closed(
    tmp_path, monkeypatch, capsys, catalog_account_default, requires_explicit_model
):
    router = load_router()
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    adapter = catalog["adapters"]["codex"]
    if catalog_account_default:
        adapter["model_selection"] = "account-default"
    else:
        adapter.pop("model_selection", None)
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    compatibility_path = write_codex_compatibility(
        tmp_path, requires_explicit_model=requires_explicit_model
    )
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "codex", "--alias", "scout", "--role", "worker",
        "--adapter-compatibility", str(compatibility_path),
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "account_default_conflicts_with_compatibility"


def test_codex_aliases_supply_proportionate_default_effort(tmp_path):
    expected = {"flagship": "high", "workhorse": "medium", "scout": "low"}
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
        ("mechanical", "scout", "low", "gpt-5.6-luna"),
        ("legwork", "workhorse", "medium", "gpt-5.6-luna"),
        ("critical-review", "flagship", "max", "gpt-5.6-sol"),
        ("orchestration", "flagship", "ultra", "gpt-5.6-sol"),
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


def test_claude_task_class_rejects_caller_authored_capability_claim(tmp_path):
    snapshot = tmp_path / "claude-caps.json"
    snapshot.write_text(json.dumps(capability_snapshot({
        "opus": {"resolved_model": "opus", "supported_efforts": ["high"]},
    }, source="claude subscription canary")))
    result, route = resolve(
        "--adapter", "claude", "--task-class", "critical-review",
        "--role", "critical-review", "--capabilities-file", str(snapshot),
    )
    assert result.returncode == 1
    assert route["status"] == "capability_snapshot_untrusted"


def test_claude_task_class_admits_probed_effort_without_laundering_provenance(tmp_path):
    snapshot = tmp_path / "claude-caps.json"
    value = capability_snapshot({
        "opus": {
            "resolved_model": "claude-opus-4-8",
            "requested_effort": "high",
            "effort_verified": False,
        },
    }, source="claude subscription canary")
    value["provenance"] = {
        "kind": "subscription_runtime_canary",
        "auth_method": "claude.ai",
        "subscription_type": "pro",
    }
    snapshot.write_text(json.dumps(value))

    result, route = resolve(
        "--adapter", "claude", "--task-class", "critical-review",
        "--role", "critical-review", "--capabilities-file", str(snapshot),
    )

    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["resolved_model"] == "claude-opus-4-8"
    assert route["requested_effort"] == route["effort"] == "high"
    # The model identity is genuinely runtime-verified; only the effort is not.
    assert route["identity_source"] == "runtime-capability+catalog"
    # The weaker provenance must survive into the receipt. If this ever reads
    # "runtime-model-catalog" the router is claiming effort evidence it lacks.
    assert route["effort_capability_source"] == "provider-unverified"


def test_claude_capability_alias_must_anchor_the_resolved_model_identity(tmp_path):
    snapshot = tmp_path / "claude-caps.json"
    value = capability_snapshot({
        "opus": {
            "resolved_model": "claude-haiku-4-5-opus",
            "requested_effort": "high",
            "effort_verified": False,
        },
    }, source="claude subscription canary")
    value["provenance"] = {
        "kind": "subscription_runtime_canary",
        "auth_method": "claude.ai",
        "subscription_type": "pro",
    }
    snapshot.write_text(json.dumps(value))

    result, route = resolve(
        "--adapter", "claude", "--task-class", "critical-review",
        "--role", "critical-review", "--capabilities-file", str(snapshot),
    )

    assert route["status"] == "capability_discovery_failed"
    assert result.returncode == 1
    assert route["effort"] == ""


def test_capability_snapshot_rejects_alias_key_collision_with_a_resolved_model_key(
    tmp_path
):
    snapshot = tmp_path / "claude-caps.json"
    value = capability_snapshot({
        "opus": {
            "resolved_model": "claude-opus-4-8-fable",
            "requested_effort": "high",
            "effort_verified": False,
        },
        "fable": {
            "resolved_model": "claude-opus-4-8-fable",
            "requested_effort": "low",
            "effort_verified": False,
        },
    }, source="claude subscription canary")
    value["provenance"] = {
        "kind": "subscription_runtime_canary",
        "auth_method": "claude.ai",
        "subscription_type": "pro",
    }
    snapshot.write_text(json.dumps(value))

    result, route = resolve(
        "--adapter", "claude", "--task-class", "critical-review",
        "--role", "critical-review", "--capabilities-file", str(snapshot),
    )

    assert route["status"] == "capability_discovery_failed"
    assert result.returncode == 1
    assert route["effort"] == ""


def test_claude_task_class_snapshot_probed_at_another_effort_fails_closed(tmp_path):
    """The task-class effort is `high`; a snapshot probed at `low` is not evidence for it."""
    snapshot = tmp_path / "claude-caps.json"
    value = capability_snapshot({
        "opus": {
            "resolved_model": "claude-opus-4-8",
            "requested_effort": "low",
            "effort_verified": False,
        },
    }, source="claude subscription canary")
    value["provenance"] = {
        "kind": "subscription_runtime_canary",
        "auth_method": "claude.ai",
        "subscription_type": "pro",
    }
    snapshot.write_text(json.dumps(value))

    result, route = resolve(
        "--adapter", "claude", "--task-class", "critical-review",
        "--role", "critical-review", "--capabilities-file", str(snapshot),
    )

    assert result.returncode == 1
    assert route["status"] == "effort_capability_unverified"
    assert route["effort"] == ""


def test_claude_alias_route_uses_verified_model_without_claiming_effort_support(tmp_path):
    snapshot = tmp_path / "claude-caps.json"
    value = capability_snapshot({
        "opus": {
            "resolved_model": "claude-opus-4-8",
            "requested_effort": "high",
            "effort_verified": False,
        },
    }, source="claude subscription canary")
    value["provenance"] = {
        "kind": "subscription_runtime_canary",
        "auth_method": "claude.ai",
        "subscription_type": "pro",
    }
    snapshot.write_text(json.dumps(value))

    result, route = resolve(
        "--adapter", "claude", "--alias", "flagship",
        "--role", "critical-review", "--effort", "high",
        "--capabilities-file", str(snapshot),
    )

    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["resolved_model"] == "claude-opus-4-8"
    assert route["requested_effort"] == route["effort"] == "high"
    assert route["effort_capability_source"] == "provider-unverified"


def test_claude_unverified_effort_snapshot_does_not_cover_another_effort(tmp_path):
    snapshot = tmp_path / "claude-caps.json"
    value = capability_snapshot({
        "opus": {
            "resolved_model": "claude-opus-4-8",
            "requested_effort": "high",
            "effort_verified": False,
        },
    }, source="claude subscription canary")
    value["provenance"] = {
        "kind": "subscription_runtime_canary",
        "auth_method": "claude.ai",
        "subscription_type": "pro",
    }
    snapshot.write_text(json.dumps(value))

    result, route = resolve(
        "--adapter", "claude", "--alias", "flagship",
        "--role", "critical-review", "--effort", "medium",
        "--capabilities-file", str(snapshot),
    )

    assert result.returncode == 1
    assert route["status"] == "effort_capability_unverified"
    assert route["effort"] == ""
    assert route["effort_capability_source"] == "provider-unverified"


def test_task_class_without_trusted_capability_evidence_fails_closed():
    result, route = resolve(
        "--adapter", "claude", "--task-class", "critical-review",
        "--role", "critical-review", "--available-model", "opus",
    )
    assert result.returncode == 1
    assert route["status"] == "task_class_capability_unverified"


def test_critical_review_task_class_rejects_worker_role_before_model_resolution():
    result, route = resolve(
        "--adapter", "claude", "--task-class", "critical-review", "--role", "worker"
    )
    assert result.returncode == 2
    assert route["status"] == "task_class_role_mismatch"
    assert route["role"] == "worker"
    assert route["task_class"] == "critical-review"


def test_legacy_alias_route_does_not_claim_task_class_binding(tmp_path):
    snapshot = write_codex_capability_snapshot(tmp_path)
    result, route = resolve(
        "--adapter", "codex", "--alias", "scout", "--role", "worker",
        "--capabilities-file", str(snapshot),
    )
    assert result.returncode == 0
    assert "task_class" not in route
    assert "route_source" not in route


@pytest.mark.parametrize(
    "arguments",
    (
        ("--alias", "scout", "--task-class", "mechanical"),
        ("--task-class", "mechanical", "--effort", "medium"),
        ("--task-class", "unknown"),
    ),
)
def test_task_class_rejects_ambiguous_or_unknown_routing_inputs(arguments):
    result, route = resolve("--adapter", "codex", *arguments, "--role", "worker")
    assert result.returncode == 2
    assert route["schema_version"] == 1
    assert route["adapter"] == "codex"
    assert route["role"] == "worker"
    assert route["status"] in {
        "route_input_conflict", "task_class_effort_conflict", "unknown_task_class",
    }


def test_task_class_rejects_explicit_model_override():
    result, route = resolve(
        "--adapter", "claude", "--task-class", "critical-review",
        "--role", "critical-review", "--model", "haiku",
    )
    assert result.returncode == 2
    assert route["status"] == "task_class_model_conflict"
    assert route["alias"] == "flagship"


def test_task_class_rejects_effective_effort_below_policy_floor(tmp_path):
    snapshot = tmp_path / "caps.json"
    snapshot.write_text(json.dumps(capability_snapshot({
        "gpt-5.6-sol": {
            "resolved_model": "gpt-5.6-sol",
            "supported_efforts": ["low"],
        },
    })))

    result, route = resolve(
        "--adapter", "codex", "--task-class", "critical-review",
        "--role", "critical-review", "--capabilities-file", str(snapshot),
    )

    assert result.returncode == 1
    assert route["status"] == "task_class_effort_below_floor"
    assert route["requested_effort"] == "max"
    assert route["effort"] == ""


def test_invalid_task_class_effort_vocabulary_fails_closed(tmp_path, monkeypatch, capsys):
    router = load_router()
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    catalog["task_class_routes"]["critical-review"]["effort"] = "hgh"
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "claude", "--task-class", "critical-review",
        "--role", "critical-review",
    ])

    route = json.loads(capsys.readouterr().out)
    assert route["status"] == "task_class_config_invalid"
    assert result == 2
    assert route["effort"] == ""


def test_task_class_role_policy_cannot_be_reconfigured_to_worker(tmp_path, monkeypatch, capsys):
    router = load_router()
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    catalog["task_class_routes"]["critical-review"]["role"] = "worker"
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "claude", "--task-class", "critical-review",
        "--role", "worker",
    ])

    route = json.loads(capsys.readouterr().out)
    assert route["status"] == "task_class_config_invalid"
    assert result == 2


@pytest.mark.parametrize(
    ("field", "value"),
    (("alias", "scout"), ("effort", "low")),
)
def test_critical_review_policy_rejects_valid_vocabulary_downgrade(
    tmp_path, monkeypatch, capsys, field, value
):
    router = load_router()
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    catalog["task_class_routes"]["critical-review"][field] = value
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "claude", "--task-class", "critical-review",
        "--role", "critical-review",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "task_class_config_invalid"


def test_task_class_effort_must_equal_the_capability_probe_effort(
    tmp_path, monkeypatch, capsys
):
    router = load_router()
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    catalog["task_class_routes"]["critical-review"]["effort"] = "max"
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "claude", "--task-class", "critical-review",
        "--role", "critical-review",
    ])

    route = json.loads(capsys.readouterr().out)
    assert route["status"] == "task_class_config_invalid"
    assert result == 2
    assert "configuration error" in route["message"]
    assert "must equal probe policy minimum_effort 'high'" in route["message"]


def test_role_default_cannot_lower_task_class_effort(tmp_path, monkeypatch, capsys):
    router = load_router()
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    catalog["families"]["openai"]["role_effort_defaults"]["orchestrator"]["flagship"] = "low"
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    snapshot = tmp_path / "caps.json"
    snapshot.write_text(json.dumps(capability_snapshot({
        "gpt-5.6-sol": {"resolved_model": "gpt-5.6-sol", "supported_efforts": ["high"]},
    })))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "codex", "--task-class", "orchestration",
        "--role", "orchestrator", "--capabilities-file", str(snapshot),
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 0
    assert route["requested_effort"] == "high"
    assert route["effort_source"] == "task-class"


def test_openai_route_without_runtime_snapshot_fails_closed(
    monkeypatch, capsys
):
    router = load_router()
    monkeypatch.setattr(router, "CATALOG_PATH", ROOT / "config" / "model-routing.json")

    result = router.main([
        "resolve", "--adapter", "codex", "--alias", "flagship", "--role", "lead",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 1
    assert route["status"] == "capability_discovery_failed"
    assert route["requested_effort"] == "ultra"
    assert route["effort"] == ""


def test_stale_openai_capability_snapshot_fails_closed(
    tmp_path, monkeypatch, capsys
):
    router = load_router()
    monkeypatch.setattr(router, "CATALOG_PATH", ROOT / "config" / "model-routing.json")
    observed_at = (
        datetime.now(timezone.utc) - timedelta(minutes=6)
    ).isoformat().replace("+00:00", "Z")
    snapshot = write_codex_capability_snapshot(tmp_path, observed_at=observed_at)

    result = router.main([
        "resolve", "--adapter", "codex", "--alias", "flagship", "--role", "lead",
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
        "lead": {"flagship": "ultra"},
        "orchestrator": {"flagship": "ultra"},
        "critical-review": {"flagship": "max"},
    }
    assert family["effort_fallback_order"] == ["max", "xhigh", "high", "medium", "low"]


def test_cli_headless_enumerates_no_effort_available():
    reference = (
        ROOT / "skills" / "orchestrate" / "references" / "cli-headless.md"
    ).read_text()

    assert "`no_effort_available`" in reference, (
        "CLI headless status guidance omits no_effort_available"
    )


def test_fresh_openai_snapshot_accepts_ultra_role_default(
    tmp_path, monkeypatch, capsys
):
    router = load_router()
    monkeypatch.setattr(router, "CATALOG_PATH", ROOT / "config" / "model-routing.json")
    snapshot = write_codex_capability_snapshot(tmp_path)

    result = router.main([
        "resolve", "--adapter", "codex", "--alias", "flagship", "--role", "lead",
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
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    catalog["families"]["openai"]["role_effort_defaults"]["worker"] = {
        "flagship": "ultra"
    }
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    snapshot = write_codex_capability_snapshot(
        tmp_path,
        models={
            "gpt-5.6-sol": {
                "resolved_model": "gpt-5.6-sol",
                "supported_efforts": ["max"],
            },
        },
    )
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "codex", "--alias", "flagship", "--role", "worker",
        "--capabilities-file", str(snapshot),
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 0
    assert route["requested_effort"] == "ultra"
    assert route["effort"] == "max"
    assert route["effort_capability_source"] == "runtime-model-catalog"


def test_ultra_eligible_roles_must_be_a_list_of_role_names(
    tmp_path, monkeypatch, capsys
):
    router = load_router()
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())
    catalog["families"]["openai"]["ultra_eligible_roles"] = "lead"
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    snapshot = write_codex_capability_snapshot(tmp_path)
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "codex", "--alias", "flagship", "--role", "lead",
        "--capabilities-file", str(snapshot),
    ])

    route = json.loads(capsys.readouterr().out)
    assert route["status"] == "effort_policy_config_invalid"
    assert result == 2
    assert "configuration error" in route["message"]
    assert "ultra_eligible_roles must be a list" in route["message"]


def test_explicit_effort_overrides_codex_ultra_default(tmp_path):
    snapshot = write_codex_capability_snapshot(tmp_path)
    result, route = resolve(
        "--adapter", "codex", "--alias", "flagship", "--role", "lead", "--effort", "high",
        "--capabilities-file", str(snapshot),
    )
    assert result.returncode == 0
    assert route["effort"] == "high"


def test_explicit_ultra_fails_for_noneligible_routes(tmp_path):
    snapshot = write_codex_capability_snapshot(tmp_path)
    cases = [
        (
            "--adapter", "codex", "--alias", "workhorse", "--role", "worker",
            "--capabilities-file", str(snapshot),
        ),
        ("--adapter", "claude", "--alias", "flagship", "--role", "worker"),
    ]
    for route_args in cases:
        result, route = resolve(*route_args, "--effort", "ultra")
        assert result.returncode == 1
        assert route["status"] == "effort_unsupported"
        assert route["requested_effort"] == "ultra"
        assert route["effort"] == ""


def test_codex_failure_records_never_expose_a_dispatchable_model(tmp_path):
    # Non-ok records still expose the selected runtime model so the failure
    # identifies the capability-gated route that was attempted.
    snapshot = write_codex_capability_snapshot(tmp_path)
    result, route = resolve(
        "--adapter", "codex", "--alias", "workhorse", "--role", "worker",
        "--effort", "ultra", "--capabilities-file", str(snapshot),
    )
    assert result.returncode == 1
    assert route["status"] == "effort_unsupported"
    assert route["resolved_model"] == "gpt-5.6-luna"
    assert route["identity_source"] == "runtime-capability+catalog"
    assert "catalog_model" not in route
    assert "model_selection" not in route


def test_codex_resolves_explicit_runtime_model(tmp_path):
    snapshot = write_codex_capability_snapshot(tmp_path)
    result, route = resolve(
        "--adapter", "codex", "--alias", "scout", "--role", "worker",
        "--model", "gpt-5.6-luna", "--effort", "low",
        "--capabilities-file", str(snapshot),
    )
    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["resolved_model"] == "gpt-5.6-luna"
    assert route["identity_source"] == "model-pattern"
    assert route["model_family"] == "openai"
    assert "catalog_model" not in route
    assert "model_selection" not in route


def test_caller_efforts_do_not_replace_openai_capability_snapshot():
    result, route = resolve(
        "--adapter",
        "codex",
        "--alias",
        "flagship",
        "--role",
        "lead",
        "--available-effort",
        "max",
        "--available-effort",
        "high",
    )
    assert result.returncode == 1
    assert route["status"] == "capability_discovery_failed"
    assert route["requested_effort"] == "ultra"
    assert route["effort"] == ""


def test_capability_snapshot_controls_default_fallback(
    tmp_path, monkeypatch, capsys
):
    router = load_router()
    monkeypatch.setattr(router, "CATALOG_PATH", ROOT / "config" / "model-routing.json")
    snapshot = tmp_path / "caps.json"
    snapshot.write_text(json.dumps(capability_snapshot({
            "gpt-5.6-sol": {
                "resolved_model": "gpt-5.6-sol",
                "supported_efforts": ["high", "xhigh", "max"],
            }
        })))
    result = router.main([
        "resolve", "--adapter", "codex", "--alias", "flagship", "--role", "lead",
        "--capabilities-file", str(snapshot),
    ])
    route = json.loads(capsys.readouterr().out)
    assert result == 0
    assert route["requested_effort"] == "ultra"
    assert route["effort"] == "max"
    assert route["effort_capability_source"] == "runtime-model-catalog"
    assert route["effort_substitution"] == (
        "ultra unavailable (runtime/model capability); used max"
    )


def test_default_effort_fallback_chooses_highest_supported_effort_at_or_below_request(
    tmp_path
):
    snapshot = write_codex_capability_snapshot(
        tmp_path,
        models={
            "gpt-5.6-sol": {
                "resolved_model": "gpt-5.6-sol",
                "supported_efforts": ["max", "medium"],
            },
        },
    )

    result, route = resolve(
        "--adapter", "codex", "--alias", "flagship", "--role", "worker",
        "--capabilities-file", str(snapshot),
    )

    assert result.returncode == 0
    assert route["requested_effort"] == "high"
    assert route["effort"] == "medium"
    assert route["effort_substitution"] == (
        "high unavailable (runtime/model capability); used medium"
    )


def test_task_class_effort_fallback_never_escalates_when_only_higher_effort_is_supported(
    tmp_path
):
    snapshot = write_codex_capability_snapshot(
        tmp_path,
        models={
            "gpt-5.6-luna": {
                "resolved_model": "gpt-5.6-luna",
                "supported_efforts": ["max"],
            },
        },
    )

    result, route = resolve(
        "--adapter", "codex", "--task-class", "mechanical", "--role", "worker",
        "--capabilities-file", str(snapshot),
    )

    assert route["status"] == "no_effort_available"
    assert result.returncode == 1
    assert route["requested_effort"] == "low"
    assert route["effort"] == ""
    assert route["effort_substitution"] == ""


def test_fresh_openai_snapshot_without_alias_candidate_fails_closed(
    tmp_path, monkeypatch, capsys
):
    router = load_router()
    monkeypatch.setattr(router, "CATALOG_PATH", ROOT / "config" / "model-routing.json")
    snapshot = tmp_path / "caps.json"
    snapshot.write_text(json.dumps(capability_snapshot({
            "gpt-5.6-terra": {
                "resolved_model": "gpt-5.6-terra",
                "supported_efforts": ["high", "xhigh", "max"],
            }
        })))
    result = router.main([
        "resolve", "--adapter", "codex", "--alias", "flagship", "--role", "lead",
        "--capabilities-file", str(snapshot),
    ])
    route = json.loads(capsys.readouterr().out)
    assert result == 1
    assert route["status"] == "no_candidate_available"
    assert route["candidates"] == ["gpt-5.6-sol"]
    assert route["requested_effort"] == "ultra"
    assert route["effort"] == "ultra"


def test_explicit_unsupported_effort_fails_against_runtime_snapshot(tmp_path):
    snapshot = tmp_path / "caps.json"
    snapshot.write_text(json.dumps(capability_snapshot({
            "gpt-5.6-sol": {
                "resolved_model": "gpt-5.6-sol",
                "supported_efforts": ["high", "xhigh", "max"],
            }
        })))
    result, route = resolve(
        "--adapter", "codex", "--alias", "flagship", "--role", "lead",
        "--effort", "ultra", "--capabilities-file", str(snapshot),
    )
    assert result.returncode == 1
    assert route["status"] == "effort_unsupported"
    assert route["effort"] == ""


def test_malformed_capability_snapshot_fails_closed(tmp_path):
    snapshot = tmp_path / "caps.json"
    snapshot.write_text("{}")
    result, route = resolve(
        "--adapter", "codex", "--alias", "flagship", "--role", "lead",
        "--capabilities-file", str(snapshot),
    )
    assert result.returncode == 1
    assert route["status"] == "capability_discovery_failed"


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
        "--adapter", "codex", "--alias", "flagship", "--role", "lead",
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
        "--adapter", "codex", "--alias", "flagship", "--role", "lead",
        "--capabilities-file", str(snapshot),
    )
    assert result.returncode == 1
    assert route["status"] == "capability_discovery_failed"


def test_untrusted_capability_snapshot_fails_closed(tmp_path):
    snapshot = tmp_path / "caps.json"
    snapshot.write_text(json.dumps({
        "schema_version": 1,
        "source": "forged",
        "observed_at": "2000-01-01T00:00:00Z",
        "models": {"gpt-5.6-sol": {"resolved_model": "gpt-5.6-sol", "supported_efforts": ["ultra"]}},
    }))
    result, route = resolve(
        "--adapter", "codex", "--alias", "flagship", "--role", "lead",
        "--capabilities-file", str(snapshot),
    )
    assert result.returncode == 1
    assert route["status"] == "capability_snapshot_untrusted"


def test_empty_runtime_capability_snapshot_fails_closed(tmp_path):
    snapshot = tmp_path / "caps.json"
    snapshot.write_text(json.dumps(capability_snapshot({})))
    result, route = resolve(
        "--adapter", "codex", "--alias", "flagship", "--role", "lead",
        "--capabilities-file", str(snapshot),
    )
    assert result.returncode == 1
    assert route["status"] == "capability_discovery_failed"


def test_model_id_effort_uses_last_token_and_explicit_unresolved_fails():
    result, route = resolve(
        "--adapter", "cursor", "--model", "cursor-grok-4.5-low", "--alias", "flagship",
        "--role", "reviewer", "--lead-family", "anthropic", "--require-distinct",
    )
    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["effort"] == "low"
    # Agy used to be a model-id adapter, which made every bare google alias
    # undispatchable: `agy --model gemini-3.1-pro` exits 1 asking for --effort.
    # It now carries effort in its own flag, so without a runtime snapshot the
    # effort resolves but is not capability-evidenced.
    result, route = resolve(
        "--adapter", "agy", "--model", "gemini-3.1-pro", "--alias", "flagship",
        "--role", "reviewer", "--effort", "high", "--lead-family", "openai", "--require-distinct",
    )
    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["effort"] == "high"
    assert route["effort_capability_source"] == "provider-unverified"
    result, route = resolve(
        "--adapter", "cursor", "--model", "composer-2-extra-high", "--alias", "flagship",
        "--role", "reviewer", "--lead-family", "anthropic", "--require-distinct",
    )
    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["effort"] == "xhigh"


def test_distinct_requirement_fails_closed_for_same_family():
    result, route = resolve(
        "--adapter",
        "codex",
        "--alias",
        "flagship",
        "--role",
        "reviewer",
        "--lead-family",
        "openai",
        "--require-distinct",
    )
    assert result.returncode == 1
    assert route["status"] == "same_family_forbidden"


def test_broker_route_records_endpoint_separately_from_model_family():
    result, route = resolve(
        "--adapter",
        "cursor",
        "--model",
        "cursor-grok-4.5-high",
        "--alias",
        "flagship",
        "--role",
        "reviewer",
        "--lead-family",
        "openai",
        "--require-distinct",
    )
    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["endpoint_provider"] == "cursor"
    assert route["model_family"] == "xai"
    assert route["identity_source"] == "model-pattern"
    assert route["distinct_from_lead"] is True


def test_cursor_composer_route_uses_cursor_model_family():
    result, route = resolve(
        "--adapter",
        "cursor",
        "--model",
        "composer-2-high",
        "--alias",
        "flagship",
        "--role",
        "worker",
        "--lead-family",
        "openai",
        "--require-distinct",
    )
    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["endpoint_provider"] == "cursor"
    assert route["model_family"] == "cursor-composer"
    assert route["effort"] == "high"
    assert route["distinct_from_lead"] is True


def test_agy_accepts_only_explicit_gemini_routing():
    allowed, allowed_route = resolve(
        "--adapter", "agy", "--model", "gemini-3.1-pro", "--alias", "flagship", "--role", "worker",
    )
    forbidden, forbidden_route = resolve(
        "--adapter", "agy", "--model", "grok-4", "--alias", "flagship", "--role", "worker"
    )

    assert allowed.returncode == 0
    assert allowed_route["status"] == "ok"
    assert allowed_route["model_family"] == "google"
    assert allowed_route["adapter_enabled"] is True
    assert forbidden.returncode == 1
    assert forbidden_route["status"] == "adapter_family_forbidden"


def test_opencode_accepts_only_explicit_account_catalogue_models():
    allowed, route = resolve(
        "--adapter", "opencode", "--model", "opencode/deepseek-v4-flash-free",
        "--alias", "scout", "--role", "worker",
        "--effort", "high",
    )
    assert allowed.returncode == 0
    assert route["status"] == "ok"
    assert route["model_family"] == "generic-open"
    assert route["compatibility_adapter"] == "opencode-acp"
    assert route["adapter_enabled"] is True
    assert route["effort"] == "high"

    forbidden, forbidden_route = resolve(
        "--adapter", "opencode", "--model", "anthropic/claude-opus",
        "--alias", "scout", "--role", "worker",
    )
    assert forbidden.returncode == 1
    assert forbidden_route["status"] in {"adapter_family_mismatch", "adapter_model_forbidden"}


def test_optional_adapter_preference_policy_is_ordered_and_native_first_for_fallbacks():
    catalog = json.loads((ROOT / "config" / "model-routing.json").read_text())

    agy = catalog["adapters"]["agy"]["model_family_preferences"]
    cursor = catalog["adapters"]["cursor"]["model_family_preferences"]

    assert agy == {"preferred": ["google", "anthropic"], "fallback": {}}
    assert cursor == {
        "preferred": ["xai", "cursor-composer"],
        "fallback": {"anthropic": "claude", "openai": "codex", "google": "agy"},
    }


def test_split_root_defaults_use_instance_routing_and_product_compatibility(tmp_path):
    product_root = tmp_path / "product"
    instance_root = tmp_path / "instance"
    shutil.copytree(ROOT / "scripts", product_root / "scripts")
    (product_root / "config").mkdir()
    shutil.copy2(
        ROOT / "config/adapter-compatibility.yaml",
        product_root / "config/adapter-compatibility.yaml",
    )
    (instance_root / "config").mkdir(parents=True)
    catalogue = json.loads((ROOT / "config/model-routing.json").read_text())
    catalogue["catalog_date"] = "2099-01-01"
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
            "scout",
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
    assert json.loads(result.stdout)["catalog_date"] == "2099-01-01"


def test_cursor_accepts_preferred_and_supported_fallback_families_without_model_name_locks():
    for model, family in (
        ("composer-2-high", "cursor-composer"),
        ("cursor-grok-4.5-high", "xai"),
        ("gemini-3.1-pro", "google"),
        ("claude-opus", "anthropic"),
        ("gpt-5.6-sol", "openai"),
        ("grokish-high", "xai"),
    ):
        allowed, allowed_route = resolve(
            "--adapter", "cursor", "--model", model, "--alias", "flagship", "--role", "worker",
        )
        assert allowed.returncode == 0
        assert allowed_route["status"] == "ok"
        assert allowed_route["model_family"] == family

    wrong_family, wrong_family_route = resolve(
        "--adapter", "cursor", "--model", "qwen3-coder", "--alias", "flagship", "--role", "worker"
    )
    assert wrong_family.returncode == 1
    assert wrong_family_route["status"] == "adapter_family_forbidden"


def test_kiro_accepts_only_open_weight_models():
    allowed_models = (
        ("deepseek-3.2", "deepseek"),
        ("glm-5", "zhipu"),
        ("minimax-m2.5", "minimax"),
        ("qwen3-coder-next", "alibaba"),
    )
    for model, family in allowed_models:
        allowed, allowed_route = resolve(
            "--adapter", "kiro", "--model", model, "--alias", "scout", "--role", "worker",
        )
        assert allowed.returncode == 0
        assert allowed_route["status"] == "ok"
        assert allowed_route["model_family"] == family
        assert allowed_route["compatibility_model_family"] == "open-weight"

    forbidden, forbidden_route = resolve(
        "--adapter", "kiro", "--model", "gemini-3.1-pro", "--alias", "scout", "--role", "worker"
    )

    assert forbidden.returncode == 1
    assert forbidden_route["status"] == "adapter_family_forbidden"


def test_pi_without_model_patterns_fails_closed_for_provider_families():
    for model in ("gpt-5.6-sol", "claude-opus-4.5", "gemini-3.1-pro"):
        result, route = resolve(
            "--adapter", "pi", "--model", model, "--alias", "scout", "--role", "worker"
        )
        assert result.returncode == 1
        assert route["status"] == "adapter_family_forbidden"


def test_same_family_rejection_precedes_adapter_family_rejection():
    result, route = resolve(
        "--adapter", "cursor", "--model", "gpt-5.6-sol", "--alias", "flagship",
        "--role", "reviewer", "--lead-family", "openai", "--require-distinct",
    )

    assert result.returncode == 1
    assert route["status"] == "same_family_forbidden"


@pytest.mark.parametrize(
    ("adapter", "model", "family", "effort"),
    [
        ("agy", "Gemini 3.1 Pro (High)", "google", "high"),
        ("cursor", "cursor-grok-4.5-high", "xai", "high"),
    ],
)
def test_optional_reviewers_route_directly_with_exact_identity(
    adapter, model, family, effort
):
    result, route = resolve(
        "--adapter", adapter, "--model", model, "--alias", "flagship",
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
            "--adapter", adapter, "--alias", "flagship", "--role", "lead",
            *capability_args,
        )

        assert result.returncode == 0
        assert route["status"] == "ok"
        assert route["adapter_enabled"] is True


def test_catalogue_adapter_without_compatibility_contract_routes_directly():
    arguments = (
        "--adapter", "copilot", "--model", "gemini-3.1-pro",
        "--alias", "flagship", "--role", "worker",
    )

    result, route = resolve(*arguments)

    assert result.returncode == 0
    assert route["status"] == "ok"


def write_agy_capability_snapshot(tmp_path, models=None):
    if models is None:
        # Measured against agy 1.1.10 and re-verified on 1.1.11: efforts are
        # per model, and gemini-3.1-pro genuinely has no medium.
        models = {
            "gemini-3.1-pro": {
                "resolved_model": "gemini-3.1-pro",
                "supported_efforts": ["low", "high"],
            },
            "gemini-3.6-flash": {
                "resolved_model": "gemini-3.6-flash",
                "supported_efforts": ["low", "medium", "high"],
            },
        }
    snapshot = tmp_path / "agy-caps.json"
    snapshot.write_text(json.dumps(capability_snapshot(models, source="agy models")))
    return snapshot


def test_agy_effort_is_validated_against_the_runtime_model_catalogue(tmp_path):
    snapshot = write_agy_capability_snapshot(tmp_path)

    def route_for(model, effort):
        return resolve(
            "--adapter", "agy", "--model", model, "--alias", "flagship",
            "--role", "reviewer", "--effort", effort, "--lead-family", "openai",
            "--require-distinct", "--capabilities-file", str(snapshot),
        )

    result, route = route_for("gemini-3.1-pro", "high")
    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["effort"] == "high"
    assert route["effort_capability_source"] == "runtime-model-catalog"

    # The catalogue used to offer flagship at medium, which agy cannot serve:
    # gemini-3.1-pro exposes only low and high. It must fail closed rather than
    # silently dispatching at some other effort.
    result, route = route_for("gemini-3.1-pro", "medium")
    assert result.returncode != 0
    assert route["status"] == "effort_unsupported"

    result, route = route_for("gemini-3.6-flash", "medium")
    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["effort"] == "medium"

    result, route = route_for("gemini-9-does-not-exist", "high")
    assert result.returncode != 0
    assert route["status"] == "capability_model_unavailable"
