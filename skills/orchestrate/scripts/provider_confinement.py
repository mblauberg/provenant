#!/usr/bin/env python3
"""Provider confinement policy and filesystem grants for the supervisor."""

from __future__ import annotations

import functools
import importlib.util
import json
import logging
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from adapters import claude as claude_adapter

# Home-relative paths a confined provider needs for its own state and sign-in.
# Initial probes (2026-09-24) were narrowed using name-only layouts for #927:
# executable/configuration trees stay read-only even alongside mutable state.
# A "*" suffix covers atomic-rewrite siblings, without granting their children.
# npm/uv/Corepack caches are redirected into each attempt; never let a lane
# poison ~/.npm's reusable packages (_cacache, _npx, _prebuilds or _tuf).
# Keep the Claude startup/config file and all rewrite siblings read-only in every lane.

READ_ONLY_PROVIDER_CONFIG = (".claude.json",)

NPM_STATE_WRITES = (".npm/_logs",)

NPM_STATE_FILES = (".npm/_update-notifier-last-checked",)

AGY_STATE_DIRS = ("annotations", "brain", "conversations", "crashes", "implicit", "knowledge", "log",
                  "plugin_data", "presence", "scratch")

CONFINED_STATE = {
    "agy": {
        "home": ".gemini",
        "read_write": tuple(f".gemini/antigravity-cli/{name}" for name in (*AGY_STATE_DIRS,
            "antigravity-oauth-token*", "mcp_oauth_tokens.json*",
            "history.jsonl*")) + (".gemini/oauth_creds.json*",),
        "write_literal": tuple(f".gemini/antigravity-cli/{name}" for name in (
            "conversation_summaries.db", "conversation_summaries.db-wal", "conversation_summaries.db-shm",
            "conversation_summaries.db-journal", "jetbox_summaries_proto.pb", "jetski_state.pbtxt",
            "last_check.timestamp", "installation_id", "updater/update.lock", "updater/update_status.json",
            "cache/CHANGELOG.md", "cache/conversation_metadata.json", "cache/default_project_id.txt",
            "cache/last_conversations.json", "cache/onboarding.json", "cache/projects.json")),
        "atomic_siblings": True,
        "prepare_dirs": lambda root, plan: tuple(root / "antigravity-cli" / name
                                                 for name in (*AGY_STATE_DIRS, "cache", "updater")),
        # config/projects holds project permission overrides and must also stay read-only.
        # sidecars schedule unsandboxed commands and are configuration, never runtime state.
        # plugin_data and the protobuf/knowledge state still need live write evidence.
        # In particular bin/, builtin/, mcp/, plugins/, skills/ and cache/marketplaces/ are not state grants.
        "no_symlinks": (".gemini",),
        # Without the keychain file agy falls back to an interactive sign-in and times out.
        "read": (".gemini/antigravity-cli", ".gemini/config", ".gemini/oauth_creds.json*",
                 "Library/Keychains/login.keychain-db"),
    },
    "opencode": {
        "read_write": (".local/share/opencode", ".local/state/opencode", ".cache/opencode"),
        # ~/.opencode is the install script's home and OpenCode v2's legacy global config directory;
        # its config discovery stats it at startup and fails with a 500 when that is denied.
        "read": (".config/opencode", ".opencode"),
        # Discovery lstats these names in the cwd and every ancestor even with Claude compatibility off.
        "ancestor_metadata": (".claude", ".codex"),
    },
    "claude": {
        "home": ".claude",
        "read_write": tuple(f".claude/{name}" for name in (
            ".credentials.json*", "cache", "debug", "feedback", "file-history", "ide", "jobs",
            "paste-cache", "sessions", "state", "teams", "telemetry", "uploads",
            "history.jsonl*", "stats-cache.json*", "mcp-needs-auth-cache.json*", "gh-pr-status-cache.json*",
            "statusline-lanes.json*")) + (".cache/claude", *NPM_STATE_WRITES),
        "write_literal": NPM_STATE_FILES,
        # Project roots also contain auto-loaded memory. Only UUID transcripts/session trees are state;
        # no root creation/rename grant that could transplant a crafted directory containing memory.
        "write_regex": (r"projects/[^/]+/[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}"
                        r"-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}(\.jsonl[^/]*|(/.*)?)",),
        "prepare_dirs": lambda root, plan: (root / "projects" / _claude_project_dir(root, plan["cwd"]).name,),
        # Shell snapshots and session-env can be sourced by later shells and stay read-only.
        # The absent ~/.cache/claude and jobs/ide/state/teams need smoke evidence when quota returns.
        "no_symlinks": (".claude", ".cache/claude", ".npm"),
        "read": (".claude", ".claude.json", ".cache/claude", ".npm"),
        # Claude Code keeps its OAuth sign-in in the login keychain; a read-only lane is signed out
        # without it. Only OAuth lanes get it: bare and endpoint lanes sign in with a key.
        "oauth_read": ("Library/Keychains/login.keychain-db",),
    },
    "cursor": {
        "home": ".cursor",
        "read_write": tuple(f".cursor/{name}" for name in (
            "chats", "ai-tracking", "plans", "agent-cli-state.json*", "ide_state.json*",
            "prompt_history.json*", "statsig-cache.json*")) + (".cache/cursor", *NPM_STATE_WRITES),
        "write_literal": NPM_STATE_FILES,
        # A project also holds workspace trust, MCP approvals and rules. Its root stays read-only.
        "write_regex": (r"projects/[^/]+/(agent-transcripts|canvases|terminals)(/.*)?",
                        r"projects/[^/]+/(worker\.log|worker\.sock|mcp-auth\.json[^/]*|mcp-cache\.json[^/]*)"),
        "prepare_dirs": lambda root, plan: (root / "projects" /
                                           re.sub(r"[^A-Za-z0-9]+", "-", str(Path(plan["cwd"]).resolve())).strip("-"),),
        # cli-config.json, argv.json, mcp.json, hooks and extensions are configuration. The absent
        # ~/.cache/cursor and projects/plans/ai-tracking need smoke evidence when quota returns.
        "no_symlinks": (".cursor", ".cache/cursor", ".npm"),
        # cursor-agent keeps its sign-in in the login keychain; a read-only lane is signed out without it.
        "read": (".cursor", ".cache/cursor", ".npm", "Library/Keychains/login.keychain-db"),
    },
    "kiro": {
        "home": ".kiro",
        # kiro-cli keeps its sign-in and refreshed tokens in data.sqlite3, guarded by .refresh.lock,
        # which it opens for writing before reading the token. The rest of its support directory holds
        # shell hooks the user's shell sources and binaries it runs, so stays read-only.
        "read_write": (".cache/kiro", *NPM_STATE_WRITES),
        # SQLite creates and removes these sidecars; each is one file at its own name.
        "write_literal": tuple(f"Library/Application Support/kiro-cli/data.sqlite3-{suffix}"
                               for suffix in ("wal", "shm", "journal")) + NPM_STATE_FILES,
        # Rewritten in place only: a lane cannot swap either for a link its unconfined CLI would follow.
        "write_in_place": ("Library/Application Support/kiro-cli/data.sqlite3",
                           "Library/Application Support/kiro-cli/.refresh.lock"),
        "no_links": ("Library/Application Support/kiro-cli",),
        "no_symlinks": (".kiro", ".cache/kiro", ".npm"),
        "read": (".kiro", ".cache/kiro", "Library/Application Support/kiro-cli", ".npm"),
        # Its prompt history spans every project.
        "read_denied": ("Library/Application Support/kiro-cli/history",),
        # Its engine finds its app bundle through the launcher links in ~/.local/bin.
        "read_literal": (".local/bin", ".local/bin/kiro-cli", ".local/bin/kiro-cli-chat",
                         ".local/bin/kiro-cli-term"),
    },
}

