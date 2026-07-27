from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess

import pytest


REPO_ROOT = Path(__file__).resolve().parents[1]
WARM_SCRIPT = REPO_ROOT / "scripts" / "agent-fabric-warm"
FRESHNESS_LIBRARY = REPO_ROOT / "scripts" / "lib" / "agent-fabric-workspace-freshness.sh"
BUILD_LOCK_LIBRARY = REPO_ROOT / "scripts" / "lib" / "agent-fabric-protocol-build-lock.sh"


def _write(path: Path, content: str = "fixture\n") -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def _fixture(tmp_path: Path) -> tuple[Path, Path, Path]:
    root = tmp_path / "agents"
    script = root / "scripts" / "agent-fabric-warm"
    script.parent.mkdir(parents=True)
    shutil.copy2(WARM_SCRIPT, script)
    script.chmod(0o755)
    if FRESHNESS_LIBRARY.exists():
        library_dir = root / "scripts/lib"
        library_dir.mkdir(parents=True)
        shutil.copy2(
            FRESHNESS_LIBRARY,
            library_dir / FRESHNESS_LIBRARY.name,
        )
        shutil.copy2(
            BUILD_LOCK_LIBRARY,
            library_dir / BUILD_LOCK_LIBRARY.name,
        )

    # The wrapper treats node_modules as the installation readiness gate.
    (root / "node_modules").mkdir()
    protocol_source = root / "runtime/agent-fabric-protocol/src/index.ts"
    fabric_output = root / "runtime/agent-fabric/dist/cli/main.js"
    _write(protocol_source)
    _write(root / "runtime/agent-fabric-protocol/dist/index.js")
    _write(fabric_output)
    _write(root / "runtime/agent-fabric/dist/mcp/main.js")
    _write(root / "runtime/agent-fabric-herdr/dist/bin.js")
    _write(root / "runtime/agent-fabric-console/dist/bin.js")
    for manifest, content in {
        "package.json": '{"type":"module"}\n',
        "package-lock.json": '{"lockfileVersion":3}\n',
        "tsconfig.json": '{"files":[]}\n',
    }.items():
        _write(root / manifest, content)
        os.utime(root / manifest, (1_700_000_000, 1_700_000_000))

    bin_dir = tmp_path / "bin"
    marker = tmp_path / "npm-invocations"
    _write(
        bin_dir / "npm",
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$WARM_TEST_MARKER\"\n",
    )
    (bin_dir / "npm").chmod(0o755)
    return root, protocol_source, marker


def _run(root: Path, marker: Path, *, now: int) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env["AGENTS_HOME"] = str(root)
    env["PATH"] = f"{marker.parent / 'bin'}:{env['PATH']}"
    env["WARM_TEST_MARKER"] = str(marker)
    # Keep mtimes deterministic instead of depending on filesystem clock
    # resolution in a fast test run.
    for output in (root / "runtime").glob("*/dist/**/*.js"):
        os.utime(output, (now, now))
    return subprocess.run(
        [str(root / "scripts/agent-fabric-warm")],
        cwd=root,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )


def test_protocol_only_staleness_rebuilds_then_fresh_workspace_is_noop(
    tmp_path: Path,
) -> None:
    root, protocol_source, marker = _fixture(tmp_path)
    now = 1_700_000_000
    os.utime(protocol_source, (now + 10, now + 10))

    stale = _run(root, marker, now=now)

    assert stale.returncode == 0, stale.stderr
    assert "agent-fabric dist stale; rebuilding workspace" in stale.stdout
    assert marker.read_text(encoding="utf-8").splitlines() == ["run build"]

    # A successful warm build advances all workspace freshness sentinels. The
    # exact same inputs must then take the fast no-op path.
    fresh = _run(root, marker, now=now + 20)

    assert fresh.returncode == 0, fresh.stderr
    assert "agent-fabric dist fresh:" in fresh.stdout
    assert marker.read_text(encoding="utf-8").splitlines() == ["run build"]


