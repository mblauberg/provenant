from pathlib import Path
import json
import os
import shutil
import subprocess


ROOT = Path(__file__).resolve().parents[1]


def test_documented_fresh_checkout_sequence_produces_runnable_doctor(tmp_path):
    readme = (ROOT / "README.md").read_text()
    commands = [
        'scripts/install-agent-fabric-dependencies',
        'scripts/agent-fabric-warm',
        'scripts/install-harness --platform claude',
        'provenant doctor',
    ]
    positions = [readme.index(command) for command in commands]
    assert positions == sorted(positions)

    checkout = tmp_path / "fresh-checkout"
    scripts = checkout / "scripts"
    scripts.mkdir(parents=True)
    for name in (
        "agent-fabric",
        "agent-fabric-mcp",
        "agent-fabric-protocol-build",
        "agent-fabric-protocol-preflight",
        "agent-fabric-warm",
        "install-agent-fabric-dependencies",
        "provenant",
    ):
        shutil.copy2(ROOT / "scripts" / name, scripts / name)
    # Copy the whole library directory. Enumerating its members here meant that
    # adding a scripts/lib file broke this fixture rather than exercising the
    # bootstrap it is meant to test.
    shutil.copytree(ROOT / "scripts" / "lib", scripts / "lib")

    for workspace in (
        "agent-fabric-protocol",
        "agent-fabric",
        "agent-fabric-herdr",
        "agent-fabric-console",
    ):
        (checkout / "runtime" / workspace).mkdir(parents=True)
    (checkout / "runtime" / "agent-fabric" / "scripts").mkdir(parents=True)
    (checkout / "runtime" / "agent-fabric" / "scripts" / "lib").mkdir(parents=True)
    shutil.copy2(
        ROOT / "runtime" / "agent-fabric" / "scripts" / "write-npm-ci-attestation.mjs",
        checkout / "runtime" / "agent-fabric" / "scripts" / "write-npm-ci-attestation.mjs",
    )
    shutil.copy2(
        ROOT / "runtime" / "agent-fabric" / "scripts" / "verify-npm-ci-attestation.mjs",
        checkout / "runtime" / "agent-fabric" / "scripts" / "verify-npm-ci-attestation.mjs",
    )
    shutil.copy2(
        ROOT / "runtime" / "agent-fabric" / "scripts" / "lib" / "npm-install-attestation.mjs",
        checkout / "runtime" / "agent-fabric" / "scripts" / "lib" / "npm-install-attestation.mjs",
    )
    (checkout / "package.json").write_text("{}\n")
    (checkout / "package-lock.json").write_text(
        json.dumps({"name": "fixture", "lockfileVersion": 3, "packages": {}}) + "\n"
    )
    (checkout / "tsconfig.json").write_text("{}\n")
    subprocess.run(["git", "init", "-q"], cwd=checkout, check=True)
    subprocess.run(["git", "add", "."], cwd=checkout, check=True)
    subprocess.run(
        [
            "git",
            "-c",
            "user.name=Quickstart Test",
            "-c",
            "user.email=quickstart@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ],
        cwd=checkout,
        check=True,
    )

    fake_bin = tmp_path / "fake-bin"
    fake_bin.mkdir()
    fake_npm = fake_bin / "npm"
    fake_npm.write_text(
        "#!/bin/sh\n"
        "set -eu\n"
        "if [ \"${1:-}\" = ci ]; then mkdir -p \"$AGENTS_HOME/node_modules\"; exit 0; fi\n"
        "[ \"${1:-} ${2:-}\" = 'run build' ]\n"
        "mkdir -p \"$AGENTS_HOME/runtime/agent-fabric-protocol/dist\"\n"
        "mkdir -p \"$AGENTS_HOME/runtime/agent-fabric/dist/cli\"\n"
        "mkdir -p \"$AGENTS_HOME/runtime/agent-fabric/dist/mcp\"\n"
        "mkdir -p \"$AGENTS_HOME/runtime/agent-fabric-herdr/dist\"\n"
        "mkdir -p \"$AGENTS_HOME/runtime/agent-fabric-console/dist\"\n"
        "printf '%s\\n' 'export {};' > \"$AGENTS_HOME/runtime/agent-fabric-protocol/dist/index.js\"\n"
        "printf '%s\\n' 'process.stdout.write(JSON.stringify({argv: process.argv.slice(2), cwd: process.cwd()}));' > \"$AGENTS_HOME/runtime/agent-fabric/dist/cli/main.js\"\n"
        "printf '%s\\n' 'export {};' > \"$AGENTS_HOME/runtime/agent-fabric/dist/mcp/main.js\"\n"
        "printf '%s\\n' 'export {};' > \"$AGENTS_HOME/runtime/agent-fabric-herdr/dist/bin.js\"\n"
        "printf '%s\\n' 'export {};' > \"$AGENTS_HOME/runtime/agent-fabric-console/dist/bin.js\"\n"
    )
    fake_npm.chmod(0o755)
    env = {
        **os.environ,
        "AGENTS_HOME": str(checkout),
        "PATH": f"{fake_bin}:{os.environ['PATH']}",
    }

    installed_bin = tmp_path / "installed-bin"
    installed_bin.mkdir()
    (installed_bin / "provenant").symlink_to(scripts / "provenant")
    subprocess.run([str(scripts / "install-agent-fabric-dependencies")], cwd=checkout, env=env, check=True)
    warmed = subprocess.run(
        [str(scripts / "agent-fabric-warm")], cwd=checkout, env=env, text=True, capture_output=True, check=False
    )
    caller_cwd = tmp_path / "project"
    caller_cwd.mkdir()
    doctor = subprocess.run(
        [str(installed_bin / "provenant"), "doctor"],
        cwd=caller_cwd,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )

    assert warmed.returncode == 0, warmed.stderr
    assert "rebuilding workspace" in warmed.stdout
    assert doctor.returncode == 0, doctor.stderr
    assert json.loads(doctor.stdout) == {"argv": ["doctor"], "cwd": str(caller_cwd)}
