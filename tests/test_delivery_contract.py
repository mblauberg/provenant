import copy
import json
from pathlib import Path

import pytest

from tests.support.modules import load_script

ROOT = Path(__file__).resolve().parents[1]
AUTHORITY_MAPPER_PATH = ROOT / "skills" / "deliver" / "scripts" / "authority_mapping.py"
AUTHORITY_FIXTURE_ROOT = ROOT / "tests" / "fixtures" / "authority-envelope-v2"


def load(path: Path, name: str):
    return load_script(path, name)


def delivery_authority_fixture():
    authority = json.loads((AUTHORITY_FIXTURE_ROOT / "delivery-authority.json").read_text())
    fields = (
        "schema_version", "approved_by", "evidence", "evidence_digest",
        "workspace_roots", "allowed_source_paths", "allowed_artifact_paths",
        "denied_paths", "prohibited_actions", "disclosure", "secrets_access",
        "secret_refs", "deployment", "deployment_targets",
        "irreversible_actions", "irreversible_action_ids", "network",
        "expires_at", "budget", "delegations",
    )
    return {field: authority[field] for field in fields}


def mapped_authority_fixture():
    authority = json.loads((AUTHORITY_FIXTURE_ROOT / "fabric-authority.json").read_text())
    fields = (
        "schemaVersion", "approval", "workspaceRoots", "sourcePaths",
        "artifactPaths", "deniedPaths", "prohibitedActions", "disclosure",
        "secrets", "deployment", "irreversibleActions", "network",
        "expiresAt", "budget",
    )
    return {field: authority[field] for field in fields}


def test_delivery_authority_v2_maps_to_the_cross_language_golden():
    module = load(AUTHORITY_MAPPER_PATH, "authority_mapping")
    delivery = delivery_authority_fixture()
    expected = mapped_authority_fixture()

    actual = module.map_delivery_authority(delivery)
    def canonical(value):
        return json.dumps(value, sort_keys=True, separators=(",", ":"))
    assert canonical(actual) == canonical(expected)


def test_delivery_authority_timestamp_validation_matches_protocol_vectors():
    module = load(AUTHORITY_MAPPER_PATH, "authority_mapping_timestamp_vectors")
    delivery = delivery_authority_fixture()
    vectors = json.loads((AUTHORITY_FIXTURE_ROOT / "timestamp-vectors.json").read_text())

    for timestamp in vectors["accepted"]:
        delivery["expires_at"] = timestamp
        mapped = module.map_delivery_authority(delivery)
        assert mapped["expiresAt"] == timestamp

    for timestamp in vectors["rejected"]:
        delivery["expires_at"] = timestamp
        with pytest.raises(module.AuthorityMappingError, match="strict RFC3339 timestamp"):
            module.map_delivery_authority(delivery)

    authority = mapped_authority_fixture()
    for vector in vectors["ordering"]:
        child = {**authority, "expiresAt": vector["child"]}
        parent = {**authority, "expiresAt": vector["parent"]}
        assert module.authority_contained(child, parent) is vector["contained"]


def test_delivery_authority_v2_is_closed_and_rejects_invalid_budget_units():
    module = load(AUTHORITY_MAPPER_PATH, "authority_mapping_closed")
    delivery = delivery_authority_fixture()

    missing = copy.deepcopy(delivery)
    del missing["network"]
    with pytest.raises(module.AuthorityMappingError, match="missing=.*network"):
        module.map_delivery_authority(missing)

    unknown = copy.deepcopy(delivery)
    unknown["implicit_default"] = True
    with pytest.raises(module.AuthorityMappingError, match="unknown=.*implicit_default"):
        module.map_delivery_authority(unknown)

    unknown_currency = copy.deepcopy(delivery)
    unknown_currency["budget"] = {"cost:ZZZ": 1}
    with pytest.raises(module.AuthorityMappingError, match="invalid budget unit"):
        module.map_delivery_authority(unknown_currency)


@pytest.mark.parametrize(
    "budget",
    [
        {"turns": -1},
        {"turns": 2**53},
        {
            **{f"input_tokens:provider-{index}": 1 for index in range(123)},
            **{unit: 1 for unit in ("turns", "provider_calls", "concurrent_turns", "descendants", "message_bytes", "artifact_bytes")},
        },
    ],
)
def test_delivery_authority_budget_matches_canonical_bounds(budget):
    module = load(AUTHORITY_MAPPER_PATH, "authority_mapping_budget_bounds")
    delivery = delivery_authority_fixture()
    delivery["budget"] = budget

    with pytest.raises(module.AuthorityMappingError, match="budget"):
        module.map_delivery_authority(delivery)


def test_delivery_authority_budget_accepts_canonical_128_key_boundary():
    module = load(AUTHORITY_MAPPER_PATH, "authority_mapping_budget_boundary")
    delivery = delivery_authority_fixture()
    delivery["budget"] = {
        **{f"input_tokens:provider-{index}": 1 for index in range(122)},
        **{unit: 1 for unit in ("turns", "provider_calls", "concurrent_turns", "descendants", "message_bytes", "artifact_bytes")},
    }
    mapped = module.map_delivery_authority(delivery)
    assert len(mapped["budget"]) == 128


