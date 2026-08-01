from collections import Counter
import hashlib
import json
from pathlib import Path
import shutil
import subprocess

import pytest
import yaml

from scripts import validate_skill_routing_evaluation as routing_validator


ROOT = Path(__file__).resolve().parents[1]


FROZEN_CURRENT_ROUTING_PROTOCOL = {
    "evaluation_id": "skill-portfolio-routing-20260719-fabric-v7",
    "frozen_at": "2026-07-19T01:51:57Z",
    "providers": [
        {
            "adapter": "agy",
            "effort": "high",
            "family": "google",
            "model": "Gemini 3.1 Pro (High)",
        },
        {
            "adapter": "cursor-agent",
            "effort": "high",
            "family": "xai",
            "model": "cursor-grok-4.5-high",
        },
    ],
}


def load(path: Path):
    return yaml.safe_load(path.read_text())


def test_every_skill_has_canonical_positive_negative_and_boundary_routes():
    skills = {path.parent.name for path in (ROOT / "skills").glob("*/SKILL.md")}
    seen_ids = set()
    seen_prompts = set()

    for skill in sorted(skills):
        path = ROOT / "skills" / skill / "evals" / "trigger_cases.yaml"
        assert path.is_file(), f"missing trigger fixture: {skill}"
        data = load(path)
        assert set(data) == {"schema_version", "target_skill", "cases"}
        assert data["schema_version"] == 1
        assert data["target_skill"] == skill
        assert isinstance(data["cases"], list)
        assert Counter(case["relation"] for case in data["cases"]) == {
            "positive": 3,
            "negative": 3,
            "boundary": 3,
        }

        for case in data["cases"]:
            assert set(case) == {"id", "relation", "prompt", "tags", "expected"}
            assert case["id"].startswith("q") and case["id"][1:].isdigit()
            assert case["id"] not in seen_ids
            seen_ids.add(case["id"])
            assert isinstance(case["prompt"], str) and case["prompt"].strip()
            assert case["prompt"] not in seen_prompts
            seen_prompts.add(case["prompt"])
            assert isinstance(case["tags"], list) and case["tags"]

            expected = case["expected"]
            assert set(expected) == {"primary_skill", "companion_skills"}
            primary = expected["primary_skill"]
            companions = expected["companion_skills"]
            assert primary is None or primary in skills
            assert isinstance(companions, list) and len(companions) == len(set(companions))
            assert set(companions) <= skills
            assert primary not in companions

            if case["relation"] == "positive":
                assert primary == skill
            elif case["relation"] == "negative":
                assert primary != skill and skill not in companions
            else:
                assert {"adjacent", "composition"} & set(case["tags"])

            if primary is None:
                assert companions == []
                assert "no-skill" in case["tags"]


def test_code_review_discipline_cases_have_prompt_and_expected_behaviour():
    data = load(ROOT / "skills" / "code-review" / "evals" / "discipline_cases.yaml")
    assert len(data["cases"]) >= 4
    for case in data["cases"]:
        assert set(case) == {"prompt", "expected"}
        assert case["prompt"].strip()
        assert case["expected"].strip()


