import os
import shutil
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def copy_attestation_scripts(agents_home: Path) -> None:
    scripts = agents_home / "runtime" / "agent-fabric" / "scripts"
    scripts.mkdir(parents=True)
    (scripts / "lib").mkdir()
    shutil.copy2(
        ROOT / "runtime/agent-fabric/scripts/verify-npm-ci-attestation.mjs",
        scripts / "verify-npm-ci-attestation.mjs",
    )
    shutil.copy2(
        ROOT / "runtime/agent-fabric/scripts/write-npm-ci-attestation.mjs",
        scripts / "write-npm-ci-attestation.mjs",
    )
    shutil.copy2(
        ROOT / "runtime/agent-fabric/scripts/lib/npm-install-attestation.mjs",
        scripts / "lib/npm-install-attestation.mjs",
    )


def attest_fixture(agents_home: Path) -> None:
    package_json = agents_home / "package.json"
    package_lock = agents_home / "package-lock.json"
    package_json.write_text('{"type":"module"}\n', encoding="utf-8")
    package_lock.write_text(
        '{"lockfileVersion":3,"packages":{}}\n', encoding="utf-8"
    )
    os.utime(package_json, ns=(1_000_000_000, 1_000_000_000))
    os.utime(package_lock, ns=(1_000_000_000, 1_000_000_000))
    subprocess.run(["git", "init", "--quiet"], cwd=agents_home, check=True)
    subprocess.run(
        ["git", "config", "user.name", "Agent Fabric MCP Fixture"],
        cwd=agents_home,
        check=True,
    )
    subprocess.run(
        ["git", "config", "user.email", "agent-fabric-mcp-fixture@example.invalid"],
        cwd=agents_home,
        check=True,
    )
    subprocess.run(["git", "add", "."], cwd=agents_home, check=True)
    subprocess.run(["git", "commit", "--quiet", "-m", "fixture"], cwd=agents_home, check=True)
    subprocess.run(
        ["node", str(agents_home / "runtime/agent-fabric/scripts/write-npm-ci-attestation.mjs"), str(agents_home)],
        cwd=agents_home,
        check=True,
        capture_output=True,
        text=True,
    )


def copy_runtime_fixture(agents_home: Path) -> None:
    copy_attestation_scripts(agents_home)
    scripts = agents_home / "scripts"
    scripts.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT / "scripts/agent-fabric-mcp", scripts / "agent-fabric-mcp")
    (scripts / "agent-fabric-mcp").chmod(0o755)
    preflight = scripts / "agent-fabric-protocol-preflight"
    preflight.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    preflight.chmod(0o755)
    dist = agents_home / "runtime/agent-fabric/dist/mcp/main.js"
    dist.parent.mkdir(parents=True, exist_ok=True)
    dist.write_text(
        "process.stderr.write(`not provisioned for ${process.cwd()}\\n`);\n"
        "process.stderr.write('exact-root bootstrap is available\\n');\n",
        encoding="utf-8",
    )
    source = agents_home / "runtime/agent-fabric/src/mcp/main.ts"
    source.parent.mkdir(parents=True, exist_ok=True)
    source.write_text("// fixture source\n", encoding="utf-8")
    os.utime(source, ns=(1_000_000_000, 1_000_000_000))
    os.utime(dist, ns=(2_000_000_000, 2_000_000_000))
    (agents_home / "node_modules/.keep").parent.mkdir(parents=True, exist_ok=True)
    (agents_home / "node_modules/.keep").write_text("fixture\n", encoding="utf-8")


def run_launcher_fixture(
    tmp_path: Path,
    *,
    dist_mtime_ns: int | None,
    source_mtime_ns: int | None,
    expected_returncode: int = 0,
) -> subprocess.CompletedProcess[str]:
    agents_home = tmp_path / "agents"
    source = agents_home / "runtime" / "agent-fabric" / "src" / "mcp" / "main.ts"
    dist = agents_home / "runtime" / "agent-fabric" / "dist" / "mcp" / "main.js"
    loader = agents_home / "node_modules" / "tsx" / "dist" / "loader.mjs"
    fake_bin = tmp_path / "bin"
    fake_node = fake_bin / "node"
    loader.parent.mkdir(parents=True)
    copy_attestation_scripts(agents_home)
    fake_bin.mkdir()
    loader.write_text("// loader\n")
    # A real install carries the package manifest beside the loader, and
    # resolution checks it: the loader path is executed, so a bare file of the
    # right name is not evidence that it is really tsx.
    (loader.parent.parent / "package.json").write_text(
        '{"name":"tsx","version":"4.0.0"}\n', encoding="utf-8"
    )
    # The stub exists to capture the final exec, not to stand in for node
    # everywhere. Loader resolution probes the interpreter with `-e`, and those
    # probes must reach the real one, or resolution is being answered by this
    # shell script rather than by node.
    real_node = shutil.which("node") or "/usr/bin/env node"
    fake_node.write_text(
        "#!/bin/sh\n"
        f'[ "${{1:-}}" != "-e" ] || exec "{real_node}" "$@"\n'
        f'case "${{1:-}}" in */verify-npm-ci-attestation.mjs|*/write-npm-ci-attestation.mjs) exec "{real_node}" "$@" ;; esac\n'
        "printf '%s\\n' \"$@\"\n"
    )
    fake_node.chmod(0o755)
    # These cases are about dist-vs-source selection, so give the fixture a
    # current protocol build; the stale-protocol path is covered separately in
    # tests/test_agent_fabric_launcher.py.
    protocol = agents_home / "runtime" / "agent-fabric-protocol"
    protocol_source = protocol / "src" / "index.ts"
    protocol_dist = protocol / "dist" / "index.js"
    protocol_source.parent.mkdir(parents=True)
    protocol_dist.parent.mkdir(parents=True)
    protocol_source.write_text("// protocol source\n")
    protocol_dist.write_text("// protocol dist\n")
    os.utime(protocol_source, ns=(1_000_000_000, 1_000_000_000))
    os.utime(protocol_dist, ns=(9_000_000_000, 9_000_000_000))
    if source_mtime_ns is not None:
        source.parent.mkdir(parents=True)
        source.write_text("// source\n")
        os.utime(source, ns=(source_mtime_ns, source_mtime_ns))
    if dist_mtime_ns is not None:
        dist.parent.mkdir(parents=True)
        dist.write_text("// dist\n")
        os.utime(dist, ns=(dist_mtime_ns, dist_mtime_ns))

    attest_fixture(agents_home)

    result = subprocess.run(
        [str(ROOT / "scripts" / "agent-fabric-mcp")],
        env={
            **os.environ,
            "AGENTS_HOME": str(agents_home),
            "PATH": f"{fake_bin}{os.pathsep}{os.environ['PATH']}",
        },
        capture_output=True,
        text=True,
        timeout=10,
        check=False,
    )

    assert result.returncode == expected_returncode, result.stderr
    return result