def test_manifest_digest_change_rebuilds_once_and_warm_records_the_new_digest(
    tmp_path: Path,
) -> None:
    root, protocol_source, marker = _fixture(tmp_path)
    now = 1_700_000_000
    os.utime(protocol_source, (now - 20, now - 20))
    for relative, content in {
        "package.json": '{"type":"module"}\n',
        "package-lock.json": '{"lockfileVersion":3}\n',
        "tsconfig.json": '{"files":[]}\n',
    }.items():
        path = root / relative
        _write(path, content)
        os.utime(path, (now - 20, now - 20))

    digest = subprocess.run(
        [
            "/bin/sh",
            "-c",
            '. "$1"; workspace_root_manifest_digest "$2"',
            "digest-fixture",
            str(root / "scripts/lib/agent-fabric-workspace-freshness.sh"),
            str(root),
        ],
        text=True,
        capture_output=True,
        check=True,
    ).stdout
    stamp = root / "runtime/agent-fabric-protocol/dist/.root-manifests.sha256"
    _write(stamp, digest)

    _write(root / "package-lock.json", '{"lockfileVersion":3,"changed":true}\n')
    os.utime(root / "package-lock.json", (now - 10, now - 10))

    rebuilt = _run(root, marker, now=now)
    fresh = _run(root, marker, now=now + 20)

    assert rebuilt.returncode == 0, rebuilt.stderr
    assert "agent-fabric dist stale; rebuilding workspace" in rebuilt.stdout
    assert fresh.returncode == 0, fresh.stderr
    assert "agent-fabric dist fresh:" in fresh.stdout
    assert marker.read_text(encoding="utf-8").splitlines() == ["run build"], (
        "warm must replace the protocol manifest stamp after its successful root build"
    )


@pytest.mark.parametrize(
    "relative_input",
    [
        "runtime/agent-fabric-protocol/package.json",
        "runtime/agent-fabric-protocol/tsconfig.json",
        "runtime/agent-fabric-protocol/tsconfig.build.json",
        "runtime/agent-fabric-protocol/scripts/write-schema.mjs",
        "package.json",
        "package-lock.json",
        "tsconfig.json",
    ],
)
def test_shared_predicate_keeps_every_protocol_build_input(
    tmp_path: Path,
    relative_input: str,
) -> None:
    root, _, marker = _fixture(tmp_path)
    now = 1_700_000_000
    build_input = root / relative_input
    _write(build_input)
    os.utime(build_input, (now + 10, now + 10))

    result = _run(root, marker, now=now)

    assert result.returncode == 0, result.stderr
    assert "agent-fabric dist stale; rebuilding workspace" in result.stdout
    assert marker.read_text(encoding="utf-8").splitlines() == ["run build"]


def test_schema_generator_rule_does_not_broaden_to_other_workspaces(
    tmp_path: Path,
) -> None:
    root, protocol_source, marker = _fixture(tmp_path)
    now = 1_700_000_000
    os.utime(protocol_source, (now - 10, now - 10))
    unrelated_script = root / "runtime/agent-fabric/scripts/unrelated.mjs"
    _write(unrelated_script)
    os.utime(unrelated_script, (now + 10, now + 10))

    result = _run(root, marker, now=now)

    assert result.returncode == 0, result.stderr
    assert "agent-fabric dist fresh:" in result.stdout
    assert not marker.exists()


def test_freshness_predicate_has_one_owner_and_two_callers() -> None:
    library = FRESHNESS_LIBRARY.read_text(encoding="utf-8")
    launcher = (REPO_ROOT / "scripts" / "agent-fabric").read_text(encoding="utf-8")
    preflight = (REPO_ROOT / "scripts" / "agent-fabric-protocol-preflight").read_text(
        encoding="utf-8",
    )
    warm = WARM_SCRIPT.read_text(encoding="utf-8")

    assert library.count("workspace_is_stale() {") == 1
    assert "workspace_is_stale() {" not in launcher
    assert "workspace_is_stale() {" not in preflight
    assert "workspace_is_stale() {" not in warm
    assert '"$script_dir/agent-fabric-protocol-preflight"' in launcher
    assert 'lib/agent-fabric-workspace-freshness.sh"' in preflight
    assert 'lib/agent-fabric-workspace-freshness.sh"' in warm