def write_completed_current_fixture(tmp_path: Path) -> tuple[Path, Path]:
    source_root = ROOT / "docs" / "evals" / "skill-portfolio-2026"
    root = tmp_path / source_root.name
    shutil.copytree(source_root, root)
    plan_path = root / "routing-protocol.json"
    holdout_path = root / "routing-holdout.yaml"
    plan = json.loads(plan_path.read_text())
    holdout = load(holdout_path)
    plan["execution"] = {"attempts_started": 6, "status": "completed"}
    plan_path.write_text(json.dumps(plan, indent=2) + "\n")
    summary_path = root / "summary.json"
    summary = json.loads(summary_path.read_text())
    current = summary["current_routing_regression"]
    current["attempts_started"] = 6
    current["status"] = "completed"
    current.pop("blocked_reason", None)
    summary_path.write_text(json.dumps(summary, indent=2) + "\n")

    route_receipts = []
    retained_lineages = []
    for attempt_number in range(1, 7):
        attempt_id = f"attempt-{attempt_number}"
        provider = plan["schedule"]["providers"][(attempt_number - 1) % 2]
        lineage = {
            "adapter": provider["adapter"],
            "family": provider["family"],
            "requested_adapter": provider["adapter"],
            "requested_family": provider["family"],
            "requested_model": provider["model"],
            "actual_model": provider["model"],
            "requested_effort": provider["effort"],
            "effective_effort": provider["effort"],
            "substitution_reason": "",
        }
        receipt = {
            "evaluation_id": plan["evaluation_id"],
            "attempt_id": attempt_id,
            "route_id": f"route-{attempt_id}",
            "receipt_id": f"receipt-{attempt_id}",
            "lineage": lineage,
        }
        receipt_path = root / "receipts" / f"{attempt_id}.json"
        receipt_path.parent.mkdir(parents=True, exist_ok=True)
        receipt_path.write_text(json.dumps(receipt, sort_keys=True) + "\n")
        route_receipts.append({
            "route_id": receipt["route_id"],
            "receipt_id": receipt["receipt_id"],
            "artifact": {
                "path": f"receipts/{attempt_id}.json",
                "sha256": "sha256:" + hashlib.sha256(receipt_path.read_bytes()).hexdigest(),
            },
        })
        retained_lineages.append(lineage)

    attempts = []
    for index, (repetition, provider) in enumerate(
        ((repetition, provider) for repetition in range(1, 4) for provider in plan["schedule"]["providers"]),
        start=1,
    ):
        attempts.append({
            "id": f"attempt-{index}",
            "repetition": repetition,
            "status": "success",
            "disposition": "used",
            "lineage": retained_lineages[index - 1],
            "route_receipt": route_receipts[index - 1],
        })

    case_results = []
    for attempt in attempts:
        for case in holdout["cases"]:
            case_results.append({
                "attempt_id": attempt["id"],
                "case_id": case["id"],
                "status": "pass",
                "primary_correct": True,
                "companion_correct": True,
            })

    (root / "routing-result.json").write_text(json.dumps({
        "schema_version": 2,
        "evaluation_id": plan["evaluation_id"],
        "protocol": {
            "path": "routing-protocol.json",
            "sha256": "sha256:" + hashlib.sha256(plan_path.read_bytes()).hexdigest(),
            "frozen_sha256": "sha256:94fd8c01aa8a30e3387c4100d84011868798f941dd229653fe0de0f56b9b75fe",
        },
        "source": {
            "path": "routing-holdout.yaml",
            "sha256": "sha256:" + hashlib.sha256(holdout_path.read_bytes()).hexdigest(),
            "frozen_sha256": "sha256:a98ce4d24e783869cfcd787d4c8d89b7591bd2602742f1e8caad84ab3363e5db",
        },
        "dataset": {
            "cases": len(holdout["cases"]),
            "case_ids": [case["id"] for case in holdout["cases"]],
        },
        "catalogue": {"owner_count": plan["catalogue"]["owner_count"]},
        "schedule": {"attempts": 6, "case_rows": 108, "families": ["google", "xai"], "repetitions": 3},
        "attempts": attempts,
        "case_results": case_results,
        "results": {
            "accounting": {
                "planned": 108, "passed": 108, "failed": 0, "omitted": 0,
                "skipped": 0, "excluded": 0, "timed_out": 0, "invalid": 0,
                "tool_errors": 0,
            },
            "attempt_accounting": {
                "planned": 6, "base_planned": 6, "retries": 0, "succeeded": 6,
                "timed_out": 0, "invalid_output": 0, "tool_errors": 0,
                "skipped": 0, "excluded": 0,
            },
            "metrics": {
                "primary_accuracy": {
                    "numerator": 108, "denominator": 108, "value": 1.0,
                    "threshold": 1.0, "passed": True,
                },
                "companion_fidelity": {
                    "numerator": 108, "denominator": 108, "value": 1.0,
                    "threshold": 0.9, "passed": True,
                },
                "critical_case_failures": 0,
            },
        },
    }, indent=2) + "\n")
    return root, root / "routing-result.json"


