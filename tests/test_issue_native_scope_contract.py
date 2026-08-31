from pathlib import Path
import json


ROOT = Path(__file__).resolve().parents[1]


def read(relative: str) -> str:
    return (ROOT / relative).read_text()


def test_repository_declares_the_canonical_scope_and_story_home():
    maintaining = read("MAINTAINING.md")
    template = read("skills/setup-repo/templates/repo-declarations.md")

    assert "### Scope and stories" in maintaining
    assert "Canonical home: issue-tracker" in maintaining
    assert "https://github.com/mblauberg/provenant/issues" in maintaining
    assert "### Scope and stories" in template
    assert "<issue-tracker|project-docs>" in template
    assert "### Workflow state" in maintaining
    assert "- Owner: tracker" in maintaining
    assert "https://github.com/mblauberg/provenant/projects/2" in maintaining
    assert "### Workflow state" in template
    assert "<tracker|project-docs>" in template


def test_scope_and_story_owner_is_respected_by_the_six_workflow_skills():
    for name in (
        "setup-repo",
        "scope",
        "engineering-docs",
        "grill-me",
        "work-map",
        "session",
    ):
        source = " ".join(read(f"skills/{name}/SKILL.md").lower().split())
        assert "repository process" in source, name
        assert "scope and stories" in source or "scope/story" in source, name
        assert "canonical" in source, name


def test_issue_native_work_map_and_state_rules_are_explicit():
    work_map = read("skills/work-map/SKILL.md")
    session = read("skills/session/SKILL.md")
    autopilot = read("skills/autopilot/SKILL.md")

    assert "parent tracker issue is the work map" in work_map.lower()
    assert "link-only" in work_map.lower()
    assert "no default rolling project state" in session.lower()
    assert "run-local" in autopilot.lower()
    assert "state.md" in autopilot.lower()


def test_issue_native_decision_and_spec_reconciliation_are_recorded():
    adr = read("docs/adr/0017-specifications-own-non-derivable-intent.md")
    adr_0009 = read("docs/adr/0009-standalone-semantic-specifications.md")
    adr_0011 = read("docs/adr/0011-github-owns-work-state.md")
    disclosure = read("docs/specs/harness/disclosure-refactor.md")
    lifecycle = read("docs/specs/harness/lifecycle.md")
    contract = json.loads(read("skills/deliver/contract/lifecycle.v1.json"))

    assert "issue-native change scope" in adr.lower()
    assert "Git retains durable invariants" in adr
    assert "0017" in adr_0009
    assert "issue-native" in adr_0011.lower()
    assert "implementation status:" not in disclosure.lower()
    assert "## Landed implementation train" not in disclosure
    assert "## Historical Fabric custody note" not in disclosure
    assert "Issue #23" not in lifecycle
    assert "current issue" in lifecycle.lower()
    for stale in (
        "routeEvaluationEvidenceV1",
        "evaluatedRouteIdentityV1",
        "topologyWavePlanV1",
        "repeated Fable routing receipts",
        "## Required delivery sequence",
        "### Implementation evidence",
    ):
        assert stale not in lifecycle
    assert "live/provider semantic held-outs" in lifecycle
    assert "deterministic contract and fixture tests" in lifecycle
    assert "dispatch and batch receipts own" in lifecycle.lower()
    assert "fabric only" in lifecycle.lower()
    assert "issue or receipt records scope or evidence; it is not approval" in lifecycle.lower()
    assert "This specification grants no authority" in lifecycle
    assert "current daemonless SQLite Fabric bus" in lifecycle
    assert "#route-and-topology-evaluation-evidence" not in read(
        "docs/research/native-orchestration-and-discovery-surfaces.md"
    )
    assert "#route-and-topology-evidence-boundary" in read(
        "docs/research/native-orchestration-and-discovery-surfaces.md"
    )
    assert "#route-and-topology-evidence-boundary" in read(
        "docs/research/evidence-based-provider-routing.md"
    )
    runbook = read("docs/runbooks/github-workflow.md")
    assert "### Native sub-issues" in runbook
    assert "sub_issues" in runbook
    assert "sub_issue_id" in runbook
    assert "CHILD_NUMBER --jq .id" in runbook
    assert "not the relationship" in runbook
    assert "Do not mirror children in a checklist" in runbook
    assert "#" in contract["source"]["state_graph"]
    assert ":91-95" not in contract["source"]["state_graph"]
