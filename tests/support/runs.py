"""Run directories and dispatch attempt records."""

from __future__ import annotations

import hashlib
import json
import subprocess
from pathlib import Path

INIT = Path(__file__).resolve().parents[2] / "skills/orchestrate/scripts/run_dir_init.sh"


def make_run(root: Path, name: str = "run") -> Path:
    """Initialise ``root/.agent-run/<name>`` with run_dir_init.sh."""
    return Path(
        subprocess.check_output([str(INIT), str(root / ".agent-run" / name)], text=True).strip()
    )


def file_digest(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def write_attempt(
    run: Path,
    task: str = "task-1",
    attempt: int = 1,
    *,
    status: str = "ok",
    result: str | None = "result\n",
    question: dict[str, str] | None = None,
    observed_exit: bool = True,
    delivery_run_id: str | None = None,
) -> Path:
    """Write one retained dispatch attempt and return its ``attempt.json``."""
    run_dir = run
    task_id = task
    attempt_id = f"attempt-{attempt:03d}"
    attempt_dir = run_dir / "dispatch/tasks" / task_id / attempt_id
    attempt_dir.mkdir(parents=True)
    prompt_path = attempt_dir / "prompt.md"
    prompt_path.write_text("prompt\n", encoding="utf-8")
    stderr_path = attempt_dir / "stderr.log"
    stderr_path.write_text("diagnostic\n", encoding="utf-8")
    adapter_path = attempt_dir / "adapter-receipt.json"
    result_path = attempt_dir / "result.md"
    result_ref = None
    if status == "blocked" and question is not None and result in (None, "question\n"):
        result = json.dumps({
            "schema_version": 1, "record_type": "provenant-worker-terminal",
            "classification": "question", "question": question,
        }, separators=(",", ":")) + "\n"
    if result is not None:
        result_path.write_text(result, encoding="utf-8")
        result_ref = {"path": str(result_path.relative_to(run_dir)), "digest": file_digest(result_path)}
    adapter = {}
    if status in {"ok", "succeeded"}:
        adapter = {
            "tool": "codex", "adapter": "codex", "execution_intent": "ordinary",
            "resolved_model": "fixture-model", "provider_family": "fixture-provider",
            "model_family": "fixture-model", "endpoint_provider": "fixture-provider",
            "identity_source": "fixture", "output_path": str(result_path),
            "output_digest": result_ref["digest"] if result_ref is not None else "",
            "read_only_guarantee": "none", "status": "ok", "exit": 0,
            "cross_family": False, "certification_eligible": False,
        }
    adapter_path.write_text(json.dumps(adapter) + "\n", encoding="utf-8")
    record = {
        "schema_version": 1,
        "record_type": "dispatch-attempt",
        "run_id": run_dir.name,
        "task_id": task_id,
        "attempt_id": attempt_id,
        "status": status,
        "outcome": "ok" if status in {"ok", "succeeded"} else status,
        "requested_route": {
            "intent": "ordinary", "adapter": "codex", "alias": "junior",
            "task_class": "", "model": "", "role": "worker", "orchestrator_family": "openai",
            "risk_tier": "substantial", "model_override_tier": "",
            "reviewer_id": "reviewer-1", "effort": "high",
        },
        "route": {
            **({field: adapter[field] for field in ("adapter", "execution_intent", "provider_family", "resolved_model")}
               if status in {"ok", "succeeded"} else {}),
            "adapter_receipt": {"path": str(adapter_path.relative_to(run_dir)), "digest": file_digest(adapter_path)},
        },
        "prompt": {"path": str(prompt_path.relative_to(run_dir)), "digest": file_digest(prompt_path)},
        "stderr": {"path": str(stderr_path.relative_to(run_dir)), "digest": file_digest(stderr_path)},
        "result": result_ref,
        "process": {"observed_exit": observed_exit, "exit_code": 0},
        "started_at": "2026-08-29T00:00:00.000Z",
        "finished_at": "2026-08-29T00:00:01.000Z",
    }
    if question is not None:
        record["question"] = question
    if delivery_run_id is not None:
        record["delivery_run_id"] = delivery_run_id
    attempt_path = attempt_dir / "attempt.json"
    record["attempt_path"] = str(attempt_path.relative_to(run_dir))
    attempt_path.write_text(json.dumps(record, sort_keys=True) + "\n", encoding="utf-8")
    digest_path = attempt_dir / "attempt.sha256"
    digest_path.write_text(f"{file_digest(attempt_path)}  attempt.json\n", encoding="utf-8")
    return attempt_path