def test_current_portfolio_routing_plan_matches_the_live_catalogue_and_fixture_contract():
    root = ROOT / "docs" / "evals" / "skill-portfolio-2026"
    skills = sorted(path.parent.name for path in (ROOT / "skills").glob("*/SKILL.md"))
    plan = json.loads((root / "routing-protocol.json").read_text())
    holdout = load(root / plan["dataset"]["path"])

    assert plan["catalogue"]["owners"] == skills
    assert plan["catalogue"]["owner_count"] == len(skills) == 33
    assert plan["dataset"]["id"] == holdout["dataset_id"]
    assert plan["dataset"]["cases"] == len(holdout["cases"]) == 18
    assert {
        "evaluation_id": plan["evaluation_id"],
        "frozen_at": plan["frozen_at"],
        "providers": plan["schedule"]["providers"],
    } == FROZEN_CURRENT_ROUTING_PROTOCOL
    routing_validator.validate_current_fixture(root)

    valid_skills = set(skills)
    for case in holdout["cases"]:
        expected = case["expected"]
        names = [
            expected["primary_skill"],
            *expected["required_companion_skills"],
            *expected["allowed_companion_skills"],
        ]
        assert {name for name in names if name is not None} <= valid_skills


def test_current_portfolio_routing_fixture_accepts_a_digest_bound_completed_result(tmp_path):
    root, _ = write_completed_current_fixture(tmp_path)

    routing_validator.validate_current_fixture(root)


