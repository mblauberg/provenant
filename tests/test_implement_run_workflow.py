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
