from pathlib import Path
import os
import subprocess

import pytest


ROOT = Path(__file__).resolve().parents[1]
LAUNCHERS = ['fabric', 'fabric-mcp']
FIXED = ['/opt/homebrew/bin/node', '/usr/local/bin/node', '/usr/bin/node']


def fake_node(path, supported):
    """A stand-in node: answers the version probe, echoes the realpath probe, reports an exec."""
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(f'''#!/bin/sh
case "$1" in
  -e) case "$2" in *versions*) exit {0 if supported else 1} ;; *) printf %s "$3" ;; esac ;;
  *) echo "ran $0" ;;
esac
''')
    path.chmod(0o755)
    return path


def launch(tmp_path, name, path_dirs=(), **extra):
    loader = tmp_path / 'loader.mjs'
    loader.write_text('')
    env = {'HOME': str(tmp_path / 'home'), 'PATH': os.pathsep.join([*map(str, path_dirs), '/usr/bin', '/bin']),
           'AGENT_FABRIC_TSX_LOADER': str(loader), **extra}
    return subprocess.run([str(ROOT / 'runtime/fabric/bin' / name)], env=env, capture_output=True, text=True,
                          stdin=subprocess.DEVNULL, timeout=10)


@pytest.mark.parametrize('name', LAUNCHERS)
def test_an_explicit_fabric_node_is_used(tmp_path, name):
    node = fake_node(tmp_path / 'explicit/node', supported=True)
    result = launch(tmp_path, name, FABRIC_NODE=str(node))
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == f'ran {node}'


@pytest.mark.parametrize('name', LAUNCHERS)
def test_an_unsupported_fabric_node_is_refused_not_replaced(tmp_path, name):
    old = fake_node(tmp_path / 'explicit/node', supported=False)
    fake_node(tmp_path / 'path/node', supported=True)
    result = launch(tmp_path, name, [tmp_path / 'path'], FABRIC_NODE=str(old))
    assert result.returncode == 127
    assert f'FABRIC_NODE={old} is not an executable Node >=24.15.0 and <25' in result.stderr
    assert 'ran' not in result.stdout


@pytest.mark.parametrize('name', LAUNCHERS)
def test_path_node_comes_first(tmp_path, name):
    node = fake_node(tmp_path / 'path/node', supported=True)
    result = launch(tmp_path, name, [tmp_path / 'path'])
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == f'ran {node}'


@pytest.mark.parametrize('name', LAUNCHERS)
def test_an_unsupported_path_node_is_skipped_for_a_supported_fallback(tmp_path, name):
    old = fake_node(tmp_path / 'path/node', supported=False)
    result = launch(tmp_path, name, [tmp_path / 'path'])
    assert f'ran {old}' not in result.stdout
    if result.returncode == 127:
        # No supported node on this machine: the error names what it tried.
        assert f'Unsupported: {old}' in result.stderr and 'set FABRIC_NODE' in result.stderr
    else:
        # A real supported node on this machine ran Fabric, which may then fail on the empty test loader.
        assert 'Unsupported' not in result.stderr


@pytest.mark.skipif(any(Path(path).exists() for path in FIXED), reason='a system node would be chosen first')
@pytest.mark.parametrize('name', LAUNCHERS)
def test_user_local_node_is_the_last_fallback(tmp_path, name):
    fake_node(tmp_path / 'path/node', supported=False)
    node = fake_node(tmp_path / 'home/.local/bin/node', supported=True)
    result = launch(tmp_path, name, [tmp_path / 'path'])
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == f'ran {node}'