@pytest.mark.parametrize("mutation, message", [
    ("missing-result", "requires a result"),
    ("wrong-protocol", "protocol artifact digest does not match"),
    ("wrong-digest", "source artifact digest does not match"),
    ("wrong-owner-count", "owner count is invalid"),
    ("rebound-protocol", "protocol digest is not frozen"),
    ("rebound-source", "source digest is not frozen"),
    ("partial-execution", "exactly six attempts"),
    ("missing-attempt", "does not retain exactly six attempts"),
    ("missing-case-row", "case-result rows are invalid"),
    ("summary-cases", "summary cases are invalid"),
    ("summary-owners", "summary owner count is invalid"),
    ("summary-dependencies", "summary dependencies are invalid"),
    ("accounting", "case accounting is invalid"),
    ("metrics", "metrics are invalid"),
    ("omitted-pass", "case-result rows are invalid"),
    ("attempt-case-state-drift", "terminal state does not match its attempt"),
    ("missing-lineage", "provider lineage is invalid"),
    ("undeclared-substitution", "substitution reason is required"),
    ("lineage-receipt-drift", "route receipt is invalid"),
    ("missing-route-receipt", "route receipt artifact digest does not match"),
    ("absent-route-receipt", "attempt evidence is invalid"),
    ("planned-lineage-echo", "current result keys are invalid"),
    ("extra-execution", "completed execution shape is invalid"),
])
def test_current_portfolio_completed_fixture_rejects_unbound_or_mismatched_result(
    tmp_path, mutation, message,
):
    root, result_path = write_completed_current_fixture(tmp_path)
    plan_path = root / "routing-protocol.json"
    plan = json.loads(plan_path.read_text())
    summary_path = root / "summary.json"
    summary = json.loads(summary_path.read_text())
    current = summary["current_routing_regression"]
    if mutation == "missing-result":
        result_path.unlink()
    else:
        result = json.loads(result_path.read_text())
        if mutation == "wrong-protocol":
            result["protocol"]["sha256"] = "sha256:" + "0" * 64
        elif mutation == "wrong-digest":
            result["source"]["sha256"] = "sha256:" + "0" * 64
        elif mutation == "rebound-protocol":
            plan_path = root / "routing-protocol.json"
            plan = json.loads(plan_path.read_text())
            plan["classifier"]["instruction"] += " changed"
            plan_path.write_text(json.dumps(plan, indent=2) + "\n")
            result["protocol"]["sha256"] = "sha256:" + hashlib.sha256(plan_path.read_bytes()).hexdigest()
        elif mutation == "rebound-source":
            source_path = root / "routing-holdout.yaml"
            source_path.write_text(source_path.read_text() + "\n")
            result["source"]["sha256"] = "sha256:" + hashlib.sha256(source_path.read_bytes()).hexdigest()
        elif mutation == "partial-execution":
            plan["execution"]["attempts_started"] = 1
            plan_path.write_text(json.dumps(plan, indent=2) + "\n")
        elif mutation == "missing-attempt":
            result["attempts"].pop()
        elif mutation == "missing-case-row":
            result["case_results"].pop()
        elif mutation == "summary-cases":
            current["cases"] = 1
            summary_path.write_text(json.dumps(summary, indent=2) + "\n")
        elif mutation == "summary-owners":
            current["catalogue_owner_count"] = 1
            summary_path.write_text(json.dumps(summary, indent=2) + "\n")
        elif mutation == "summary-dependencies":
            current["dependencies"] = ["fabric://invented"]
            summary_path.write_text(json.dumps(summary, indent=2) + "\n")
        elif mutation == "accounting":
            result["results"]["accounting"]["passed"] = 107
        elif mutation == "metrics":
            result["results"]["metrics"]["primary_accuracy"]["numerator"] = 107
        elif mutation == "omitted-pass":
            result["case_results"][0]["status"] = "omitted"
        elif mutation == "attempt-case-state-drift":
            result["attempts"][0]["status"] = "skipped"
            result["attempts"][0]["disposition"] = "unavailable"
            result["results"]["attempt_accounting"].update({
                "succeeded": 5,
                "skipped": 1,
            })
        elif mutation == "missing-lineage":
            del result["attempts"][0]["lineage"]["actual_model"]
        elif mutation == "undeclared-substitution":
            result["attempts"][0]["lineage"]["actual_model"] = "other-model"
        elif mutation == "lineage-receipt-drift":
            result["attempts"][0]["lineage"]["actual_model"] = "other-model"
            result["attempts"][0]["lineage"]["substitution_reason"] = "provider substitution"
            result["attempts"][0]["disposition"] = "substituted"
        elif mutation == "missing-route-receipt":
            result["attempts"][0]["route_receipt"]["artifact"]["sha256"] = "sha256:" + "0" * 64
        elif mutation == "absent-route-receipt":
            del result["attempts"][0]["route_receipt"]
        elif mutation == "planned-lineage-echo":
            result["provider_lineage"] = plan["schedule"]["providers"]
        elif mutation == "extra-execution":
            plan["execution"]["extra"] = True
            plan_path.write_text(json.dumps(plan, indent=2) + "\n")
        else:
            result["catalogue"]["owner_count"] = 32
        result_path.write_text(json.dumps(result, indent=2) + "\n")

    with pytest.raises(routing_validator.Invalid, match=message):
        routing_validator.validate_current_fixture(root)


@pytest.mark.parametrize("mutation", ["removed", "replaced", "mismatched"])
def test_current_portfolio_planned_fixture_rejects_dependency_drift(tmp_path, mutation):
    source_root = ROOT / "docs" / "evals" / "skill-portfolio-2026"
    root = tmp_path / source_root.name
    shutil.copytree(source_root, root)
    plan_path = root / "routing-protocol.json"
    summary_path = root / "summary.json"
    plan = json.loads(plan_path.read_text())
    summary = json.loads(summary_path.read_text())
    if mutation == "removed":
        plan["execution"].pop("dependencies")
    elif mutation == "replaced":
        plan["execution"]["dependencies"] = ["fabric://invented"]
    else:
        summary["current_routing_regression"]["dependencies"] = ["fabric://invented"]
    plan_path.write_text(json.dumps(plan, indent=2) + "\n")
    summary_path.write_text(json.dumps(summary, indent=2) + "\n")

    with pytest.raises(routing_validator.Invalid, match="planned current dependencies"):
        routing_validator.validate_current_fixture(root)


