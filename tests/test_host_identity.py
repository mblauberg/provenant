"""One local host id for fabric hosts, quota and cooldowns."""
import json
import socket
import sys
from pathlib import Path

import pytest

from tests.support.modules import load_script

ROOT = Path(__file__).resolve().parents[1]


def load(name, path):
    return load_script(ROOT / path, name, register=True)


@pytest.fixture
def owners(tmp_path, monkeypatch):
    """Each layer's view of the host id, isolated from the real ~/.agents."""
    for name in ('host_identity_fabric_hosts', 'host_identity_provider_identity'):
        monkeypatch.delitem(sys.modules, name, raising=False)
    monkeypatch.setenv('HOME', str(tmp_path / 'home'))
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(tmp_path / 'instance'))
    monkeypatch.setattr(socket, 'gethostname', lambda: 'Anes-MacBook-Pro.local')
    hosts = load('host_identity_fabric_hosts', 'scripts/fabric_hosts.py')
    identity = load('host_identity_provider_identity', 'scripts/provider_identity.py')

    def views():
        return {'identity': identity.host_id(),
                'fabric': hosts.HostsConfig().local_host}
    return hosts, views, tmp_path / 'instance/.agent-fabric/hosts.json'


def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(value if isinstance(value, str) else json.dumps(value))


owner = load('host_identity_under_test', 'scripts/host_identity.py')


@pytest.mark.parametrize('raw, expected', [
    ('Anes-MacBook-Pro.local', 'anes-macbook-pro'),
    ('studio', 'studio'),
    ('My Mac (2)', 'my-mac-2'),
    ('--build__box--.example.com', 'build-box'),
    ('___', 'local'),
    ('.local', 'local'),
    ('', 'local'),
    ('日本', 'local'),
    ('a' * 80, 'a' * 63),
    ('a' * 62 + '-b', 'a' * 62),
])
def test_hostname_slug_matches_the_host_name_rule(raw, expected):
    assert owner.slug(raw) == expected
    assert owner.NAME.fullmatch(owner.slug(raw))


def test_a_failing_gethostname_falls_back_to_local(owners, monkeypatch):
    def broken():
        raise OSError('no hostname')
    monkeypatch.setattr(socket, 'gethostname', broken)
    _, views, _ = owners
    assert set(views().values()) == {'local'}


def test_no_hosts_file_gives_every_layer_the_hostname_slug(owners):
    hosts, views, path = owners
    assert not path.exists()
    assert set(views().values()) == {'anes-macbook-pro'}
    assert hosts.load_config().local_host == 'anes-macbook-pro'


def test_empty_peers_keeps_the_configured_id_and_runs_local_only(owners):
    hosts, views, path = owners
    write(path, {'schema_version': 1, 'local_host': 'studio', 'peers': {}})
    assert set(views().values()) == {'studio'}
    config = hosts.load_config()
    assert (config.local_host, config.peers) == ('studio', {})


@pytest.mark.parametrize('local_host', ['My Mac', 'Studio', 'studio.local', '', 7, None, 'a' * 64])
def test_an_invalid_local_host_fails_the_same_way_in_every_layer(owners, local_host):
    hosts, views, path = owners
    write(path, {'schema_version': 1, 'local_host': local_host, 'peers': {}})
    with pytest.raises(hosts.HostError) as raised:
        hosts.load_config()
    assert raised.value.code == 'invalid_config'
    # No layer accepts the bad value; all use the hostname's slug.
    assert set(views().values()) == {'anes-macbook-pro'}


def test_a_malformed_hosts_file_falls_back_to_the_slug(owners):
    hosts, views, path = owners
    write(path, '{"local_host": "studio", "local_host": "other"}')
    assert set(views().values()) == {'anes-macbook-pro'}
    write(path, '{broken')
    assert set(views().values()) == {'anes-macbook-pro'}


def test_a_broken_peer_entry_keeps_a_valid_local_host_in_every_layer(owners):
    hosts, views, path = owners
    write(path, {'schema_version': 1, 'local_host': 'studio', 'peers': {'Bad Name': {'ssh_destination': 'x'}}})
    with pytest.raises(hosts.HostError):
        hosts.load_config()
    assert set(views().values()) == {'studio'}


def test_earlier_stored_ids_read_as_this_host(owners):
    _, _, path = owners
    write(path, {'schema_version': 1, 'local_host': 'studio'})
    for stored in ('Anes-MacBook-Pro.local', 'anes-macbook-pro', 'local', 'studio'):
        assert owner.canonical(stored, 'studio') == 'studio'
    for other in ('peer', 'Peer-Box.local', None, ['studio']):
        assert owner.canonical(other, 'studio') == other


def test_a_configured_peer_named_like_this_hostname_is_never_a_local_alias(owners, monkeypatch):
    _, _, path = owners
    monkeypatch.setattr(socket, 'gethostname', lambda: 'box.local')
    write(path, {'schema_version': 1, 'local_host': 'studio', 'peers': {'box': {'ssh_destination': 'box'},
                                                                         'local': {'ssh_destination': 'l'}}})
    assert owner.aliases('studio') == {'studio', 'box.local'}
    assert owner.canonical('box', 'studio') == 'box'
    assert owner.canonical('local', 'studio') == 'local'
    assert owner.canonical('box.local', 'studio') == 'studio'