# Codex needs its native broker, configuration and event services under seatbelt.
CODEX_BASE_MACH_SERVICES = (
    "com.apple.analyticsd", "com.apple.analyticsd.messagetracer", "com.apple.appsleep",
    "com.apple.bsd.dirhelper", "com.apple.cfprefsd.agent", "com.apple.cfprefsd.daemon",
    "com.apple.diagnosticd", "com.apple.FSEvents", "com.apple.logd", "com.apple.logd.events",
    "com.apple.networkd", "com.apple.ocspd", "com.apple.PowerManagement.control",
    "com.apple.runningboard", "com.apple.secinitd", "com.apple.SecurityServer",
    "com.apple.system.DirectoryService.libinfo_v1", "com.apple.system.logger",
    "com.apple.system.notification_center", "com.apple.system.opendirectoryd.libinfo",
    "com.apple.system.opendirectoryd.membership", "com.apple.SystemConfiguration.configd",
    "com.apple.SystemConfiguration.DNSConfiguration", "com.apple.trustd",
    "com.apple.trustd.agent", "com.apple.xpc.activity.unmanaged",
)

# Chrome needs the macOS font, window, launch and privacy services.
CODEX_BROWSER_MACH_SERVICES = (
    "com.apple.fonts", "com.apple.coreservices.launchservicesd",
    "com.apple.windowserver.active", "com.apple.tccd.system",
    "com.apple.distributed_notifications@Uv3",
)

# Chrome and Chromium need to register their sandbox-local rendezvous ports.
CODEX_BROWSER_MACH_PORT_PREFIXES = (
    "com.google.Chrome.MachPortRendezvousServer.",
    "org.chromium.Chromium.MachPortRendezvousServer.",
)

# PostgreSQL initialisation needs System V shared memory and semaphores.
CODEX_POSTGRES_IPC_OPERATIONS = ("ipc-sysv-shm", "ipc-sysv-sem")

EXTRA_DENIED_READS = (".claude/projects", ".codex/sessions")

