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
_ACCOUNT_WARNING_EMITTED = False


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
        if sys.platform == 'darwin':
            # The login Keychain belongs to the OS user, not HOME or AGY_CONFIG_DIR.
            return Path(pwd.getpwuid(os.getuid()).pw_dir) / 'Library/Keychains/login.keychain-db'
        # No Keychain elsewhere: agy's own state directory names the sign-in.
        return home / '.gemini/antigravity-cli'
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
    enabled_entries = [(label, entry) for label, entry in entries.items()
                       if isinstance(entry, dict) and entry.get('enabled') is not False]
    # Explicit paths take precedence over labels that implicitly name the default.
    for label, entry in sorted(enabled_entries, key=lambda item: (not bool(item[1].get('identity_dir')), item[0])):
        identity = entry.get('identity_dir') or default_identity(provider)
        if identity is not None and same_path(identity_dir, identity):
            return label
    if same_path(identity_dir, default_identity(provider)):
        return 'default'
    resolved = str(Path(identity_dir).expanduser().resolve())
    return 'path-' + hashlib.sha256(os.fsencode(resolved)).hexdigest()[:20]


def _owner():
    name = 'provenant_host_identity'
    if name not in sys.modules:
        spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name('host_identity.py'))
        module = importlib.util.module_from_spec(spec)
        sys.modules[name] = module
        spec.loader.exec_module(module)
    return sys.modules[name]


def host_id():
    """The shared host id (scripts/host_identity.py): a valid hosts.json local_host, else the hostname's slug."""
    root = Path(os.environ.get('AGENT_FABRIC_INSTANCE_ROOT') or '~/.agents').expanduser()
    return _owner().host_id(root / '.agent-fabric/hosts.json')


def same_host(value, current=None):
    """This machine's records read as one host, whichever earlier default stored them."""
    return _owner().canonical(value, current or host_id())


def host_aliases(current=None):
    """The current id first, then the earlier ids this machine's stores may hold."""
    current = current or host_id()
    return [current, *sorted(_owner().aliases(current) - {current})]


def scope(provider, *, default=False):
    global _ACCOUNT_WARNING_EMITTED
    try:
        identity = default_identity(provider) if default else selected_identity(provider)
        account = account_key(provider, identity)
    except ValueError as exc:
        if not _ACCOUNT_WARNING_EMITTED:
            _ACCOUNT_WARNING_EMITTED = True
            print(f"provider identity: telemetry.accounts config error: {exc}", file=sys.stderr)
        account = None
    except (OSError, RuntimeError, KeyError):
        account = None
    return {'account': account, 'host': host_id()}


def matches(record, active):
    return ((record.get('account'), same_host(record.get('host')))
            == (active.get('account'), same_host(active.get('host'))))
