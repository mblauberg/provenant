from pathlib import Path
import json

import yaml


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
    expected_rules = {
        "setup-repo": "parent issue is the canonical change scope/story home",
        "scope": "declared parent issue or project-docs home",
        "engineering-docs": "parent issues own canonical scope and stories",
        "grill-me": "parent tracker issue owns the scope/story",
        "work-map": "parent tracker issue is the work map",
        "session": "declared scope/story and workflow-state owners",
    }
    for name, expected_rule in expected_rules.items():
        source = " ".join(read(f"skills/{name}/SKILL.md").lower().split())
        assert "repository process" in source, name
        assert expected_rule in source, name


def test_issue_native_work_map_and_state_rules_are_explicit():
    work_map = read("skills/work-map/SKILL.md")
    session = read("skills/session/SKILL.md")
    autopilot = read("skills/autopilot/SKILL.md")
    architecture = read("docs/ARCHITECTURE.md")
    compact_architecture = " ".join(architecture.lower().split())

    assert "parent tracker issue is the work map" in work_map.lower()
    assert "link-only" in work_map.lower()
    assert "no default rolling project state" in session.lower()
    assert "run-local" in autopilot.lower()
    assert "state.md" in autopilot.lower()
    assert "only for a declared" in compact_architecture
    assert "unavailable-tracker or cross-tracker route" in compact_architecture
    assert "never mirrors live work" in compact_architecture


def test_issue_native_user_gates_do_not_fork_into_a_markdown_register():
    scope = " ".join(read("skills/scope/SKILL.md").lower().split())
    engineering_docs = " ".join(
        read("skills/engineering-docs/SKILL.md").lower().split()
    )

    assert "user gates | declared scope/story owner" in scope
    assert "never create in issue-tracker mode" in engineering_docs


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
    assert "## Historical problem (July 2026)" in disclosure
    assert "~34 lines" not in disclosure
    assert "current issue" not in lifecycle.lower()
    assert "live/provider semantic held-outs" in lifecycle
    assert "deterministic contract and fixture tests" in lifecycle
    assert "dispatch and batch receipts own" in lifecycle.lower()
    assert "fabric only" in lifecycle.lower()
    assert "issue or receipt records scope or evidence; it is not approval" in lifecycle.lower()
    assert "This specification grants no authority" in lifecycle
    assert "Non-code delivery has no shared executable contract" not in lifecycle
    assert "or a future `deliver`" not in lifecycle
    assert "current daemonless SQLite Fabric bus" in lifecycle
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
    assert "requires explicit external-write authority" in runbook
    assert "do not call `gh`" in runbook
    assert "not the relationship" in runbook
    assert "Do not mirror children in a checklist" in runbook
    live_form = read(".github/ISSUE_TEMPLATE/work-item.yml")
    template_form = read("skills/setup-repo/templates/ISSUE_TEMPLATE/work-item.yml")
    assert "URL is navigation only" in live_form
    assert live_form == template_form
    assert contract["source"]["state_graph"] == (
        "docs/specs/harness/lifecycle.md#state-graph"
    )


def test_pre_rename_lifecycle_routing_dataset_is_explicitly_historical():
    dataset = yaml.safe_load(read("evals/lifecycle-routing.yaml"))

    assert dataset["status"] == "historical"
    assert dataset["catalogue_state"] == "pre-autopilot-rename"
    assert "not a current routable skill" in dataset["note"]
