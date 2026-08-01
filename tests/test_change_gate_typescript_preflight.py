import subprocess
import sys
from pathlib import Path

import pytest

from scripts.change_gates import GateError, gate_right_reason_red


def _commit_fixture(root):
    subprocess.run(["git", "-C", str(root), "init", "--quiet"], check=True)
    subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)
    subprocess.run(
        [
            "git",
            "-C",
            str(root),
            "-c",
            "user.name=Gate Test",
            "-c",
            "user.email=gate-test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "base",
        ],
        check=True,
    )


def _typescript_fixture(tmp_path, *, build_body=None, with_node_modules=True):
    source = tmp_path / "source"
    source.mkdir()
    (source / "runtime" / "agent-fabric" / "scripts").mkdir(parents=True)
    (source / "runtime" / "agent-fabric" / "scripts" / "write-npm-ci-attestation.mjs").write_text(
        "import { writeFile } from 'node:fs/promises';\n"
        "import { join } from 'node:path';\n"
        "await writeFile(join(process.argv[2], 'runtime/agent-fabric/.npm-ci-attestation'), 'base-writer\\n');\n",
        encoding="utf-8",
    )
    (source / "scripts").mkdir()
    build = source / "scripts" / "agent-fabric-protocol-build"
    build.write_text(
        "#!/bin/sh\n"
        "set -eu\n"
        "test -L \"$AGENTS_HOME/node_modules\"\n"
        "test \"$(cat \"$AGENTS_HOME/runtime/agent-fabric/.npm-ci-attestation\")\" = base-writer\n"
        "mkdir -p \"$AGENTS_HOME/runtime/agent-fabric-protocol/dist\"\n"
        "printf 'base-build\\n' > \"$AGENTS_HOME/protocol-build-marker\"\n"
        "printf 'base-dist\\n' > \"$AGENTS_HOME/runtime/agent-fabric-protocol/dist/index.js\"\n"
        "printf 'base-stamp\\n' > \"$AGENTS_HOME/runtime/agent-fabric-protocol/dist/.root-manifests.sha256\"\n"
        + (build_body or ""),
        encoding="utf-8",
    )
    build.chmod(0o755)
    (source / "tests").mkdir()
    (source / "tests" / "protocol.test.ts").write_text("base test\n", encoding="utf-8")
    (source / "tests" / "protocol_gate.py").write_text(
        "from pathlib import Path\n"
        "if Path('runtime/agent-fabric/.npm-ci-attestation').read_text() != 'base-writer\\n':\n"
        "    print('AGENT_FABRIC_PROTOCOL_BUILD_STALE: base attestation was not refreshed')\n"
        "    raise SystemExit(1)\n"
        "if Path('protocol-build-marker').read_text() != 'base-build\\n':\n"
        "    print('AGENT_FABRIC_PROTOCOL_BUILD_STALE: base protocol build was not run')\n"
        "    raise SystemExit(1)\n"
        "if Path('runtime/agent-fabric-protocol/dist/index.js').read_text() != 'base-dist\\n':\n"
        "    print('AGENT_FABRIC_PROTOCOL_BUILD_STALE: base dist was not produced')\n"
        "    raise SystemExit(1)\n"
        "raise AssertionError('intended TypeScript assertion')\n",
        encoding="utf-8",
    )
    _commit_fixture(source)

    if with_node_modules:
        (source / "node_modules").mkdir()
        (source / "node_modules" / "installed.marker").write_text("installed\n", encoding="utf-8")
    return source


def test_typescript_right_reason_red_prepares_base_with_its_own_writer_and_build(
    tmp_path, capsys
):
    source = _typescript_fixture(tmp_path)
    writer = source / "runtime" / "agent-fabric" / "scripts" / "write-npm-ci-attestation.mjs"
    writer.write_text(
        "import { writeFile } from 'node:fs/promises';\n"
        "import { join } from 'node:path';\n"
        "await writeFile(join(process.argv[2], 'runtime/agent-fabric/.npm-ci-attestation'), 'current-writer\\n');\n",
        encoding="utf-8",
    )
    build = source / "scripts" / "agent-fabric-protocol-build"
    build.write_text(
        "#!/bin/sh\n"
        "set -eu\n"
        "printf 'current-build\\n' > \"$AGENTS_HOME/protocol-build-marker\"\n"
        "exit 1\n",
        encoding="utf-8",
    )
    build.chmod(0o755)
    source_attestation = source / "runtime" / "agent-fabric" / ".npm-ci-attestation"
    source_attestation.write_bytes(b"source-attestation\n")
    source_dist = source / "runtime" / "agent-fabric-protocol" / "dist" / "index.js"
    source_dist.parent.mkdir(parents=True)
    source_dist.write_bytes(b"source-dist\n")
    source_stamp = source_dist.parent / ".root-manifests.sha256"
    source_stamp.write_bytes(b"source-stamp\n")
    source_dist_before = {
        path.name: path.read_bytes() for path in source_dist.parent.iterdir()
    }

    result = gate_right_reason_red(
        source,
        "HEAD",
        {
            "py": f'{sys.executable} -c "raise SystemExit(1)" {{test}}',
            "ts": f'{sys.executable} tests/protocol_gate.py {{test}}',
        },
        ["tests/protocol.test.ts"],
        tmp_path / "scratch",
    )
    output = capsys.readouterr().out

    assert result == 0, output
    assert "classification=assertion-failure" in output
    assert source_attestation.read_bytes() == b"source-attestation\n"
    assert {
        path.name: path.read_bytes() for path in source_dist.parent.iterdir()
    } == source_dist_before
    assert not list((tmp_path / "scratch").glob("gate-*"))


