from pathlib import Path
import json
import os
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
VALIDATOR = ROOT / "runtime" / "agent-fabric" / "scripts" / "validate-adapter-executables.ts"
SCHEMA = ROOT / "runtime" / "agent-fabric" / "schemas" / "adapter-compatibility.schema.json"


def run_validator(tmp_path, executable, path):
    compatibility = tmp_path / "adapter-compatibility.yaml"
    compatibility.write_text(f"""schema_version: 1
activation_policy:
  real_adapters_require_separate_gate: true
  default_enabled: false
adapters:
  example:
    enabled: true
    delivery_stage: 3
    implementation:
      kind: native-cli
      executable: {json.dumps(executable)}
    contract:
      protocol: example
    runtime_range:
      platforms: [linux-x64]
    model_family_constraints:
      allowed: [example]
      requires_explicit_model: true
    official_source_url: https://example.invalid/example
""")
    environment = os.environ.copy()
    environment["PATH"] = path + os.pathsep + "/opt/homebrew/opt/node@24/bin" + os.pathsep + os.environ.get("PATH", "")
    return subprocess.run(
        ["/opt/homebrew/opt/node@24/bin/node", "--import", "tsx", str(VALIDATOR),
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
    assert "adapter example is enabled but executable 'missing-example' is not resolvable on PATH" in result.stderr


def test_install_time_check_accepts_enabled_adapter_with_resolvable_executable(tmp_path):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    executable = bin_dir / "example"
    executable.write_text("#!/bin/sh\nexit 0\n")
    executable.chmod(0o700)

    result = run_validator(tmp_path, "example", str(bin_dir))
    assert result.returncode == 0, result.stderr