@pytest.mark.parametrize(
    ("mutate", "message"),
    [
        (lambda authority: authority.update(workspace_roots=[f"root-{index}" for index in range(65)]), "workspace_roots"),
        (lambda authority: authority.update(allowed_source_paths=[f"src-{index}" for index in range(257)]), "allowed_source_paths"),
        (lambda authority: authority.update(allowed_artifact_paths=[f"artifact-{index}" for index in range(257)]), "allowed_artifact_paths"),
        (lambda authority: authority.update(denied_paths=[f"denied-{index}" for index in range(257)]), "denied_paths"),
        (lambda authority: authority.update(prohibited_actions=[f"action-{index}" for index in range(257)]), "prohibited_actions"),
        (lambda authority: authority.update(secret_refs=[f"secret-{index}" for index in range(257)]), "secret_refs"),
        (lambda authority: authority.update(deployment_targets=[f"target-{index}" for index in range(257)]), "deployment_targets"),
        (lambda authority: authority.update(irreversible_action_ids=[f"action-{index}" for index in range(257)]), "irreversible_action_ids"),
        (lambda authority: authority["network"].update(allowed_hosts=[f"host-{index}.test" for index in range(257)]), "allowed_hosts"),
        (lambda authority: authority.update(allowed_source_paths=["input\0escape"]), "allowed_source_paths"),
        (lambda authority: authority.update(allowed_source_paths=["x" * 4097]), "allowed_source_paths"),
        (lambda authority: authority.update(expires_at="2026-07-20T00:00Z"), "expires_at"),
        (lambda authority: authority.update(expires_at="2026-02-30T00:00:00Z"), "expires_at"),
        (lambda authority: authority.update(workspace_roots=["workspace"], allowed_source_paths=["outside"]), "allowed_source_paths"),
        (
            lambda authority: authority.update(
                workspace_roots=["workspace"],
                allowed_source_paths=["workspace/input"],
                allowed_artifact_paths=["outside"],
            ),
            "allowed_artifact_paths",
        ),
    ],
)
def test_delivery_authority_mapper_rejects_the_canonical_codec_negative_boundaries(mutate, message):
    module = load(AUTHORITY_MAPPER_PATH, f"authority_mapping_parity_{message}")
    delivery = delivery_authority_fixture()
    mutate(delivery)

    with pytest.raises(module.AuthorityMappingError, match=message):
        module.map_delivery_authority(delivery)


def test_complete_delivery_delegation_inherits_approval_and_must_narrow_every_dimension():
    module = load(AUTHORITY_MAPPER_PATH, "authority_mapping_delegation")
    delivery = delivery_authority_fixture()
    delegation = {
        "actor": "worker-1",
        "schema_version": 2,
        "workspace_roots": ["."],
        "allowed_source_paths": ["input"],
        "allowed_artifact_paths": ["output/worker-1"],
        "denied_paths": [".git"],
        "prohibited_actions": ["deployment", "external-release"],
        "disclosure": "local-only",
        "secrets_access": "none",
        "secret_refs": [],
        "deployment": False,
        "deployment_targets": [],
        "irreversible_actions": False,
        "irreversible_action_ids": [],
        "network": {"tool_egress": "none", "allowed_hosts": []},
        "expires_at": "2026-07-19T00:00:00Z",
        "budget": {"turns": 2},
    }
    delivery["delegations"] = [delegation]

    mapped = module.map_delivery_delegations(delivery)
    parent = module.map_delivery_authority(delivery)
    assert len(mapped) == 1
    assert mapped[0]["actor"] == "worker-1"
    assert mapped[0]["authority"]["artifactPaths"] == ["output/worker-1"]
    assert mapped[0]["authority"]["approval"] == parent["approval"]

    widened = copy.deepcopy(delivery)
    widened["delegations"][0]["network"] = {
        "tool_egress": "allowlist",
        "allowed_hosts": ["api.example.test", "outside.example.test"],
    }
    with pytest.raises(module.AuthorityMappingError, match="broadens parent authority"):
        module.map_delivery_delegations(widened)

    widened_budget = copy.deepcopy(delivery)
    widened_budget["delegations"][0]["budget"] = {"wall_clock_milliseconds": 1}
    with pytest.raises(module.AuthorityMappingError, match="broadens parent authority"):
        module.map_delivery_delegations(widened_budget)


def test_review_delegation_separates_source_reads_and_artifacts():
    module = load(AUTHORITY_MAPPER_PATH, "authority_mapping_review_delegation")
    delivery = delivery_authority_fixture()
    delivery["delegations"] = [{
        "actor": "reviewer-1",
        "schema_version": 2,
        "workspace_roots": ["."],
        "allowed_source_paths": ["input"],
        "allowed_artifact_paths": ["output/reviews/reviewer-1"],
        "denied_paths": [".git"],
        "prohibited_actions": ["deployment", "external-release"],
        "disclosure": "approved-providers",
        "secrets_access": "none",
        "secret_refs": [],
        "deployment": False,
        "deployment_targets": [],
        "irreversible_actions": False,
        "irreversible_action_ids": [],
        "network": {"tool_egress": "none", "allowed_hosts": []},
        "expires_at": "2026-07-19T00:00:00Z",
        "budget": {"turns": 2},
    }]

    mapped = module.map_delivery_delegations(delivery)[0]["authority"]

    assert mapped["sourcePaths"] == ["input"]
    assert mapped["artifactPaths"] == ["output/reviews/reviewer-1"]
    assert set(mapped["sourcePaths"]).isdisjoint(mapped["artifactPaths"])
