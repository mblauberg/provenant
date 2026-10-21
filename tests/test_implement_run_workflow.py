"""Executable guards for the dynamic implement workflow's entry boundaries."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = ROOT / "workflows" / "implement-run.js"

HARNESS = r"""
;(async () => {
  const fs = require('node:fs')
  const [workflowPath, argsJson, bootJson] = process.argv.slice(1)
  const source = fs.readFileSync(workflowPath, 'utf8').replace('export const meta', 'const meta')
  const args = JSON.parse(argsJson)
  const bootstrap = JSON.parse(bootJson)
  const calls = []
  const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor
  const run = new AsyncFunction('args', 'agent', 'parallel', 'log', 'phase', source)
  const agent = async (_prompt, options) => {
    calls.push(options.label)
    if (options.label === 'bootstrap') return bootstrap
    if (options.label.startsWith('checkpoint:understand-dispatch')) throw new Error('STOP_AFTER_POST_BOOT')
    throw new Error(`unexpected workflow phase: ${options.label}`)
  }
  const parallel = async (promises) => Promise.all(promises)
  try {
    const result = await run(args, agent, parallel, () => {}, () => {})
    process.stdout.write(JSON.stringify({calls, result}) + '\n')
  } catch (error) {
    if (error.message !== 'STOP_AFTER_POST_BOOT') throw error
    process.stdout.write(JSON.stringify({calls, stoppedAfterPostBoot: true}) + '\n')
  }
})().catch((error) => {
  console.error(error.stack || error)
  process.exitCode = 1
})
"""


def run_workflow(*, risk: str, effective_risk: str) -> dict[str, object]:
    args = {
        "task": "repair a bounded defect",
        "runId": "risk-floor-test",
        "risk": risk,
        "specApproved": True,
        "designStatus": "approved",
        "acceptanceCriteria": ["the receipt remains valid"],
    }
    bootstrap = {
        "runDir": "/tmp/.agent-run/risk-floor-test",
        "repoRoot": "/tmp/project",
        "gitCwd": "/tmp/project",
        "baseRevision": "a" * 40,
        "effectiveRisk": effective_risk,
        "riskPreflightPassed": True,
        "requiresUserAcceptance": False,
        "conventions": {},
        "modelRoutes": {},
    }
    completed = subprocess.run(
        ["node", "-e", HARNESS, str(WORKFLOW), json.dumps(args), json.dumps(bootstrap)],
        text=True,
        capture_output=True,
        check=False,
    )
    assert completed.returncode == 0, completed.stderr
    return json.loads(completed.stdout)


def test_terminal_request_stops_when_bootstrap_receipt_reports_lower_risk():
    result = run_workflow(risk="terminal", effective_risk="substantial")

    assert result["calls"] == ["bootstrap"]


def test_bootstrap_may_raise_the_requested_risk_floor():
    result = run_workflow(risk="substantial", effective_risk="crucial")

    assert result["calls"][0] == "bootstrap"
    assert "checkpoint:understand-dispatch" in result["calls"]
    assert result["stoppedAfterPostBoot"] is True


def test_bootstrap_anchors_delivery_run_to_primary_checkout():
    source = WORKFLOW.read_text()
    assert "git rev-parse --path-format=absolute --git-common-dir" in source
    assert "primary checkout" in source
    assert "approved intent" in source and "primary checkout" in source


COMPLETE_HARNESS = r"""
;(async () => {
  const fs = require('node:fs')
  const [path, argsJson, repliesJson] = process.argv.slice(1)
  const source = fs.readFileSync(path, 'utf8').replace('export const meta', 'const meta')
  const replies = JSON.parse(repliesJson)
  const calls = []
  const agent = async (_prompt, options) => {
    const label = options.label
    calls.push(label)
    const reply = Object.hasOwn(replies, label) ? replies[label] : replies[label.split(':')[0]]
    if (reply === undefined) throw new Error(`missing reply: ${label}`)
    if (reply === null) return null
    return {...reply, ...(label.startsWith('review:0:') ? {angle: label.split(':')[2]} : {})}
  }
  const parallel = async (items) => Promise.all(items.map(item => typeof item === 'function' ? item() : item))
  const pipeline = async (items, stage) => Promise.all(items.map(stage))
  const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor
  const run = new AsyncFunction('args', 'agent', 'parallel', 'pipeline', 'log', 'phase', source)
  const result = await run(JSON.parse(argsJson), agent, parallel, pipeline, () => {}, () => {})
  process.stdout.write(JSON.stringify({calls, result}) + '\n')
})().catch(error => { console.error(error.stack); process.exitCode = 1 })
"""


def execute_complete_workflow(path, args, replies):
    completed = subprocess.run(
        ["node", "-e", COMPLETE_HARNESS, str(path), json.dumps(args), json.dumps(replies)],
        text=True, capture_output=True, check=False,
    )
    assert completed.returncode == 0, completed.stderr
    return json.loads(completed.stdout).get("result")


def implementation_result(*, risk="substantial", requires_user=False, machine_pass=True,
                          escalated=False, checks_pass=True, blocked=False, substituted=False, metadata=True):
    args = {"task": "bounded repair", "runId": "acceptance-test", "risk": risk,
            "specApproved": True, "designStatus": "approved", "acceptanceCriteria": ["fixed"],
            "requiresUserAcceptance": requires_user}
    patch = {"patchPath": "fix.diff", "targetFiles": ["app.py"], "riskTier": "low"}
    replies = {
        "bootstrap": {"runDir": "/tmp/run", "repoRoot": "/tmp/project", "gitCwd": "/tmp/project",
                      "baseRevision": "a" * 40, "effectiveRisk": risk, "riskPreflightPassed": True,
                      "requiresUserAcceptance": requires_user,
                      "conventions": {}, "modelRoutes": {}},
        "checkpoint": {"verified": True}, "understand": {}, "plan": {},
        "plan:adjudicate": {"chosenFraming": "minimal"},
        "implement": {"patches": [patch]}, "repair": {"patches": [patch]},
        "review": {"verdict": "block" if blocked else "approve", "issues": [],
                   "crossFamily": {"ran": True, "status": "ok", "crossFamily": True,
                                   "certificationEligible": True, "readOnlyGuarantee": "enforced"}},
        "review:council-challenge": {"anonymized": True, "randomized": True},
        "review:council-reduction": {"freshContext": True, "unresolvedDissent": []},
        "verify": {"passed": checks_pass},
        "apply:serial": {"applied": [] if escalated else [patch],
                         "escalated": [patch] if escalated else [], "machineGatePassed": machine_pass,
                         "acceptanceBasis": "" if requires_user or risk == "terminal" or escalated else "checks-and-review"},
        "run:fail": {"verified": True},
    }
    if not metadata:
        del replies["bootstrap"]["requiresUserAcceptance"]
    if substituted:
        replies["apply:serial"]["applied"] = [{"patchPath": "unreviewed.diff", "targetFiles": ["other.py"]}]
    return execute_complete_workflow(WORKFLOW, args, replies)


def test_ordinary_implementation_completes_after_checks_review_and_recorded_acceptance():
    result = implementation_result()
    assert result["state"] == "complete"
    assert result["acceptanceBasis"] == "checks-and-review"


def test_implementation_preserves_human_and_machine_gates():
    assert implementation_result(requires_user=True)["state"] == "awaiting-human"
    assert implementation_result(risk="terminal", escalated=True, machine_pass=False)["state"] == "awaiting-apply-approval"
    assert implementation_result(machine_pass=False)["state"] == "failed"
    assert implementation_result(escalated=True)["state"] == "awaiting-human"
    assert implementation_result(checks_pass=False)["state"] != "complete"
    assert implementation_result(blocked=True)["state"] != "complete"


def polish_result(*, escalated=False, requires_user=False, risk="substantial", reverted=False,
                  extra_applied=False, metadata=True, unreviewed=False):
    candidate = {"id": "fix", "file": "app.py", "locator": "line:1", "kind": "simplify"}
    replies = {
        "bootstrap+recon": {"runDir": "/tmp/polish", "modelRoutes": {}, "sliceRoot": "src",
                            "effectiveRisk": risk, "requiresUserAcceptance": requires_user},
        "scan": {"candidates": [candidate]},
        "review": {"verdict": "apply", "risk": "low", "patchPath": "fix.diff", "objectiveChecks": ["test"]},
        "cf-review": {"crossFamilyRan": False},
        "adjudicate": {"decisions": [{"id": "fix", "decision": "escalate" if escalated else "auto-apply",
                                       "patchPath": "fix.diff", "objectiveChecks": ["test"]}]},
        "serial-applier": {"applied": [] if escalated or reverted else ["fix"],
                           "reverted": ["fix"] if reverted else [], "escalated": ["fix"] if escalated else [],
                           "redAmberGreen": "green"},
    }
    if unreviewed:
        replies["review"] = None
        replies["cf-review"] = None
        replies["adjudicate"]["decisions"] = []
        replies["serial-applier"]["applied"] = []
    if extra_applied:
        replies["serial-applier"]["applied"].append("unreviewed")
    if not metadata:
        del replies["bootstrap+recon"]["effectiveRisk"]
        del replies["bootstrap+recon"]["requiresUserAcceptance"]
    return execute_complete_workflow(ROOT / "workflows/codebase-polish.js",
                                     {"path": "src", "requiresUserAcceptance": requires_user, "risk": risk}, replies)


def test_polish_completes_applied_work_and_preserves_remaining_decisions():
    assert polish_result()["status"] == "complete"
    assert polish_result(escalated=True)["status"] == "awaiting-human"
    assert polish_result(requires_user=True)["status"] == "awaiting-human"
    assert polish_result(risk="terminal")["status"] == "awaiting-human"
    assert polish_result(reverted=True)["status"] != "complete"


def test_completion_requires_applied_patch_identity():
    assert implementation_result(substituted=True)["state"] != "complete"


def test_polish_completion_requires_applied_patch_identity():
    assert polish_result(extra_applied=True)["status"] != "complete"


def test_polish_completion_requires_verified_scope():
    assert polish_result(metadata=False)["status"] != "complete"


def test_implementation_completion_requires_verified_scope():
    assert implementation_result(metadata=False) is None


def test_polish_cannot_complete_when_scanned_candidates_have_no_review():
    result = polish_result(unreviewed=True)
    assert result["status"] != "complete"
    assert "fix" in result["escalated"]