def test_portfolio_routing_summary_retains_a_self_consistent_predecessor_result():
    root = ROOT / "docs" / "evals" / "skill-portfolio-2026"
    archive = root / "predecessor"
    summary = json.loads((root / "summary.json").read_text())["predecessor_routing_regression"]
    result = json.loads((archive / "routing-result-20260714.json").read_text())
    plan = json.loads((archive / "routing-protocol-20260714.json").read_text())
    repository = result["repository"]
    assert summary["evaluation_id"] == result["evaluation_id"]
    assert summary["repository"] == repository == plan["repository"]
    assert set(repository) == {"commit", "path"}
    assert len(repository["commit"]) == 40
    historical_root = repository["path"]
    historical_result = json.loads(subprocess.run(
        ["git", "show", f'{repository["commit"]}:{historical_root}/routing-result.json'],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout)
    subprocess.run(
        ["git", "cat-file", "-e", f'{repository["commit"]}:{historical_root}/{result["dataset"]["path"]}'],
        cwd=ROOT,
        check=True,
        capture_output=True,
    )
    assert historical_result["evaluation_id"] == result["evaluation_id"]
    assert historical_result["metrics"] == result["metrics"]
    assert historical_result["lineage"] == result["lineage"]
    assert summary["case_rows"] == result["schedule"]["case_rows"]
    for name in ("primary_accuracy", "companion_fidelity"):
        assert summary[name] == result["metrics"][name]["value"]
        assert summary[f"{name.split('_')[0]}_threshold"] == result["metrics"][name]["threshold"]
        assert result["metrics"][name]["passed"] is True
    assert result["metrics"]["critical_case_failures"] == 0


def test_portfolio_summary_retains_bounded_failure_lineage():
    root = ROOT / "docs" / "evals" / "skill-portfolio-2026"
    summary = json.loads((root / "summary.json").read_text())
    nonpasses = summary["retained_nonpasses"]

    assert {item["status"] for item in nonpasses} == {"incomplete", "fail", "cancelled"}
    assert {item["evaluation_id"] for item in nonpasses} >= {
        "skill-portfolio-routing-20260711-v2",
        "skill-portfolio-routing-20260711-v3",
        "skill-portfolio-routing-20260711-v4",
        "skill-portfolio-routing-20260711-v5",
    }
    for item in nonpasses:
        assert item["reason"].strip()


def test_research_currentness_routes_live_work_out_of_dated_recommendations():
    research = ROOT / "docs" / "research"
    index = (research / "README.md").read_text()
    portfolio = (research / "skill-portfolio-practices-2026.md").read_text()

    assert "GitHub issues and Project Status" in index
    assert "11 July historical dispositions, not current work" in portfolio
    assert "../specs/agent-fabric/effects.md" in portfolio
    for issue in (141, 328, 330):
        assert f"https://github.com/mblauberg/provenant/issues/{issue}" in portfolio

    for stale_work_label in (
        "P1 scoped follow-up",
        "P1 follow-up:",
        "P2 experiment",
        "P2 prototype",
        "remaining P1 architecture proposals",
    ):
        assert stale_work_label not in portfolio


def test_live_opencode_research_defers_activation_state_to_configuration():
    provider_boundary = (
        ROOT / "docs" / "research" / "provider-adapter-and-runtime-boundaries.md"
    ).read_text()
    continuity_snapshot = (
        ROOT
        / "docs"
        / "research"
        / "evidence-snapshots"
        / "agent-continuity-routing-2026-07.md"
    ).read_text()

    assert "OpenCode's current activation state is owned by" in provider_boundary
    assert "OpenCode is an enabled" not in provider_boundary
    assert "The enabled OpenCode route" not in provider_boundary
    assert "It is now an enabled optional adapter" not in continuity_snapshot
