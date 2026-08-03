from pathlib import Path
import json
import os
import shutil
import subprocess


ROOT = Path(__file__).resolve().parents[1]
VALIDATOR = ROOT / "runtime" / "agent-fabric" / "scripts" / "validate-adapter-executables.ts"
SCHEMA = ROOT / "runtime" / "agent-fabric" / "schemas" / "adapter-compatibility.schema.json"
NODE = shutil.which("node") or "node"


def run_validator(tmp_path, adapter_id, executable, path, schema=SCHEMA):
    path_dir = Path(path)
    path_dir.mkdir(parents=True, exist_ok=True)
    primary_executables = {
        "claude-agent-sdk": "missing-claude" if adapter_id == "claude-agent-sdk" else "seeded-claude",
        "codex-app-server": "missing-codex" if adapter_id == "codex-app-server" else "seeded-codex",
    }
    for name in primary_executables.values():
        if name.startswith("missing-"):
            continue
        executable_path = path_dir / name
        executable_path.write_text("#!/bin/sh\nexit 0\n")
        executable_path.chmod(0o700)
    entries = {**primary_executables, adapter_id: executable}
    adapter_blocks = []
    for current_id, current_executable in entries.items():
        stage = 4 if current_id not in {"claude-agent-sdk", "codex-app-server"} else 3
        adapter_blocks.append(f"""  {current_id}:
    enabled: true
    delivery_stage: {stage}
    implementation:
      kind: native-cli
      executable: {json.dumps(current_executable)}
      wrapper_entrypoint: runtime/agent-fabric/src/adapters/providers/optional/example.ts
      provider_identity: apple-designated
    contract:
      protocol: example
    runtime_range:
      platforms: [linux-x64]
    model_family_constraints:
      allowed: [example]
      requires_explicit_model: true
    official_source_url: https://example.invalid/example
""")
    compatibility = tmp_path / "adapter-compatibility.yaml"
    compatibility.write_text(f"""schema_version: 1
activation_policy:
  real_adapters_require_separate_gate: true
  default_enabled: false
  executable_resolution_version: 2
adapters:
{"".join(adapter_blocks)}""")
    environment = os.environ.copy()
    environment["PATH"] = str(path_dir) + os.pathsep + os.environ.get("PATH", "")
    return subprocess.run(
        [NODE, "--import", "tsx", str(VALIDATOR),
         "--compatibility", str(compatibility), "--schema", str(schema)],
        cwd=ROOT,
        env=environment,
        text=True,
        capture_output=True,
        check=False,
    )


def test_install_time_check_rejects_enabled_primary_with_unresolvable_executable(tmp_path):
    result = run_validator(tmp_path, "claude-agent-sdk", "missing-claude", str(tmp_path / "empty-bin"))
    assert result.returncode != 0
    assert "adapter claude-agent-sdk is enabled but executable 'missing-claude' is not resolvable on PATH" in result.stderr


def test_install_time_check_accepts_enabled_adapter_with_resolvable_executable(tmp_path):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    executable = bin_dir / "example"
    executable.write_text("#!/bin/sh\nexit 0\n")
    executable.chmod(0o700)

    result = run_validator(tmp_path, "agy", "example", str(bin_dir))
    assert result.returncode == 0, result.stderr


def test_install_time_check_reports_unresolvable_optional_without_failing(tmp_path):
    result = run_validator(tmp_path, "agy", "missing-agy", str(tmp_path / "empty-bin"))
    assert result.returncode == 0, result.stderr
    assert "optional adapter agy is unavailable" in result.stdout


def test_install_time_check_uses_the_published_schema_argument(tmp_path):
    schema = tmp_path / "reject-everything.json"
    schema.write_text(json.dumps({"type": "string"}))

    result = run_validator(tmp_path, "agy", "missing-agy", str(tmp_path / "empty-bin"), schema)

    assert result.returncode != 0
    assert "does not match published schema" in result.stderr