# Home-relative credential stores. No lane reads them, whatever else its profile grants.
CREDENTIAL_STORES = (
    '.ssh', '.aws', '.azure', '.gnupg', '.codex', '.claude',
    '.gemini', '.cursor', '.kiro', '.docker', '.kube', '.netrc', '.npmrc',
    '.git-credentials', '.pypirc', '.pgpass', '.vault-token', '.password-store', '.boto',
    '.s3cfg', '.terraform.d', '.cargo/credentials', '.cargo/credentials.toml',
    '.gem/credentials', '.config/git/credentials', '.config/hub', '.config/op',
    '.config/gh', '.config/gcloud', '.config/claude', '.config/codex',
    '.config/openai', '.config/opencode', '.local/share/opencode', 'Library/Keychains',
    'Library/Application Support',
)

# Instruction files OpenCode v2 reads from each ancestor of its cwd.
OPENCODE_INSTRUCTION_FILES = ("AGENTS.md", "CLAUDE.md", "HARNESS.md")

def _opencode_discovery_grantable(path, *, directory=False):
    # A symlinked discovery entry must not carry a read grant to its target: files only when the target is an
    # instruction file outside credential stores, directories only when the target is not a credential store.
    path = Path(path)
    if not path.is_symlink():
        return True
    target = path.resolve()
    if credential_path(target) and not target.is_relative_to(Path.home().resolve() / ".claude"):
        return False
    if directory:
        return target.is_dir()
    return target.is_file() and target.name in OPENCODE_INSTRUCTION_FILES

@functools.lru_cache(maxsize=1)
def _fabric_hosts_owner():
    name = "provenant_provider_hosts"
    if name not in sys.modules:
        spec = importlib.util.spec_from_file_location(
            name, Path(__file__).resolve().parents[3] / "scripts/fabric_hosts.py")
        module = importlib.util.module_from_spec(spec)
        sys.modules[name] = module
        spec.loader.exec_module(module)
    return sys.modules[name]

