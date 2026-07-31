import os
import shutil
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
NODE = shutil.which("node") or "/usr/bin/env node"


def _copy(source: Path, target: Path) -> None:
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, target)


def _fixture(tmp_path: Path) -> tuple[Path, tuple[Path, Path]]:
    root = tmp_path / "agents"
    _copy(
        ROOT / "runtime/agent-fabric-protocol/bin/protocol-build-preflight.js",
        root / "runtime/agent-fabric-protocol/bin/protocol-build-preflight.js",
    )
    _copy(
        ROOT / "runtime/agent-fabric-herdr/bin/agent-fabric-herdr.js",
        root / "runtime/agent-fabric-herdr/bin/agent-fabric-herdr.js",
    )
    _copy(
        ROOT / "runtime/agent-fabric-console/bin/agent-fabric-console.js",
        root / "runtime/agent-fabric-console/bin/agent-fabric-console.js",
    )
    _copy(
        ROOT / "runtime/agent-fabric/scripts/verify-npm-ci-attestation.mjs",
        root / "runtime/agent-fabric/scripts/verify-npm-ci-attestation.mjs",
    )
    _copy(
        ROOT / "runtime/agent-fabric/scripts/write-npm-ci-attestation.mjs",
        root / "runtime/agent-fabric/scripts/write-npm-ci-attestation.mjs",
    )
    _copy(
        ROOT / "runtime/agent-fabric/scripts/lib/npm-install-attestation.mjs",
        root / "runtime/agent-fabric/scripts/lib/npm-install-attestation.mjs",
    )
    preflight = root / "scripts/agent-fabric-protocol-preflight"
    preflight.parent.mkdir(parents=True, exist_ok=True)
    preflight.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    preflight.chmod(0o755)
    (root / "node_modules/.keep").parent.mkdir(parents=True, exist_ok=True)
    (root / "node_modules/.keep").write_text("fixture\n", encoding="utf-8")
    (root / "runtime/agent-fabric/package.json").write_text("{}\n", encoding="utf-8")
    (root / "package.json").write_text('{"type":"module"}\n', encoding="utf-8")
    (root / "package-lock.json").write_text(
        '{"lockfileVersion":3,"packages":{}}\n', encoding="utf-8"
    )
    for name, output in (("herdr", "herdr"), ("console", "console")):
        dist = root / f"runtime/agent-fabric-{name}/dist/bin.js"
        dist.parent.mkdir(parents=True, exist_ok=True)
        dist.write_text(f'process.stdout.write("{output} dist\\n");\n', encoding="utf-8")

    subprocess.run(["git", "init", "--quiet"], cwd=root, check=True)
    subprocess.run(["git", "config", "user.name", "Entrypoint Test"], cwd=root, check=True)
    subprocess.run(
        ["git", "config", "user.email", "entrypoint-test@example.invalid"],
        cwd=root,
        check=True,
    )
    subprocess.run(["git", "add", "."], cwd=root, check=True)
    subprocess.run(["git", "commit", "--quiet", "-m", "fixture"], cwd=root, check=True)
    subprocess.run(
        [
            NODE,
            str(root / "runtime/agent-fabric/scripts/write-npm-ci-attestation.mjs"),
            str(root),
        ],
        cwd=root,
        check=True,
        capture_output=True,
        text=True,
    )
    return root, (
        root / "runtime/agent-fabric-herdr/bin/agent-fabric-herdr.js",
        root / "runtime/agent-fabric-console/bin/agent-fabric-console.js",
    )


def test_herdr_and_console_gate_compiled_dist_before_import(tmp_path: Path) -> None:
    root, wrappers = _fixture(tmp_path)
    environment = {**os.environ, "AGENTS_HOME": str(root)}

    for wrapper in wrappers:
        result = subprocess.run(
            [NODE, str(wrapper)], cwd=root, env=environment, capture_output=True, text=True, check=False
        )
        assert result.returncode == 0, result.stderr
        assert result.stdout == f"{wrapper.parts[-3].removeprefix('agent-fabric-')} dist\n"

    (root / "node_modules/tampered.js").write_text("tampered\n", encoding="utf-8")
    for wrapper in wrappers:
        result = subprocess.run(
            [NODE, str(wrapper)], cwd=root, env=environment, capture_output=True, text=True, check=False
        )
        assert result.returncode == 1
        assert "NPM_INSTALL_ATTESTATION_MISMATCH" in result.stderr
        assert result.stdout == ""
