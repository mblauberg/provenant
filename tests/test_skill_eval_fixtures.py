from collections import Counter
import json
from pathlib import Path
import subprocess

import yaml


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


def test_current_portfolio_routing_plan_matches_the_live_catalogue_and_has_no_result():
    root = ROOT / "docs" / "evals" / "skill-portfolio-2026"
    skills = sorted(path.parent.name for path in (ROOT / "skills").glob("*/SKILL.md"))
    summary = json.loads((root / "summary.json").read_text())["current_routing_regression"]
    plan = json.loads((root / "routing-protocol.json").read_text())
    holdout = load(root / plan["dataset"]["path"])

    assert plan["catalogue"]["owners"] == skills
    assert plan["catalogue"]["owner_count"] == len(skills) == 32
    assert "project-activation" not in plan["catalogue"]["owners"]
    assert summary["catalogue_owner_count"] == plan["catalogue"]["owner_count"]
    assert plan["dataset"]["id"] == holdout["dataset_id"]
    assert plan["dataset"]["cases"] == len(holdout["cases"]) == 18
    assert plan["execution"] == {
        "attempts_started": 0,
        "blocked_reason": "FABRIC-ROUNDTRIP-UNAVAILABLE",
        "dependencies": ["https://github.com/mblauberg/provenant/issues/330"],
        "status": "planned-unexecuted",
    }
    assert {
        "evaluation_id": plan["evaluation_id"],
        "frozen_at": plan["frozen_at"],
        "providers": plan["schedule"]["providers"],
    } == FROZEN_CURRENT_ROUTING_PROTOCOL
    assert summary["evaluation_id"] == plan["evaluation_id"]
    assert summary["dependencies"] == plan["execution"]["dependencies"]
    assert summary["attempts_started"] == 0
    assert summary["blocked_reason"] == "FABRIC-ROUNDTRIP-UNAVAILABLE"
    assert summary["status"] == "outstanding"
    assert not (root / "routing-result.json").exists()

    valid_skills = set(skills)
    for case in holdout["cases"]:
        expected = case["expected"]
        names = [
            expected["primary_skill"],
            *expected["required_companion_skills"],
            *expected["allowed_companion_skills"],
        ]
        assert {name for name in names if name is not None} <= valid_skills


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
    assert "typed effects owners" in portfolio
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