def _jsonc_loads(text):
    # The one JSONC reader lives in the product's scripts/lib; a skill reaches it by path.
    spec = importlib.util.spec_from_file_location(
        "provenant_jsonc", Path(__file__).resolve().parents[3] / "scripts/lib/jsonc.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.parse_jsonc(text)[0]

def _opencode_configured_instructions(home, config_home):
    # Absolute `instructions` entries in the global OpenCode config, such as the harness constitution in
    # the product checkout. OpenCode fails to start when one is denied, whatever the lane's cwd.
    paths = []
    for config in (config_home / "opencode/opencode.json", config_home / "opencode/opencode.jsonc",
                   home / ".opencode/opencode.json", home / ".opencode/opencode.jsonc"):
        try:
            entries = _jsonc_loads(config.read_text(encoding="utf-8")).get("instructions", [])
        except (OSError, ValueError, AttributeError):
            continue
        for entry in entries if isinstance(entries, list) else []:
            if not isinstance(entry, str) or "://" in entry or any(c in entry for c in "*?[{"):
                continue
            path = Path(entry).expanduser()
            if not path.is_absolute():
                continue
            for candidate in dict.fromkeys([path, path.resolve()]):
                if candidate.is_file() and not credential_path(candidate):
                    paths.append(candidate)
    return list(dict.fromkeys(paths))

def credential_path(path):
    candidate = Path(path).expanduser().resolve()
    home = Path.home().resolve()
    stores = [home / name for name in CREDENTIAL_STORES]
    if any(candidate.is_relative_to(store) or store.is_relative_to(candidate) for store in stores):
        return True
    parts = [part.lower() for part in candidate.parts]
    return (
        bool(set(parts) & {".ssh", ".aws", ".azure", ".gnupg", ".codex", ".claude",
                           ".gemini", ".cursor", ".kiro", ".docker", ".kube", ".netrc", ".npmrc",
                           ".git-credentials", ".pypirc", ".pgpass", ".vault-token",
                           ".password-store"})
        or any(
            part
            in {
                ".env",
                ".env.local",
                ".env.production",
                "credentials.json",
                "auth.json",
                "auth.db",
                "token.json",
                "application_default_credentials.json",
            }
            for part in parts
        )
        or any(
            part == ".config"
            and index + 1 < len(parts)
            and parts[index + 1] in {"gcloud", "gh", "claude", "codex", "openai", "opencode"}
            for index, part in enumerate(parts)
        )
        or any(
            part in {".codex", ".claude"}
            and index + 1 < len(parts)
            and parts[index + 1] in {"auth.json", ".credentials.json"}
            for index, part in enumerate(parts)
        )
    )

def _sandbox_exec_path():
    if sys.platform != "darwin" or os.environ.get("PROVENANT_NO_OS_CONFINEMENT") == "1":
        return None
    path = shutil.which("sandbox-exec")
    return path if path and _sandbox_exec_usable(path) else None

@functools.lru_cache(maxsize=None)
def _sandbox_exec_usable(path):
    # macOS refuses a nested sandbox; a sandboxed caller runs unconfined with a warning.
    try:
        probe = subprocess.run(
            [path, "-p", "(version 1)(allow default)", "/usr/bin/true"],
            capture_output=True, timeout=10, stdin=subprocess.DEVNULL,
        )
    except (OSError, subprocess.SubprocessError):
        return False
    return probe.returncode == 0

def _sbpl_string(path, *, keep_leaf=False, canonical=False):
    # keep_leaf leaves a lane-replaceable final entry unresolved, so a link planted there cannot move the grant.
    # canonical emits a path already resolved and validated, so a link swapped in since cannot move it.
    path = Path(path).expanduser()
    if canonical:
        return _sbpl_quote(str(path))
    value = str(path.parent.resolve() / path.name) if keep_leaf else str(path.resolve())
    return _sbpl_quote(value)

def _sbpl_quote(value):
    return '"' + str(value).replace("\\", "\\\\").replace('"', '\\"') + '"'

def _sbpl_filter(path, *, keep_leaf=False, canonical=False):
    text = str(path)
    if text.endswith("*"):
        pattern = text[:-1]
        pattern = _sbpl_regex_escape(pattern)
        return '(regex #"^' + pattern + '[^/]*$")'
    return "(subpath " + _sbpl_string(path, keep_leaf=keep_leaf, canonical=canonical) + ")"

def _sbpl_expand_counts(pattern: str) -> str:
    """Spell out `[class]{n}`: Seatbelt regex has no counted repetition and silently matches nothing."""
    return re.sub(r"(\[[^\]]+\])\{(\d+)\}", lambda match: match.group(1) * int(match.group(2)), pattern)

def _sbpl_regex_escape(value):
    regex_metacharacters = set(r'.^$*+?()[]{}|\\"')
    return "".join("\\" + char if char in regex_metacharacters else char for char in value)

def _sbpl_rule(action, operations, paths, *, literal=False, keep_leaf=False, canonical=False):
    filters = (('(literal ' + _sbpl_string(path, keep_leaf=keep_leaf, canonical=canonical) + ')') if literal
               else _sbpl_filter(path, keep_leaf=keep_leaf, canonical=canonical) for path in paths)
    return f"({action} {operations} " + " ".join(filters) + ")\n" if paths else ""

def _sbpl_named_rule(action, operation, names, *, selector="global-name"):
    return f"({action} {operation} " + " ".join(
        f"({selector} {_sbpl_quote(name)})" for name in names
    ) + ")\n" if names else ""

def _sbpl_unix_socket_allow(roots):
    remotes = ['(remote unix-socket (path-literal "/private/var/run/mDNSResponder"))']
    remotes.extend(
        "(remote unix-socket (subpath " + _sbpl_string(root) + "))"
        for root in dict.fromkeys(roots)
    )
    return "(allow network-outbound " + " ".join(remotes) + ")\n"

def _git_toplevel(path):
    result = subprocess.run(["git", "-C", str(path), "rev-parse", "--show-toplevel"],
                            capture_output=True, text=True, timeout=5)
    return Path(result.stdout.strip()).resolve() if result.returncode == 0 and result.stdout.strip() else None

def protected_paths(workspace_root, cwd=None, worktree=None, read_roots=()):
    workspace = Path(workspace_root).expanduser().resolve()
    owners = [workspace]
    roots = [_git_toplevel(path) for path in (workspace, cwd, worktree, *read_roots) if path is not None]
    if roots[0] is None:
        for child in sorted(workspace.iterdir()):
            if child.is_dir() and _git_toplevel(child) == child.resolve():
                roots.append(child.resolve())
    owners.extend(root for root in roots if root is not None and root not in owners)
    protected = []
    worktrees = {}
    for owner in owners:
        declaration = owner / ".agents/fabric-policy.json"
        if not declaration.is_file():
            continue
        try:
            policy = json.loads(declaration.read_text())
            paths = policy.get("protected_paths", []) if isinstance(policy, dict) else None
            if not isinstance(paths, list) or any(not isinstance(p, str) or not p or
                                                 Path(p).is_absolute() or ".." in Path(p).parts or
                                                 any(char in p for char in "*?[]") for p in paths):
                raise ValueError("invalid protected paths")
        except (OSError, KeyError, TypeError, ValueError) as exc:
            raise ValueError(f"invalid protected path policy: {declaration}") from exc
        for relative in paths:
            resolved = (owner / relative).resolve()
            existing = resolved
            while not existing.is_dir() and existing != existing.parent:
                existing = existing.parent
            repo = _git_toplevel(existing)
            if repo is None or not resolved.is_relative_to(repo):
                protected.append(resolved)
                continue
            if repo not in worktrees:
                listing = subprocess.run(["git", "-C", str(repo), "worktree", "list", "--porcelain"],
                                         capture_output=True, text=True, timeout=5)
                if listing.returncode != 0:
                    raise ValueError("cannot list worktrees for protected path policy")
                worktrees[repo] = [Path(line[9:]).resolve() for line in listing.stdout.splitlines()
                                   if line.startswith("worktree ")]
            protected.extend(root / resolved.relative_to(repo) for root in worktrees[repo])
    return list(dict.fromkeys(protected))

def check_protected_inputs(route, workspace_root, cwd, *, worktree=None, prompt_file=None, add_dirs=(), read_roots=()):
    if route.get("trains_on_prompts") is False:
        return []
    workspace = Path(workspace_root).expanduser().resolve()
    paths = protected_paths(workspace, cwd, worktree, read_roots)
    if not paths:
        return []
    for candidate, reject_parent in ((prompt_file, True), (cwd, False), *((path, True) for path in add_dirs)):
        if candidate is None:
            continue
        item = Path(candidate).expanduser()
        item = (item if item.is_absolute() else workspace / item).resolve()
        for protected in paths:
            if item.is_relative_to(protected) or (reject_parent and protected.is_relative_to(item)):
                raise ValueError(f"protected path {protected}; fix: use a non-training route")
    return paths

def _real_dir_chain(locks):
    """True when .agent-run and locks are real directories, judged without following links."""
    try:
        return all(stat.S_ISDIR(path.lstat().st_mode) for path in (locks.parent, locks))
    except OSError:
        return False

def codex_writer_shared_paths(cwd, safe_path):
    """Only nested runs and Fabric's SQLite files, checked like explicit add_dirs."""
    state = Path(os.environ.get("AGENT_FABRIC_STATE_DIRECTORY",
                                Path.home() / ".local/state/agent-harness/fabric"))
    paths = {}
    for root, names in ((layout.run_root(cwd) / ".agent-run/runs", ("",)),
                        (state, tuple("fabric.sqlite3" + suffix for suffix in ("", "-wal", "-shm", "-journal")))):
        if names == ("",) and root != root.resolve():
            continue  # A runs link must not turn into a grant for sessions or other state.
        root = (Path(cwd) / root).resolve()
        if safe_path(str(root)):
            for name in names:
                path = str(root / name)
                if safe_path(path) and (not name or not os.path.lexists(path) or _plain_file(Path(path))):
                    paths[path] = bool(name)  # Files use literal SBPL grants; runs use subpath.
    return paths

def os_confinement_profile(plan, search_path=None):
    """Apply the provider's read and write boundary to this attempt.

    Seatbelt checks file-write* on both ends of a hard link, so the write rules also bound `ln`.
    SBPL applies the last matching rule, so each later rule narrows or widens the one before.
    """
    root = Path(plan.get("workspace_root") or plan["cwd"]).expanduser().resolve()
    home = Path.home().resolve()
    state = _provider_state(plan)
    add_dirs = [Path(path) for path in plan.get("applied", {}).get("add_dirs", [])]
    run_dir = Path(plan["run_dir"]) if plan.get("run_dir") else None
    browser_tmp = plan.get("applied", {}).get("browser_tmpdir")
    attempt_paths = [*([run_dir] if run_dir else []), *([Path(browser_tmp)] if browser_tmp else [])]
    state_writes = _state_paths(home, state.get("read_write", ()))
    cwd_path = Path(plan["cwd"]).resolve()
    discovery_metadata = [ancestor / name for ancestor in (cwd_path, *cwd_path.parents)
                          for name in state.get("ancestor_metadata", ())]
    # Discovery's realPath opens each of these directories, not only lstats them, and a live lane still died with
    # EPERM on metadata alone (#1012). Grant the directory itself (names, never children) after protected-path
    # denies, literal and without following a leaf symlink, so descendants stay unreadable.
    metadata_grant = _sbpl_rule("allow", "file-read-data file-read-metadata", discovery_metadata,
                                literal=True, keep_leaf=True)
    if plan.get("mode") == "worktree_write":
        cwd = Path(plan["cwd"])
        # Only an owner-recorded linked worktree gets Git grants; any other Git directory in cwd
        # is already writable as part of it.
        git_denied, git_allowed, literal_files, read_only = [], [], [], []
        if plan.get("trusted_git"):
            location = owner_location(cwd, common=plan["trusted_git"]["common"])
            location.validate()
            git_denied = [location.common]
            git_allowed, literal_files = writer_git_writes(location.private, location.common, plan.get("lane_ref"))
            read_only = writer_read_only(cwd, location.private, location.common)
        allowed = [cwd, *add_dirs, *attempt_paths, Path("/dev")]
        shared_paths = plan["applied"].get("shared_write_paths", {})
        locks_dir = plan.get("applied", {}).get("locks_dir")
        if locks_dir and _real_dir_chain(Path(locks_dir)):
            allowed.append(Path(locks_dir))
        capabilities = plan.get("applied", {}).get("capabilities", [])
        profile = (
            "(version 1)\n(allow default)\n(deny file-write*)\n"
            + (_sbpl_rule("allow", "file-read-data", [state["resolved_home"]])
               if plan.get("adapter") == "claude" else "")
            + _sbpl_rule("allow", "file-write*", allowed)
            + "".join(rule for rule, _ in _state_write_grants(home, state, ("read_write",)))
            + _sbpl_rule("deny", "file-write*", git_denied)
            + _sbpl_rule("allow", "file-write*", git_allowed, keep_leaf=True)
            # Owner-resolved lexical paths: a link planted in logs/ or refs/ cannot move these grants.
            + _sbpl_rule("allow", "file-write*", literal_files, literal=True, canonical=True)
        )
        for path, literal in shared_paths.items():
            profile += _sbpl_rule("allow", "file-write*", [Path(path)], literal=literal, keep_leaf=True)
        profile += _state_write_guards(home, {"no_links": [path for path, literal in shared_paths.items() if literal]})
        profile += _sbpl_rule("deny", "file-write*", read_only, keep_leaf=True)
        if plan.get("adapter") == "codex" and capabilities:
            codex_home = Path(plan["codex_home"])
            auth_path = Path(plan["codex_auth_path"])
            profile += _sbpl_rule("allow", "file-write*", [codex_home])
            # Quote without resolving, and allow in-place rewrites only, so the lane cannot swap the
            # entry for a link that would move the next attempt's grant.
            profile += f"(allow file-write* (literal {_sbpl_quote(auth_path)}))\n"
            profile += f"(deny file-write-create file-write-unlink (literal {_sbpl_quote(auth_path)}))\n"
            profile += "(deny mach-lookup)\n"
            profile += _sbpl_named_rule("allow", "mach-lookup", CODEX_BASE_MACH_SERVICES)
            profile += ("(deny mach-register)\n(deny signal)\n(allow signal (target same-sandbox))\n"
                        "(deny ipc-sysv*)\n(deny user-preference-write)\n")
            if "postgres" in capabilities:
                profile += "(allow " + " ".join(CODEX_POSTGRES_IPC_OPERATIONS) + ")\n"
            if "browser" in capabilities:
                profile += _sbpl_named_rule("allow", "mach-lookup", CODEX_BROWSER_MACH_SERVICES)
                profile += _sbpl_named_rule("allow", "mach-lookup", CODEX_BROWSER_MACH_PORT_PREFIXES,
                                            selector="global-name-prefix")
                profile += _sbpl_named_rule("allow", "mach-register", CODEX_BROWSER_MACH_PORT_PREFIXES,
                                            selector="global-name-prefix")
            profile += "(deny network-outbound (remote unix-socket))\n"
            profile += _sbpl_unix_socket_allow(
                [cwd, *add_dirs, *attempt_paths, codex_home]
            )
        profile += _state_write_guards(home, state)
        profile += _provider_config_write_denies(home)
        profile += _sbpl_rule("deny", "file-write*", plan.get("protected_paths", []))
        profile += _sbpl_rule("deny", "file-read*", plan.get("protected_paths", []))
        return profile + metadata_grant + _claude_account_denies(plan)
    configured_xdg = Path(os.environ.get("XDG_CONFIG_HOME", "")).expanduser()
    xdg_config_home = configured_xdg if configured_xdg.is_absolute() else home / ".config"
    git_config_files = [home / ".gitconfig", xdg_config_home / "git/config"]
    reads = [*state.get("read", ()), *(() if _uses_api_key(plan) else state.get("oauth_read", ()))]
    project_files, project_directories = _project_config_reads(plan, root)
    discovery_dirs, discovery_files = [], []
    if plan.get("adapter") == "opencode":
        # OpenCode v2 resolves every ancestor of its cwd, home, ~/.claude and ~/.agents at startup and reads the
        # instruction files it finds there. Home stays denied; re-allow those directories as listings
        # (never their contents) and the instruction files by name.
        cwd_path = Path(plan["cwd"]).resolve()
        # Home is listed even when the cwd sits outside it, as in a /tmp worktree.
        discovery_dirs = [*dict.fromkeys([*cwd_path.parents, home]), home / ".claude", home / ".agents"]
        discovery_files = [d / name for d in cwd_path.parents for name in OPENCODE_INSTRUCTION_FILES]
        discovery_files += [home / ".agents/AGENTS.md", home / ".claude/CLAUDE.md"]
        discovery_dirs = [d for d in discovery_dirs if _opencode_discovery_grantable(d, directory=True)]
        discovery_files = [f for f in discovery_files if _opencode_discovery_grantable(f)]
        configured = _opencode_configured_instructions(home, xdg_config_home)
        discovery_files += configured
        discovery_dirs += [parent for path in configured for parent in path.parents]
    toolchain_files, uv_project_files, toolchain_directories = _toolchain_reads(plan, root, search_path)
    launcher_links, install_directories = _adapter_binary_reads(plan, search_path)
    return (
        "(version 1)\n(allow default)\n(deny file-write*)\n"
        + _sbpl_rule("allow", "file-write*", [*attempt_paths, Path("/dev")])
        + "".join(rule for rule, _ in _state_write_grants(home, state, ("read_write",)))
        + _state_write_guards(home, state)
        + _sbpl_rule("deny", "file-read-data", [home, Path("/private/tmp")])
        + _sbpl_rule("deny", "file-read-data", [root, *add_dirs])
        + _sbpl_rule("allow", "file-read-data", [*discovery_dirs, *discovery_files], literal=True)
        + _sbpl_rule("allow", "file-read-data", attempt_paths)
        + _sbpl_rule("allow", "file-read-data", state_writes, canonical=True)
        + _sbpl_rule("allow", "file-read-data", _state_read_paths(home, reads))
        + _sbpl_rule("allow", "file-read-data", [home / path for path in state.get("read_literal", ())],
                     literal=True, keep_leaf=True)
        + _sbpl_rule("allow", "file-read-data", git_config_files, literal=True)
        + _sbpl_rule("allow", "file-read-data", [
            Path(plan["cwd"]), *add_dirs
        ])
        # Unresolved, so a link planted under a config name cannot carry the grant elsewhere.
        + _sbpl_rule("allow", "file-read-data", project_files, literal=True, keep_leaf=True)
        + _sbpl_rule("allow", "file-read-data", project_directories, keep_leaf=True)
        + _sbpl_rule("allow", "file-read-data", toolchain_files, literal=True, canonical=True)
        + _sbpl_rule("allow", "file-read-data", uv_project_files, literal=True, keep_leaf=True)
        + _sbpl_rule("allow", "file-read-data", toolchain_directories, canonical=True)
        + _sbpl_rule("allow", "file-read-data", launcher_links, literal=True, keep_leaf=True)
        + _sbpl_rule("allow", "file-read-data", install_directories, canonical=True)
        # After every allow, since a state grant such as ~/.claude would otherwise reopen them.
        + _sbpl_rule("deny", "file-read-data", [home / path for path in
                                                (*EXTRA_DENIED_READS, *state.get("read_denied", ()))])
        + (_sbpl_rule('deny', 'file-read-data', [state['resolved_home'] / 'projects'])
           + _claude_session_reads(home, plan) if plan.get("adapter") == "claude" else "")
        + _provider_config_write_denies(home)
        + _sbpl_rule("deny", "file-read*", plan.get("protected_paths", []))
        + metadata_grant
        + _claude_account_denies(plan)
    )

def _claude_account_denies(plan):
    """Apply after broad workspace grants, excluding only the selected subtree."""
    if plan.get('adapter') != 'claude':
        return ''
    selected = Path(plan.get('provider_home') or Path.home() / '.claude').resolve()
    rules = []
    if selected != (Path.home() / '.claude').resolve():
        # Also deny broad workspace/writer reads of the ambient startup config.
        startup = Path.home() / '.claude.json'
        rules.append(_sbpl_rule('deny', 'file-read*', [startup, startup.resolve()]))
    for path in plan.get('claude_account_dirs', []):
        other = Path(path)
        if other == selected:
            continue
        if selected.is_relative_to(other):
            rules.append('(deny file-read* file-write* (require-all '
                         f'(subpath {_sbpl_quote(other)}) '
                         f'(require-not (subpath {_sbpl_quote(selected)}))))\n')
        else:
            rules.append(_sbpl_rule('deny', 'file-read* file-write*', [other]))
    return ''.join(rules)

def _provider_state(plan):
    """Resolve the provider home once, sharing it between preparation and every state rule.

    A user's home-level dotfiles link is supported; mutable entries below it still cannot move
    a grant through a link. Keep regexes relative to this same canonical provider home.
    """
    state = dict(CONFINED_STATE.get(plan.get("adapter"), {}))
    relative = state.get("home")
    if relative:
        root = Path(plan.setdefault("provider_home", str((Path.home() / relative).resolve())))
        default = Path.home().resolve() / relative
        claude_child = (plan.get('adapter') == 'claude' and root.is_relative_to(default)
                        and not credential_path(Path('/provenant-identity') / root.relative_to(default)))
        if root != default and credential_path(root) and not claude_child:
            raise PermissionError(f"provider home resolves into a credential store: {root}")
        state["resolved_home"] = root
        if plan.get("adapter") == "claude" and root != default.resolve():
            state["read"] = tuple(entry for entry in state.get("read", ()) if entry != ".claude.json")
        for key in ("read_write", "write_literal", "write_in_place", "read", "oauth_read",
                    "read_literal", "read_denied", "no_links", "no_symlinks"):
            state[key] = tuple(str(root / Path(entry).relative_to(relative))
                               if Path(entry).is_relative_to(relative) else entry
                               for entry in state.get(key, ()))
    return state

def _state_paths(home, entries):
    """Writable provider state, resolved once for a canonical grant under its own name. A lane can
    replace such an entry, so a link planted there fails the launch rather than moving the next
    attempt's grant to the link's target. A "*" entry stays a pattern and is never resolved."""
    paths = []
    for entry in entries:
        path = home / entry
        if not entry.endswith("*"):
            if path.is_symlink():
                raise PermissionError(f"provider state must not be a symlink: {path}; "
                                      "fix: replace the link with the file or directory it points to")
            path = path.parent.resolve() / path.name
        paths.append(path)
    return paths

def _state_read_paths(home, entries):
    """Readable provider state. The grant follows a link, so a link into a credential store, such as
    ~/.opencode pointing at ~/.ssh, is skipped rather than reopening that store."""
    paths = [home / entry for entry in entries]
    return [path for path in paths if not (path.is_symlink() and credential_path(path.resolve()))]

def _provider_config_write_denies(home):
    paths = []
    for relative in READ_ONLY_PROVIDER_CONFIG:
        path = home / relative
        # Deny both the link name (including replacement siblings) and its normal read target.
        paths.extend((str(path) + "*", str(path.resolve()) + "*"))
    return _sbpl_rule("deny", "file-write*", dict.fromkeys(paths))

def _state_write_grants(home, state, keys=("read_write", "write_literal", "write_in_place", "write_regex")):
    """The profile rules and receipt descriptions of the same canonical provider grants."""
    grants = []
    for key in keys:
        if key == "write_regex":
            root = state.get("resolved_home", home)
            paths = ["^" + _sbpl_regex_escape(str(root)) + "/" + _sbpl_expand_counts(pattern) + "$"
                     for pattern in state.get(key, ())]
        else:
            paths = _state_paths(home, state.get(key, ()))
            if key == "write_literal" and state.get("atomic_siblings"):
                paths = [*paths, *(str(path) + "*" for path in paths)]
        for path in paths:
            value = str(path)
            regex = key == "write_regex" or value.endswith("*")
            if key != "write_regex" and value.endswith("*"):
                value = "^" + _sbpl_regex_escape(value[:-1]) + "[^/]*$"
            if regex:
                rule = f'(allow file-write* (regex #"{value}"))\n'
                description = "regex:" + value
            else:
                rule = _sbpl_rule("allow", "file-write*", [path], literal=key != "read_write", canonical=True)
                description = value
            grants.append((rule, description))
    return grants

def _state_write_guards(home, state):
    """Rewrite-in-place grants, and no links a lane could plant for the provider's unconfined CLI."""
    in_place = _state_paths(home, state.get("write_in_place", ()))
    rules = "".join(rule for rule, _ in _state_write_grants(
        home, state, ("write_literal", "write_in_place", "write_regex")))
    rules += _sbpl_rule("deny", "file-write-create file-write-unlink", in_place, literal=True, canonical=True)
    for path in state.get("no_links", ()):
        # Nor a directory or FIFO at a file grant, which would hold or block what the CLI opens.
        rules += (f"(deny file-write-create (require-all {_sbpl_filter(home / path)} (require-any "
                  "(vnode-type SYMLINK) (vnode-type DIRECTORY) (vnode-type FIFO))))\n")
    for path in state.get("no_symlinks", ()):
        # Session/cache trees need new directories, unlike the fixed-file Kiro support grants.
        rules += f"(deny file-write-create (require-all {_sbpl_filter(home / path)} (vnode-type SYMLINK)))\n"
    return rules

def _claude_project_dir(home, cwd):
    """Claude Code's transcript directory for cwd, named as Claude names it from the resolved path."""
    encoded = str(Path(cwd).resolve()).encode("utf-16-le", "surrogatepass")
    units = [int.from_bytes(encoded[index:index + 2], "little") for index in range(0, len(encoded), 2)]
    name = "".join(chr(unit) if unit < 128 and chr(unit).isalnum() else "-" for unit in units)
    if len(name) > 200:
        # Claude keeps 200 characters and appends abs() of JavaScript's 32-bit string hash in base 36.
        value = 0
        for unit in units:
            value = (value * 31 + unit) & 0xFFFFFFFF
        value = abs(value - (1 << 32) if value >= 1 << 31 else value)
        digits = ""
        while True:
            value, remainder = divmod(value, 36)
            digits = "0123456789abcdefghijklmnopqrstuvwxyz"[remainder] + digits
            if not value:
                break
        name = name[:200] + "-" + digits
    return Path(home) / ".claude/projects" / name

CLAUDE_SESSION_ID = r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}"

