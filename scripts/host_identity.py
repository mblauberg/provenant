"""The one owner of this machine's host id, shared by fabric hosts, telemetry, quota and cooldowns."""
from __future__ import annotations

import json
import os
from pathlib import Path
import re
import socket

NAME_LIMIT = 63
NAME = re.compile(rf'[a-z0-9-]{{1,{NAME_LIMIT}}}')
# The id used when the hostname yields nothing, and the id fabric used before hosts.json existed.
FALLBACK = 'local'


class InvalidHost(ValueError):
    code = 'invalid_config'


def config_path():
    root = Path(os.environ.get('AGENT_FABRIC_INSTANCE_ROOT') or '~/.agents').expanduser()
    return root / '.agent-fabric/hosts.json'


def hostname():
    try:
        value = socket.gethostname()
    except (OSError, UnicodeError):
        return ''
    return value if isinstance(value, str) else ''


def slug(value):
    """A hostname as a host name: lowercase, cut at the first dot, invalid characters as '-'."""
    text = re.sub(r'[^a-z0-9-]', '-', str(value or '').lower().split('.', 1)[0])
    text = re.sub(r'-+', '-', text).strip('-')[:NAME_LIMIT].strip('-')
    return text or FALLBACK


def configured(path=None):
    """hosts.json `local_host`, or None without a hosts.json; InvalidHost when it is unusable."""
    path = Path(path) if path is not None else config_path()
    try:
        raw = path.read_bytes()
    except FileNotFoundError:
        return None
    except OSError as exc:
        raise InvalidHost('Cannot read host configuration') from exc
    def unique(items):
        if len({key for key, _ in items}) != len(items):
            raise ValueError('duplicate member')
        return dict(items)
    try:
        value = json.loads(raw, object_pairs_hook=unique)
    except (ValueError, UnicodeError) as exc:
        raise InvalidHost('Host configuration must be strict JSON') from exc
    local = value.get('local_host') if isinstance(value, dict) else None
    if not isinstance(local, str) or not NAME.fullmatch(local):
        raise InvalidHost(f'local_host must be 1-{NAME_LIMIT} lowercase letters, digits and hyphens')
    return local


def resolve(path=None):
    """(host id, config error or None): a valid `local_host`, otherwise the hostname's slug."""
    try:
        local = configured(path)
    except InvalidHost as exc:
        return slug(hostname()), str(exc)
    return local or slug(hostname()), None


def host_id(path=None):
    return resolve(path)[0]


def aliases(current=None):
    """Ids this machine's own stores may hold: the current id, the raw hostname, its slug and 'local'."""
    raw = hostname()
    return {current or host_id(), raw, slug(raw), FALLBACK} - {''}


def canonical(value, current=None):
    """Read a host stored by this machine under an earlier default as the current id."""
    current = current or host_id()
    return current if isinstance(value, str) and value in aliases(current) else value
