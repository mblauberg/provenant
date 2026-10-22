"""Direct malformed-success matrix for the shared retained-attempt validator."""
from __future__ import annotations

import hashlib
import importlib.util
import json
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "skills/orchestrate/scripts/attempt_evidence.py"


def load_validator():
    spec = importlib.util.spec_from_file_location("attempt_evidence_matrix", SCRIPT)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def valid_evidence(tmp_path: Path):
    result = b"result\n"
    result_path = tmp_path / "result.md"
    result_path.write_bytes(result)
    digest = "sha256:" + hashlib.sha256(result).hexdigest()
    adapter = {
        "tool": "codex", "adapter": "codex", "execution_intent": "ordinary",
        "resolved_model": "fixture-model", "provider_family": "openai",
        "model_family": "fixture-model", "endpoint_provider": "openai",
        "identity_source": "fixture", "output_path": str(result_path),
        "output_digest": digest, "read_only_guarantee": "none", "status": "ok",
        "exit": 0, "cross_family": False, "certification_eligible": False,
    }
    record = {
        "status": "ok", "process": {"observed_exit": True, "exit_code": 0},
        "result": {"path": "result.md", "digest": digest},
        "requested_route": {"adapter": "codex", "intent": "ordinary"},
        "route": {key: adapter[key] for key in ("adapter", "execution_intent", "provider_family", "resolved_model")},
    }
    return record, {"result": result, "adapter_receipt": (json.dumps(adapter) + "\n").encode()}, adapter


@pytest.mark.parametrize(("mutation", "message"), [
    ("missing_process", "successful attempt does not prove process exit"),
    ("unobserved_exit", "successful attempt does not prove process exit"),
    ("false_exit", "successful attempt does not prove exit 0"),
    ("float_exit", "successful attempt does not prove exit 0"),
    ("missing_result", "successful attempt has no result evidence"),
    ("empty_result", "successful attempt result is empty"),
    ("bad_result_digest", "successful attempt result digest is invalid"),
    ("missing_adapter_receipt", "successful attempt has no adapter receipt"),
    ("missing_route", "successful attempt route is invalid"),
    ("missing_adapter_field", "successful adapter receipt is missing route identity"),
    ("missing_assurance_flag", "successful adapter receipt is missing assurance flags"),
])
def test_malformed_success_evidence_is_rejected(tmp_path: Path, mutation: str, message: str) -> None:
    module = load_validator()
    record, payloads, adapter = valid_evidence(tmp_path)
    if mutation == "missing_process":
        record.pop("process")
    elif mutation == "unobserved_exit":
        record["process"]["observed_exit"] = False
    elif mutation == "false_exit":
        record["process"]["exit_code"] = False
    elif mutation == "float_exit":
        record["process"]["exit_code"] = 0.0
    elif mutation == "missing_result":
        record.pop("result")
    elif mutation == "empty_result":
        payloads["result"] = b""
    elif mutation == "bad_result_digest":
        record["result"]["digest"] = "sha256:wrong"
    elif mutation == "missing_adapter_receipt":
        payloads.pop("adapter_receipt")
    elif mutation == "missing_route":
        record.pop("route")
    elif mutation == "missing_adapter_field":
        adapter.pop("provider_family")
        payloads["adapter_receipt"] = (json.dumps(adapter) + "\n").encode()
    elif mutation == "missing_assurance_flag":
        adapter.pop("cross_family")
        payloads["adapter_receipt"] = (json.dumps(adapter) + "\n").encode()
    with pytest.raises(module.AttemptEvidenceError, match=message):
        module.validate_successful_attempt(tmp_path, record, payloads)