def _claude_session_reads(home, plan):
    """A Claude lane may read back its own session's transcript, which it needs to resume."""
    sessions = [session for session in dict.fromkeys((plan.get("resume_session"), plan.get("session_id")))
                if session and re.fullmatch(CLAUDE_SESSION_ID, str(session))]
    if not sessions:
        return ""
    root = _provider_state(plan).get("resolved_home", home / ".claude")
    directory = root / "projects" / _claude_project_dir(home, plan["cwd"]).name
    return "(allow file-read-data " + " ".join(
        f"(literal {_sbpl_quote(directory / (session + '.jsonl'))}) (subpath {_sbpl_quote(directory / session)})"
        for session in sessions
    ) + ")\n"

def _uses_api_key(plan):
    """Whether a Claude lane signs in with a key (bare or endpoint route) rather than OAuth."""
    return plan.get("adapter") == "claude" and claude_adapter.uses_api_key(plan.get("route") or {})

PROJECT_CONFIG = {"opencode": (("opencode.json", "opencode.jsonc", "AGENTS.md", "CLAUDE.md"), (".opencode",))}

def _project_config_reads(plan, root):
    """What a read-only provider loads between its cwd and the repository (or workspace) root.

    OpenCode fails at startup when it can see but not read a config file above its cwd.
    """
    files, directories = PROJECT_CONFIG.get(plan.get("adapter"), ((), ()))
    if not files and not directories:
        return [], []
    cwd = Path(plan["cwd"]).resolve()
    stop = _git_toplevel(cwd) or root
    parents = [parent for parent in cwd.parents if parent.is_relative_to(stop)]
    return ([parent / name for parent in parents for name in files],
            [parent / name for parent in parents for name in directories])
