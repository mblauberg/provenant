from pathlib import Path
import json
import os
import shutil
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
VALIDATOR = ROOT / "runtime" / "agent-fabric" / "scripts" / "validate-adapter-executables.ts"
SCHEMA = ROOT / "runtime" / "agent-fabric" / "schemas" / "adapter-compatibility.schema.json"


def run_validator(tmp_path, executable, path, optional_executable=None):
    optional_adapter = "" if optional_executable is None else f"""  agy:
    enabled: true
    delivery_stage: 3
    implementation:
      kind: native-cli
      executable: {json.dumps(optional_executable)}
      provider_identity: apple-designated
      wrapper_entrypoint: fixture-wrapper.js
    contract:
      protocol: example
    runtime_range:
      platforms: [linux-x64]
    model_family_constraints:
      allowed: [example]
      requires_explicit_model: true
    official_source_url: https://example.invalid/example
"""
    compatibility = tmp_path / "adapter-compatibility.yaml"
    compatibility.write_text(f"""schema_version: 1
activation_policy:
  real_adapters_require_separate_gate: true
  default_enabled: false
  executable_resolution_version: 2
adapters:
  claude-agent-sdk:
    enabled: true
    delivery_stage: 3
    implementation:
      kind: native-cli
      executable: {json.dumps(executable)}
      provider_identity: apple-designated
      wrapper_entrypoint: fixture-wrapper.js
    contract:
      protocol: example
    runtime_range:
      platforms: [linux-x64]
    model_family_constraints:
      allowed: [example]
      requires_explicit_model: true
    official_source_url: https://example.invalid/example
{optional_adapter}""")
    environment = os.environ.copy()
    environment["PATH"] = path + os.pathsep + os.environ.get("PATH", "")
    node = shutil.which("node", path=environment["PATH"])
    assert node is not None
    return subprocess.run(
        [node, "--import", "tsx", str(VALIDATOR),
         "--compatibility", str(compatibility), "--schema", str(SCHEMA)],
        cwd=ROOT,
        env=environment,
        text=True,
        capture_output=True,
        check=False,
    )


def test_install_time_check_rejects_enabled_adapter_with_unresolvable_executable(tmp_path):
    result = run_validator(tmp_path, "missing-example", str(tmp_path / "empty-bin"))
    assert result.returncode != 0
    assert "adapter claude-agent-sdk is enabled but executable 'missing-example' is not resolvable on PATH" in result.stderr


def test_install_time_check_rejects_enabled_optional_adapter_without_filter(tmp_path):
    result = run_validator(
        tmp_path,
        sys.executable,
        str(tmp_path / "empty-bin"),
        optional_executable="missing-optional",
    )
    assert result.returncode != 0
    assert "adapter agy is enabled but executable 'missing-optional' is not resolvable on PATH" in result.stderr


def test_install_time_check_accepts_enabled_adapter_with_resolvable_executable(tmp_path):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    executable = bin_dir / "example"
    executable.write_text("#!/bin/sh\nexit 0\n")
    executable.chmod(0o700)

    result = run_validator(tmp_path, "example", str(bin_dir))
    assert result.returncode == 0, result.stderr
