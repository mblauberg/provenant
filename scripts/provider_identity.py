"""Non-secret CLI identity scopes shared by quota and cooldown routing.

Only paths and configured labels are inspected; credentials are never opened.
"""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import pwd
import re
import sys


LABEL = re.compile(r'[a-z0-9][a-z0-9._-]{0,63}')
_TELEMETRY = None
_TELEMETRY_ROOT = None


def config():
    root = Path(os.environ.get('AGENT_FABRIC_INSTANCE_ROOT') or '~/.agents').expanduser()
    try:
        value = json.loads((root / 'config/telemetry.json').read_text())
    except (OSError, ValueError):
        return {}
    return value if isinstance(value, dict) else {}


def configured():
    value = config().get('accounts') or {}
    if not isinstance(value, dict) or not all(isinstance(entries, dict) for entries in value.values()):
        raise ValueError('telemetry accounts must map provider to {label: {identity_dir}}')
    for entries in value.values():
        for label in entries:
            if not LABEL.fullmatch(str(label)):
                raise ValueError(f'account label {label!r} must be a short lowercase name, never an email or token')
    return value


def default_identity(provider):
    home = Path.home()
    if provider == 'cursor':
        return home / ('Library/Application Support/Cursor/User/globalStorage' if sys.platform == 'darwin'
                       else '.config/Cursor/User/globalStorage')
    if provider == 'agy':
        # The login Keychain belongs to the OS user, not HOME or AGY_CONFIG_DIR.
        return Path(pwd.getpwuid(os.getuid()).pw_dir) / 'Library/Keychains/login.keychain-db'
    return {'claude': home / '.claude', 'codex': home / '.codex',
            'opencode': home / '.local/share/opencode', 'kiro': home / '.kiro',
            'copilot': home / '.copilot'}.get(provider)


def selected_identity(provider):
    variable = {'claude': 'CLAUDE_CONFIG_DIR', 'codex': 'CODEX_HOME'}.get(provider)
    if variable and os.environ.get(variable):
        return Path(os.environ[variable]).expanduser()
    if provider == 'opencode' and os.environ.get('XDG_DATA_HOME'):
        return Path(os.environ['XDG_DATA_HOME']).expanduser() / 'opencode'
    return default_identity(provider)


def same_path(left, right):
    try:
        return Path(left).expanduser().resolve() == Path(right).expanduser().resolve()
    except (OSError, RuntimeError, TypeError, ValueError):
        return False


def account_key(provider, identity_dir, entries=None):
    if identity_dir is None:
        return None
    entries = configured().get(provider, {}) if entries is None else entries
    for label, entry in sorted(entries.items()):
        if isinstance(entry, dict) and entry.get('enabled') is not False:
            identity = entry.get('identity_dir') or selected_identity(provider)
            if identity is not None and same_path(identity_dir, identity):
                return label
    if same_path(identity_dir, default_identity(provider)):
        return 'default'
    resolved = str(Path(identity_dir).expanduser().resolve())
    return 'path-' + hashlib.sha256(os.fsencode(resolved)).hexdigest()[:20]


def host_id():
    """Use the telemetry/peer host id owner (hosts.json local_host, else hostname)."""
    global _TELEMETRY, _TELEMETRY_ROOT
    root = Path(os.environ.get('AGENT_FABRIC_INSTANCE_ROOT') or '~/.agents').expanduser().resolve()
    if _TELEMETRY is None or _TELEMETRY_ROOT != root:
        spec = importlib.util.spec_from_file_location('provenant_identity_telemetry', Path(__file__).with_name('telemetry.py'))
        _TELEMETRY = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(_TELEMETRY)
        _TELEMETRY_ROOT = root
    return _TELEMETRY.host_id()


def scope(provider, *, default=False):
    try:
        identity = default_identity(provider) if default else selected_identity(provider)
        account = account_key(provider, identity)
    except (OSError, RuntimeError, ValueError, KeyError):
        account = None
    return {'account': account, 'host': host_id()}


def matches(record, active):
    return (record.get('account'), record.get('host')) == (active.get('account'), active.get('host'))