def test_python_right_reason_red_skips_typescript_scratch_preflight(tmp_path, capsys):
    source = tmp_path / "source"
    source.mkdir()
    (source / "scripts").mkdir()
    build = source / "scripts" / "agent-fabric-protocol-build"
    build.write_text(
        "#!/bin/sh\n"
        "printf 'build must not run\\n' >&2\n"
        "exit 91\n",
        encoding="utf-8",
    )
    build.chmod(0o755)
    (source / "runtime" / "agent-fabric" / "scripts").mkdir(parents=True)
    (source / "runtime" / "agent-fabric" / "scripts" / "write-npm-ci-attestation.mjs").write_text(
        "throw new Error('writer must not run');\n", encoding="utf-8"
    )
    (source / "tests").mkdir()
    (source / "tests" / "python_test.py").write_text("base test\n", encoding="utf-8")
    _commit_fixture(source)
    (source / "tests" / "python_test.py").write_text("current test\n", encoding="utf-8")

    result = gate_right_reason_red(
        source,
        "HEAD",
        {
            "py": f'{sys.executable} -c "raise AssertionError(\'python assertion\')" {{test}}',
            "ts": f'{sys.executable} -c "raise SystemExit(91)" {{test}}',
        },
        ["tests/python_test.py"],
        tmp_path / "scratch",
    )

    assert result == 0
    assert "classification=assertion-failure" in capsys.readouterr().out


def test_typescript_right_reason_red_reports_exact_missing_install_repair(tmp_path):
    source = _typescript_fixture(tmp_path, with_node_modules=False)
    ran = tmp_path / "test-ran"
    command = (
        f'{sys.executable} -c "from pathlib import Path; '
        f'Path({str(ran)!r}).write_text(\'ran\'); raise AssertionError(\'must not run\')" {{test}}'
    )

    with pytest.raises(
        GateError,
        match=r"^CHANGE_GATES_TS_PREREQUISITE: source node_modules is missing; "
        r"rerun: scripts/install-agent-fabric-dependencies$",
    ):
        gate_right_reason_red(
            source,
            "HEAD",
            {"py": command, "ts": command},
            ["tests/protocol.test.ts"],
            tmp_path / "scratch",
        )

    assert not ran.exists()


def test_typescript_right_reason_red_reports_typed_protocol_build_failure(tmp_path):
    source = _typescript_fixture(tmp_path, build_body="exit 7\n")
    ran = tmp_path / "test-ran"
    command = (
        f'{sys.executable} -c "from pathlib import Path; '
        f'Path({str(ran)!r}).write_text(\'ran\'); raise AssertionError(\'must not run\')" {{test}}'
    )

    with pytest.raises(GateError, match=r"^CHANGE_GATES_TS_BUILD: .*returncode=7"):
        gate_right_reason_red(
            source,
            "HEAD",
            {"py": command, "ts": command},
            ["tests/protocol.test.ts"],
            tmp_path / "scratch",
        )

    assert not ran.exists()


def test_typescript_scratch_preflight_rejects_base_symlink_escape(tmp_path):
    source = tmp_path / "source"
    outside = source / "outside"
    source.mkdir()
    outside.mkdir()
    (source / "runtime").mkdir()
    (source / "runtime" / "agent-fabric").symlink_to("../outside", target_is_directory=True)
    (source / "scripts").mkdir()
    build = source / "scripts" / "agent-fabric-protocol-build"
    build.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    build.chmod(0o755)
    (source / "tests").mkdir()
    (source / "tests" / "protocol.test.ts").write_text("base test\n", encoding="utf-8")
    _commit_fixture(source)
    (source / "runtime" / "agent-fabric").unlink()
    (source / "runtime" / "agent-fabric" / "scripts").mkdir(parents=True)
    (source / "runtime" / "agent-fabric" / "scripts" / "write-npm-ci-attestation.mjs").write_text(
        "throw new Error('current writer');\n", encoding="utf-8"
    )
    (source / "node_modules").mkdir()

    with pytest.raises(GateError, match=r"^CHANGE_GATES_TS_PREREQUISITE: .*contains a symlink"):
        gate_right_reason_red(
            source,
            "HEAD",
            {
                "py": f'{sys.executable} -c "raise SystemExit(1)" {{test}}',
                "ts": f'{sys.executable} -c "raise AssertionError(\'must not run\')" {{test}}',
            },
            ["tests/protocol.test.ts"],
            tmp_path / "scratch",
        )

    assert not (outside / ".npm-ci-attestation").exists()