def test_wrapper_rejects_dist_when_source_is_newer(tmp_path: Path) -> None:
    invoked = run_launcher_fixture(
        tmp_path, dist_mtime_ns=1_000_000_000, source_mtime_ns=2_000_000_000,
    ).stdout.splitlines()

    assert invoked[-1].endswith("/runtime/agent-fabric/src/mcp/main.ts")


def test_wrapper_uses_fresh_dist(tmp_path: Path) -> None:
    invoked = run_launcher_fixture(
        tmp_path, dist_mtime_ns=2_000_000_000, source_mtime_ns=1_000_000_000,
    ).stdout.splitlines()

    assert invoked == [str(tmp_path / "agents" / "runtime" / "agent-fabric" / "dist" / "mcp" / "main.js")]


def test_wrapper_uses_source_loader_when_dist_is_missing(tmp_path: Path) -> None:
    invoked = run_launcher_fixture(
        tmp_path, dist_mtime_ns=None, source_mtime_ns=1_000_000_000,
    ).stdout.splitlines()

    assert invoked == [
        "--import",
        str(tmp_path / "agents" / "node_modules" / "tsx" / "dist" / "loader.mjs"),
        str(tmp_path / "agents" / "runtime" / "agent-fabric" / "src" / "mcp" / "main.ts"),
    ]


def test_wrapper_fails_closed_when_dist_exists_but_source_tree_is_missing(tmp_path: Path) -> None:
    result = run_launcher_fixture(
        tmp_path,
        dist_mtime_ns=2_000_000_000,
        source_mtime_ns=None,
        expected_returncode=1,
    )

    assert result.stdout == ""
    assert "Fabric source tree is unavailable" in result.stderr


def test_source_wrapper_preserves_caller_cwd_for_project_seat_resolution(tmp_path: Path) -> None:
    agents_home = tmp_path / "agents"
    copy_runtime_fixture(agents_home)
    attest_fixture(agents_home)
    state = tmp_path / "state"
    state.mkdir(mode=0o700)
    environment = {
        **os.environ,
        "AGENTS_HOME": str(agents_home),
        "AGENT_FABRIC_SOCKET_PATH": str(tmp_path / "missing.sock"),
        "AGENT_FABRIC_STATE_DIRECTORY": str(state),
        "AGENT_FABRIC_SEAT": "codex",
    }
    result = subprocess.run(
        [str(agents_home / "scripts" / "agent-fabric-mcp")],
        cwd=tmp_path,
        env=environment,
        capture_output=True,
        text=True,
        timeout=10,
        check=False,
    )

    assert result.returncode == 0
    assert f"not provisioned for {tmp_path}" in result.stderr
    assert "exact-root bootstrap is available" in result.stderr
    assert "Fabric tools are unavailable until seats are provisioned" not in result.stderr
    assert "runtime/agent-fabric or an ancestor project" not in result.stderr


def test_wrapper_resolves_symlinked_install_and_rejects_relative_agents_home(tmp_path: Path) -> None:
    agents_home = tmp_path / "agents"
    copy_runtime_fixture(agents_home)
    attest_fixture(agents_home)
    state = tmp_path / "state"
    bin_directory = tmp_path / "bin"
    state.mkdir(mode=0o700)
    bin_directory.mkdir()
    wrapper = bin_directory / "agent-fabric-mcp"
    wrapper.symlink_to(agents_home / "scripts" / "agent-fabric-mcp")
    environment = {
        **os.environ,
        "AGENT_FABRIC_SOCKET_PATH": str(tmp_path / "missing.sock"),
        "AGENT_FABRIC_STATE_DIRECTORY": str(state),
        "AGENT_FABRIC_SEAT": "codex",
    }
    environment.pop("AGENTS_HOME", None)
    symlinked = subprocess.run(
        [str(wrapper)], cwd=tmp_path, env=environment, capture_output=True, text=True, timeout=10, check=False,
    )
    assert symlinked.returncode == 0
    assert f"not provisioned for {tmp_path}" in symlinked.stderr
    relative = subprocess.run(
        [str(agents_home / "scripts" / "agent-fabric-mcp")],
        cwd=tmp_path,
        env={**environment, "AGENTS_HOME": "relative"},
        capture_output=True,
        text=True,
        timeout=10,
        check=False,
    )
    assert relative.returncode == 2
    assert "AGENTS_HOME must be absolute" in relative.stderr
