import json
from datetime import datetime, timedelta, timezone
import importlib.util
from pathlib import Path
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


def resolve(*args, adapter_gate="direct-cli"):
    arguments = [str(SCRIPT), "resolve", *args]
    if "--adapter-gate" not in args:
        arguments.extend(("--adapter-gate", adapter_gate))
    result = subprocess.run(
        arguments,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    return result, json.loads(result.stdout) if result.stdout else None


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


def load_router():
    path = ROOT / "scripts" / "model_route.py"
    spec = importlib.util.spec_from_file_location("model_route_under_test", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


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
        "--adapter-gate", "direct-cli",
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
        "--adapter-gate", "direct-cli",
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
        "--adapter-gate", "direct-cli",
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
        "--adapter-gate", "direct-cli",
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
        "--adapter-gate", "direct-cli",
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
        "--role", "worker", "--adapter-gate", "direct-cli",
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
        "--role", "worker", "--adapter-gate", "direct-cli",
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
        "--adapter-gate", "direct-cli",
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
        "--role", "worker", "--adapter-gate", "direct-cli",
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
        "--adapter-gate", "direct-cli",
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
        "--adapter-gate", "direct-cli",
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
        "--capabilities-file", str(snapshot_path), "--adapter-gate", "direct-cli",
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
        "--capabilities-file", str(snapshot_path), "--adapter-gate", "direct-cli",
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
        "--adapter-gate", "direct-cli",
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
        "--adapter-gate", "direct-cli",
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
        "--adapter-gate", "direct-cli",
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
        "--adapter-gate", "direct-cli",
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
        "--adapter-gate", "direct-cli",
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
        "--alias", "flagship", "--role", "worker", "--adapter-gate", "direct-cli",
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
        "--role", "worker", "--adapter-gate", "direct-cli",
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
        "--role", "worker", "--adapter-gate", "direct-cli",
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

    assert list(router.override_scan_families("gpt-5.6-sol", catalog)) == [
        "anthropic", "openai",
    ]

    result = router.main([
        "resolve", "--adapter", "cursor", "--alias", "flagship", "--role", "worker",
        "--model", "gpt-5.6-sol", "--adapter-gate", "direct-cli",
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
    catalog["families"]["openai"]["aliases"] = aliases
    catalog_path = tmp_path / "model-routing.json"
    catalog_path.write_text(json.dumps(catalog))
    monkeypatch.setattr(router, "CATALOG_PATH", catalog_path)

    result = router.main([
        "resolve", "--adapter", "codex", "--alias", "flagship", "--role", "worker",
        "--model", "gpt-5.6-sol", "--adapter-gate", "direct-cli",
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
        "--adapter-gate", "direct-cli",
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


def test_openai_aliases_resolve_to_account_default_dispatch(tmp_path):
    # The Codex account is a ChatGPT subscription: explicit model ids are
    # rejected by the runtime (HTTP 400), so codex routes dispatch on the
    # account default while retaining the catalog id for effort/audit (#190).
    expected = {
        "flagship": "gpt-5.6-sol",
        "workhorse": "gpt-5.6-terra",
        "scout": "gpt-5.6-luna",
    }
    snapshot = write_codex_capability_snapshot(tmp_path)
    for alias, model in expected.items():
        result, route = resolve(
            "--adapter", "codex", "--alias", alias, "--role", "worker",
            "--capabilities-file", str(snapshot),
        )
        assert result.returncode == 0
        assert route["resolved_model"] == ""
        assert route["catalog_model"] == model
        assert route["model_selection"] == "account-default"
        assert route["identity_source"] == "account-default"
        assert route["model_family"] == "openai"


def test_account_default_codex_ignores_runtime_selectable_model_list(tmp_path):
    snapshot = write_codex_capability_snapshot(tmp_path)
    result, route = resolve(
        "--adapter", "codex", "--alias", "flagship", "--role", "worker",
        "--available-model", "gpt-5.6-terra",
        "--capabilities-file", str(snapshot),
    )
    assert result.returncode == 0
    assert route["resolved_model"] == ""
    assert route["catalog_model"] == "gpt-5.6-sol"
    assert route["model_selection"] == "account-default"


def test_aliases_supply_proportionate_default_effort(tmp_path):
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
    ("task_class", "alias", "effort", "catalog_model"),
    (
        ("mechanical", "scout", "low", "gpt-5.6-luna"),
        ("legwork", "workhorse", "medium", "gpt-5.6-terra"),
        ("critical-review", "flagship", "max", "gpt-5.6-sol"),
        ("orchestration", "flagship", "ultra", "gpt-5.6-sol"),
    ),
)
def test_task_classes_bind_codex_policy_identity_without_transport_model(
    tmp_path, task_class, alias, effort, catalog_model
):
    snapshot = tmp_path / f"{task_class}.json"
    snapshot.write_text(json.dumps(capability_snapshot({
        catalog_model: {
            "resolved_model": catalog_model,
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
    assert route["resolved_model"] == ""
    assert route["catalog_model"] == catalog_model
    assert route["model_selection"] == "account-default"
    assert route["identity_source"] == "account-default"


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
        "--role", "critical-review", "--adapter-gate", "direct-cli",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "task_class_config_invalid"
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
        "--role", "worker", "--adapter-gate", "direct-cli",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "task_class_config_invalid"


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
        "--role", "critical-review", "--adapter-gate", "direct-cli",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 2
    assert route["status"] == "task_class_config_invalid"


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
        "--adapter-gate", "direct-cli",
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
        "--adapter-gate", "direct-cli",
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
        "--capabilities-file", str(snapshot), "--adapter-gate", "direct-cli",
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


def test_fresh_openai_snapshot_accepts_ultra_role_default(
    tmp_path, monkeypatch, capsys
):
    router = load_router()
    monkeypatch.setattr(router, "CATALOG_PATH", ROOT / "config" / "model-routing.json")
    snapshot = write_codex_capability_snapshot(tmp_path)

    result = router.main([
        "resolve", "--adapter", "codex", "--alias", "flagship", "--role", "lead",
        "--capabilities-file", str(snapshot), "--adapter-gate", "direct-cli",
    ])

    route = json.loads(capsys.readouterr().out)
    assert result == 0
    assert route["requested_effort"] == "ultra"
    assert route["effort"] == "ultra"
    assert route["effort_capability_source"] == "runtime-model-catalog"
    assert route["effort_substitution"] == ""


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
    # Non-ok records must not present the catalog id as resolved_model:
    # a consumer keying on resolved_model would dispatch an id the
    # subscription runtime rejects (#190).
    snapshot = write_codex_capability_snapshot(tmp_path)
    result, route = resolve(
        "--adapter", "codex", "--alias", "workhorse", "--role", "worker",
        "--effort", "ultra", "--capabilities-file", str(snapshot),
    )
    assert result.returncode == 1
    assert route["status"] == "effort_unsupported"
    assert route["resolved_model"] == ""
    assert route["catalog_model"] == "gpt-5.6-terra"
    assert route["model_selection"] == "account-default"


def test_codex_rejects_explicit_model_for_account_default_adapter():
    # An explicit id would be sent to the runtime and rejected with HTTP 400,
    # so the resolver fails closed instead of emitting a doomed route (#190).
    result, route = resolve(
        "--adapter", "codex", "--alias", "flagship", "--role", "lead", "--model", "gpt-5.6-sol"
    )
    assert result.returncode == 1
    assert route["status"] == "adapter_account_default_only"
    assert route["resolved_model"] == ""
    assert route["requested_model"] == "gpt-5.6-sol"
    assert route["catalog_model"] == "gpt-5.6-sol"
    assert route["model_selection"] == "account-default"
    assert route["identity_source"] == "account-default"
    assert route["model_family"] == "openai"


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
        "--adapter-gate", "direct-cli",
    ])
    route = json.loads(capsys.readouterr().out)
    assert result == 0
    assert route["requested_effort"] == "ultra"
    assert route["effort"] == "max"
    assert route["effort_capability_source"] == "runtime-model-catalog"
    assert route["effort_substitution"] == (
        "ultra unavailable (runtime/model capability); used max"
    )


def test_fresh_openai_snapshot_missing_catalog_model_fails_closed(
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
        "--adapter-gate", "direct-cli",
    ])
    route = json.loads(capsys.readouterr().out)
    assert result == 1
    assert route["status"] == "capability_model_unavailable"
    assert route["catalog_model"] == "gpt-5.6-sol"
    assert route["resolved_model"] == ""
    assert route["requested_effort"] == "ultra"
    assert route["effort"] == ""
    assert route["effort_capability_source"] == "runtime-model-catalog"


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
        "--adapter-gate", "direct-cli",
    )
    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["effort"] == "low"
    result, route = resolve(
        "--adapter", "agy", "--model", "Gemini 3.1 Pro (High)", "--alias", "flagship",
        "--role", "reviewer", "--effort", "high", "--lead-family", "openai", "--require-distinct",
        "--adapter-gate", "direct-cli",
    )
    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["effort"] == "high"
    assert route["effort_capability_source"] == "model-id"
    result, route = resolve(
        "--adapter", "cursor", "--model", "composer-2-extra-high", "--alias", "flagship",
        "--role", "reviewer", "--lead-family", "anthropic", "--require-distinct",
        "--adapter-gate", "direct-cli",
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
        "--adapter-gate",
        "direct-cli",
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
        "--adapter-gate",
        "direct-cli",
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
        "--adapter-gate",
        "direct-cli",
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
        "--adapter-gate", "direct-cli",
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
        "--alias", "scout", "--role", "worker", "--adapter-gate", "fabric",
        "--effort", "high",
    )
    assert allowed.returncode == 0
    assert route["status"] == "ok"
    assert route["model_family"] == "generic-open"
    assert route["compatibility_adapter"] == "opencode-acp"
    assert route["adapter_active"] is True
    assert route["effort"] == "high"

    forbidden, forbidden_route = resolve(
        "--adapter", "opencode", "--model", "anthropic/claude-opus",
        "--alias", "scout", "--role", "worker", "--adapter-gate", "fabric",
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
            "--adapter-gate", "direct-cli",
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
            "--adapter-gate", "direct-cli",
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
def test_activated_optional_reviewers_route_through_fabric_with_exact_identity(
    adapter, model, family, effort
):
    result, route = resolve(
        "--adapter", adapter, "--model", model, "--alias", "flagship",
        "--role", "reviewer", "--effort", effort,
        "--lead-family", "openai", "--require-distinct",
        adapter_gate="fabric",
    )

    assert result.returncode == 0
    assert route["status"] == "ok"
    assert route["resolved_model"] == model
    assert route["model_family"] == family
    assert route["effort"] == effort
    assert route["adapter_enabled"] is True
    assert route["adapter_active"] is True
    assert route["adapter_unresolved_pins"] == []


def test_primary_adapters_honour_fabric_activation_gate(tmp_path):
    snapshot = write_codex_capability_snapshot(tmp_path)
    for adapter in ("claude", "codex"):
        capability_args = (
            ("--capabilities-file", str(snapshot)) if adapter == "codex" else ()
        )
        fabric, fabric_route = resolve(
            "--adapter", adapter, "--alias", "flagship", "--role", "lead",
            *capability_args,
            adapter_gate="fabric",
        )
        direct, direct_route = resolve(
            "--adapter", adapter, "--alias", "flagship", "--role", "lead",
            *capability_args,
            "--adapter-gate", "direct-cli",
        )

        assert fabric.returncode == 0
        assert fabric_route["status"] == "ok"
        assert fabric_route["adapter_enabled"] is True
        assert fabric_route["adapter_unresolved_pins"] == []
        assert direct.returncode == 0
        assert direct_route["status"] == "ok"


def test_fabric_gate_rejects_catalogue_adapter_without_compatibility_contract():
    arguments = (
        "--adapter", "copilot", "--model", "gemini-3.1-pro",
        "--alias", "flagship", "--role", "worker",
    )

    fabric, fabric_route = resolve(*arguments, adapter_gate="fabric")
    direct, direct_route = resolve(*arguments)

    assert fabric.returncode == 2
    assert fabric_route["status"] == "adapter_compatibility_unknown"
    assert direct.returncode == 0
    assert direct_route["status"] == "ok"


def test_fabric_gate_rejects_inactive_adapter_before_dispatch(tmp_path):
    fabric_config = tmp_path / "agent-fabric.yaml"
    fabric_config.write_text("schemaVersion: 1\nactiveAdapters: []\n")

    result, route = resolve(
        "--adapter", "agy", "--model", "gemini-3.1-pro", "--alias", "flagship",
        "--role", "reviewer", "--lead-family", "openai", "--require-distinct",
        "--fabric-config", str(fabric_config), adapter_gate="fabric",
    )

    assert result.returncode == 1
    assert route["status"] == "adapter_inactive"
    assert route["adapter_enabled"] is True


def test_fabric_gate_fails_closed_for_invalid_activation_config(tmp_path):
    fabric_config = tmp_path / "agent-fabric.yaml"
    fabric_config.write_text("schemaVersion: 1\nactiveAdapters: agy\n")

    result, route = resolve(
        "--adapter", "agy", "--model", "gemini-3.1-pro", "--alias", "flagship",
        "--role", "worker", "--fabric-config", str(fabric_config), adapter_gate="fabric",
    )

    assert result.returncode == 2
    assert route["status"] == "fabric_activation_invalid"
