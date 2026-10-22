"""Fabric v1 rows, terse digests and locked durable terminal indexes."""

from contextlib import contextmanager
from datetime import UTC, datetime, timedelta
import fcntl
import hashlib
import importlib.util
import json
import math
import os
import re
from pathlib import Path
import secrets
import sys

try:
    from .context_usage import marker as context_marker
    from .layout import run_root
    from .output_custody import open_parent
except ImportError:
    from context_usage import marker as context_marker
    from layout import run_root
    from output_custody import open_parent

TERMINAL_STATUSES = {
    "ok",
    "partial",
    "quarantined",
    "failed",
    "usage_limited",
    "rate_limited",
    "auth_required",
    "model_unavailable",
    "permission_blocked",
    "stalled",
    "startup_timeout",
    "writer_changed",
    "timed_out",
    "cancelled",
    "interrupted",
    "lost",
    "rejected",
    "tool_missing",
    "input_required",
}


def timestamp(value):
    return value.astimezone(UTC).isoformat().replace("+00:00", "Z")


def parse_time(value):
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


def render_digest(row):
    run_id = row.get("run_id", "")
    state = row.get("state", "terminal")
    status = row.get("status") or state
    prov = row.get("provenance") or {}
    requested = prov.get("requested") or {}
    route = (
        requested.get("adapter", "")
        + "/"
        + (prov.get("observed_model") or prov.get("resolved_model") or "?")
    )
    effort = prov.get("effort_applied")
    if effort:
        route += "@" + effort
    if status == "rejected":
        return (
            "rejected "
            + row.get("error", "preflight")
            + " · fix: "
            + (row.get("fix") or "inspect receipt")
        )
    if status == "input_required":
        question = row.get("question") or ""
        if isinstance(question, dict):
            question = question.get("prompt", "")
        return (
            f"input_required {run_id} "
            + json.dumps(" ".join(question.split())[:200], ensure_ascii=False)
            + f' · reply: fabric_dispatch{{resume:"{run_id}",prompt:"…"}}'
        )
    if state != "terminal":
        if state == "queued":
            return f"queued {run_id} {route} · {row.get('reason') or 'waiting to start'}"
        fallback = prov.get("fallback_from")
        if fallback:
            previous = fallback.get("route", "")
            reset = f" (resets {fallback['reset_at']})" if fallback.get("reset_at") else ""
            return f"running {run_id} attempt {row['attempt']}: {previous} {fallback['status']}{reset} → {route}"
        access = (
            ("write " + str(row.get("worktree") or row.get("cwd")))
            if row.get("mode") == "worktree_write"
            else "read"
        )
        return f'running {run_id} {route} {access} · fabric_status{{ids:["{run_id}"],wait_seconds:55}}'
    try:
        duration = max(
            0,
            round(
                (
                    parse_time(row["ended_at"]) - parse_time(row["started_at"])
                ).total_seconds()
            ),
        )
    except (KeyError, TypeError, ValueError):
        duration = 0
    result = row.get("paths", {}).get("result")
    if result and row.get("run_dir"):
        result = str(Path(row["run_dir"]) / result)
    detail = " · result " + result if result else ""
    if status == "quarantined":
        quarantine = row.get("quarantine") or {}
        detail += " · not landable; review quarantine: " + str(quarantine.get("reason") or "inspect receipt")
        detail += "; patches: " + (", ".join(quarantine.get("patches") or []) or "unavailable")
        if quarantine.get("patch_error"):
            detail += "; " + str(quarantine["patch_error"])
    elif status not in {"ok", "partial"}:
        excerpt = row.get("evidence", {}).get("excerpt") or ""
        excerpt_line = next(
            (" ".join(line.split())[:120] for line in excerpt.splitlines() if line.strip()),
            "",
        ) if isinstance(excerpt, str) else ""
        detail = " · " + (
            row.get("fix")
            or row.get("evidence", {}).get("signature")
            or excerpt_line
            or "inspect stderr"
        )
        if row.get("reset_at"):
            detail += " (resets " + row["reset_at"] + ")"
    text = f"{status} {run_id} {route} {duration}s" + detail
    if prov.get("line"):
        text += "\n  " + prov["line"] + context_marker(row.get("context"))
    warnings = [str(item) for item in row.get("warnings") or [] if item]
    if warnings:
        # Warn, don't block: the caller sees what was substituted or skipped.
        text += "\n  ! " + "; ".join(dict.fromkeys(warnings))[:200]
    return text


def cooldown_path():
    if os.environ.get("FABRIC_COOLDOWNS_PATH"):
        return Path(os.environ["FABRIC_COOLDOWNS_PATH"])
    if os.environ.get("AGENT_FABRIC_STATE_ROOT"):
        return Path(os.environ["AGENT_FABRIC_STATE_ROOT"]) / "cooldowns.json"
    return Path.home() / ".local/state/agent-harness/fabric/cooldowns.json"


def route_health_path():
    if os.environ.get("AGENT_FABRIC_ROUTE_HEALTH_PATH"):
        return Path(os.environ["AGENT_FABRIC_ROUTE_HEALTH_PATH"])
    if os.environ.get("AGENT_FABRIC_STATE_ROOT"):
        return Path(os.environ["AGENT_FABRIC_STATE_ROOT"]) / "route-health.json"
    return Path.home() / ".local/state/agent-harness/fabric/route-health.json"


def store_parent(path):
    """Create missing parents through directory descriptors; refuse redirected stores."""
    try:
        return open_parent(str(path))
    except FileNotFoundError:
        parent, leaf = store_parent(Path(path).parent)
        try:
            try:
                os.mkdir(leaf, 0o700, dir_fd=parent)
            except FileExistsError:
                pass
        finally:
            os.close(parent)
        return open_parent(str(path))


def store_open(path, flags, create_parents=False):
    parent, leaf = store_parent(path) if create_parents else open_parent(str(path))
    try:
        return os.open(leaf, flags | getattr(os, "O_NOFOLLOW", 0), 0o600, dir_fd=parent)
    finally:
        os.close(parent)


@contextmanager
def locked(path):
    path = Path(path)
    fd = store_open(path, os.O_RDWR | os.O_CREAT, create_parents=True)
    try:
        if os.fstat(fd).st_nlink != 1:
            raise ValueError("lock must not be hard-linked")
        fcntl.flock(fd, fcntl.LOCK_EX)
        yield
    finally:
        fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)


def identity_resolver():
    """Load the product's shared non-secret identity owner, including in direct CLI runs."""
    module = sys.modules.get('provider_identity')
    if module is None:
        product = Path(__file__).resolve().parents[3]
        spec = importlib.util.spec_from_file_location('provider_identity', product / 'scripts/provider_identity.py')
        module = importlib.util.module_from_spec(spec)
        sys.modules['provider_identity'] = module
        spec.loader.exec_module(module)
    return module


_PACING_CACHE = {}
_ROUTE_MODULES = {}


def _route_module():
    """Load model_route once per product and instance root, not on every policy read."""
    try:
        from .exec_routing import _model_route_module
    except ImportError:
        from exec_routing import _model_route_module
    key = (os.environ.get("AGENT_FABRIC_PRODUCT_ROOT"), os.environ.get("AGENT_FABRIC_INSTANCE_ROOT"))
    if key not in _ROUTE_MODULES:
        _ROUTE_MODULES.clear()
        _ROUTE_MODULES[key] = _model_route_module()
    return _ROUTE_MODULES[key]


def _catalogue_stamp():
    """Identify the catalogue files pacing_policy reads, using the router's own paths."""
    route = _route_module()
    stamp = []
    for path in (route.PRODUCT_CATALOG_PATH, route.CATALOG_PATH):
        try:
            info = path.stat()
            stamp.append((str(path), info.st_mtime_ns, info.st_size))
        except OSError:
            stamp.append((str(path), None, None))
    return tuple(stamp)


def pacing_policy():
    """Read pacing knobs through the existing routing catalogue and its overlay.

    Cached per process until either catalogue file changes, so hot paths and the
    cooldowns lock do not re-execute the routing module on every call.
    """
    stamp = _catalogue_stamp()
    cached = _PACING_CACHE.get(stamp)
    if cached is None:
        _PACING_CACHE.clear()
        cached = _PACING_CACHE[stamp] = _read_pacing_policy()
    return dict(cached)


def _read_pacing_policy():
    defaults = {"base_delay_seconds": 30, "cap_seconds": 900,
                "jitter_fraction": 0.2, "minimum_budget_seconds": 30}
    configured = _route_module().load_catalog().get("rate_limit_pacing", {})
    for name in defaults:
        value = configured.get(name) if isinstance(configured, dict) else None
        lower = 0 if name == "jitter_fraction" else 0.001
        if (isinstance(value, (int, float)) and not isinstance(value, bool)
                and math.isfinite(value) and value >= lower
                and (name != "jitter_fraction" or value <= 1)):
            defaults[name] = value
    defaults["cap_seconds"] = max(defaults["base_delay_seconds"], defaults["cap_seconds"])
    return defaults


def cooldown_key(adapter, model, scope):
    # An opaque, collision-free storage key; callers see adapter/model projections.
    return json.dumps([adapter, scope.get('account'), scope.get('host'), model], separators=(',', ':'))


def _load_cooldowns(path, at, *, persist=True):
    """Read scopes and backoff history; persist migration/pruning only under the lock."""
    if path.is_symlink():
        raise ValueError('cooldown store must not be a symlink')
    try:
        with os.fdopen(store_open(path, os.O_RDONLY)) as stream:
            document = json.load(stream)
        entries = document.get('cooldowns', document)
        if not isinstance(entries, dict):
            return {}
    except (FileNotFoundError, ValueError, TypeError, AttributeError):
        return {}
    values = {}
    resolver = identity_resolver()
    for key, entry in entries.items():
        if not isinstance(entry, dict):
            continue
        try:
            if parse_time(entry['cooling_until']) <= at and not (
                    entry.get('status') == 'rate_limited' and entry.get('backoff_seconds')):
                continue
        except (ValueError, TypeError, KeyError, AttributeError):
            continue
        adapter, _, model = str(key).partition('/')
        entry = {**entry, 'adapter': entry.get('adapter', adapter), 'model': entry.get('model', model)}
        if 'host' not in entry:
            # Only old writers omitted host. New unknown scopes retain explicit nulls.
            legacy = resolver.scope(entry['adapter'], default=True)
            entry['host'] = legacy['host']
            if entry.get('account') is None:
                entry['account'] = legacy['account']
        # This machine's earlier ids (raw hostname, its slug, 'local') key as the current id, so a
        # success clears and a repeat limit doubles the same entry. Peer ids are never aliases.
        entry['host'] = resolver.same_host(entry['host'])
        scoped = cooldown_key(entry['adapter'], entry['model'], entry)
        previous = values.get(scoped)
        if previous is None or parse_time(entry['cooling_until']) > parse_time(previous['cooling_until']):
            values[scoped] = entry
    if persist and values != entries:
        _store_cooldowns(path, values)
    return values


def active_cooldowns(values, at=None):
    """Project only the current account and host of each adapter to adapter/model."""
    at = at or datetime.now(UTC)
    resolver = identity_resolver()
    scopes = {}
    active = {}
    for entry in values.values():
        if parse_time(entry['cooling_until']) <= at:
            continue
        adapter = entry['adapter']
        if adapter not in scopes:
            scopes[adapter] = resolver.scope(adapter)
        if resolver.matches(entry, scopes[adapter]):
            active[adapter + '/' + entry['model']] = entry
    return active


def read_cooldowns(path=None, at=None):
    at = at or datetime.now(UTC)
    path = Path(path or cooldown_path())
    try:
        if not path.exists():
            return {}
        try:
            with locked(path.with_name('cooldowns.lock')):
                return active_cooldowns(_load_cooldowns(path, at), at)
        except (OSError, ValueError):  # ValueError: lock refused (hard-linked); the store may still be readable
            return active_cooldowns(_load_cooldowns(path, at, persist=False), at)
    except (OSError, ValueError, TypeError, KeyError, AttributeError):
        return {}


def read_route_health(path=None, *, cooldowns_path=None, at=None):
    """Read recent route outcomes joined with currently active cooldowns."""
    at = at or datetime.now(UTC)
    path = Path(path or route_health_path())
    try:
        if path.is_symlink():
            return {}
        with os.fdopen(store_open(path, os.O_RDONLY)) as stream:
            document = json.load(stream)
        routes = document.get("routes", {}) if document.get("schema") == "fabric.route-health.v1" else {}
        if not isinstance(routes, dict):
            return {}
    except (OSError, ValueError, TypeError, AttributeError):
        routes = {}
    cooldowns = read_cooldowns(cooldowns_path, at)
    result = {}
    for key, route in routes.items():
        if not isinstance(key, str) or not isinstance(route, dict):
            continue
        recent = route.get("recent", [])
        if not isinstance(recent, list):
            recent = []
        current = []
        for item in recent:
            if not isinstance(item, dict):
                continue
            try:
                age = (at - parse_time(item.get("at"))).total_seconds()
            except (ValueError, TypeError):
                continue
            if 0 <= age <= 7 * 86400:
                current.append(item)
        recent = current[:20]
        adapter = str(route.get("adapter", ""))
        model = str(route.get("model", ""))
        task_class = str(route.get("task_class", ""))
        cooldown = cooldowns.get(f"{adapter}/{model}") or cooldowns.get(f"{adapter}/*") or {}
        result[key] = {
            "adapter": adapter,
            "model": model,
            "task_class": task_class,
            "recent": recent,
            "recent_failures": sum(item.get("status") in {"failed", "empty_output"} for item in recent),
            "rate_limits": sum(item.get("status") in {"rate_limited", "usage_limited"} for item in recent),
            "cooling_until": cooldown.get("cooling_until", "") if isinstance(cooldown, dict) else "",
        }
    return result


def write_route_health(row, *, path=None, at=None):
    """Append one bounded terminal outcome for an adapter/model/task-class route."""
    try:
        from .exec_routing import registered_model
    except ImportError:
        from exec_routing import registered_model
    if row.get("status") == "writer_changed":
        return
    path = Path(path or route_health_path())
    at = at or datetime.now(UTC)
    provenance = row["provenance"]
    adapter = provenance["requested"]["adapter"]
    model = registered_model(adapter, provenance.get("resolved_model") or "*")
    task_class = row.get("task_class") or provenance.get("requested", {}).get("task_class") or "ordinary"
    status = row.get("status", "failed")
    if status == "failed" and row.get("evidence", {}).get("signature") == "empty_output":
        status = "empty_output"
    if status not in {"ok", "quarantined", "failed", "empty_output", "cancelled", "rate_limited", "usage_limited", "permission_blocked"}:
        status = "failed"
    key = f"{adapter}|{model}|{task_class}"
    entry = {"status": status, "task_class": task_class, "at": timestamp(at), "run_id": row.get("run_id", "")}
    if status == "ok" and row.get("evidence", {}).get("signature") == "exit_after_output":
        entry["warning"] = "exit_after_output"
    with locked(path.with_name("route-health.lock")):
        try:
            if path.is_symlink():
                raise ValueError("route health store must not be a symlink")
            with os.fdopen(store_open(path, os.O_RDONLY)) as stream:
                document = json.load(stream)
            routes = document.get("routes", {}) if document.get("schema") == "fabric.route-health.v1" else {}
            if not isinstance(routes, dict):
                routes = {}
        except FileNotFoundError:
            routes = {}
        except (OSError, ValueError, TypeError, AttributeError):
            routes = {}
        previous = routes.get(key, {})
        recent = previous.get("recent", []) if isinstance(previous, dict) else []
        recent = [item for item in recent if isinstance(item, dict)]
        routes[key] = {"adapter": adapter, "model": model, "task_class": task_class,
                       "recent": [entry, *recent][:20]}
        parent, leaf = store_parent(path)
        temp = ".route-health-" + secrets.token_hex(12)
        fd = os.open(temp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600, dir_fd=parent)
        try:
            with os.fdopen(fd, "w") as stream:
                json.dump({"schema": "fabric.route-health.v1", "routes": routes}, stream, indent=2, sort_keys=True)
                stream.write("\n")
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temp, leaf, src_dir_fd=parent, dst_dir_fd=parent)
            os.fsync(parent)
        finally:
            try:
                os.unlink(temp, dir_fd=parent)
            except FileNotFoundError:
                pass
            os.close(parent)


def _store_cooldowns(path, values):
    """Atomic write of the cooldown map; callers hold the lock."""
    parent, leaf = store_parent(path)
    temp = ".cooldowns-" + secrets.token_hex(12)
    fd = os.open(temp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600, dir_fd=parent)
    try:
        with os.fdopen(fd, "w") as stream:
            json.dump(
                {"schema": "fabric.cooldowns.v1", "cooldowns": values},
                stream,
                indent=2,
            )
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temp, leaf, src_dir_fd=parent, dst_dir_fd=parent)
        os.fsync(parent)
    finally:
        try:
            os.unlink(temp, dir_fd=parent)
        except FileNotFoundError:
            pass
        os.close(parent)


def clear_cooldowns(selector=None, path=None, *, account=None):
    """Remove cooldowns: all, one adapter (`*` and every model key), or one adapter/model."""
    try:
        from .exec_routing import registered_model
    except ImportError:
        from exec_routing import registered_model
    adapter, slash, model = (selector or "").partition("/")
    if selector is not None and (not adapter.strip() or adapter.startswith("-")):
        raise ValueError("selector must be adapter or adapter/model; omit it to clear every cooldown")
    if account is not None and not identity_resolver().LABEL.fullmatch(account):
        raise ValueError('account must be a short non-secret label')
    path = Path(path or cooldown_path())
    if slash:
        model = registered_model(adapter, model or "*")
    with locked(path.with_name("cooldowns.lock")):
        values = _load_cooldowns(path, datetime.now(UTC))
        resolver = identity_resolver()
        host = resolver.host_id()
        gone = [key for key, entry in values.items()
                if (account is None or (entry.get('account') == account
                                        and resolver.same_host(entry.get('host')) == host))
                and (not selector or (entry['adapter'] == adapter and (not slash or entry['model'] == model)))]
        removed = sorted({values[key]['adapter'] + '/' + values[key]['model'] for key in gone})
        for key in gone:
            del values[key]
        if gone:
            _store_cooldowns(path, values)
    return removed


def write_cooldown(row, *, path=None, at=None):
    if row["status"] not in {"ok", "usage_limited", "rate_limited"}:
        return
    if row.get("evidence", {}).get("signature") == "cooldown_active":
        return
    try:
        from .exec_routing import registered_model
    except ImportError:
        from exec_routing import registered_model
    path = Path(path or cooldown_path())
    at = at or datetime.now(UTC)
    prov = row["provenance"]
    adapter = prov["requested"]["adapter"]
    model = registered_model(adapter, (prov.get("observed_model") if row["status"] == "ok" else None)
                             or prov["resolved_model"] or "*", warnings=row.setdefault("warnings", []))
    excerpt = row.get("evidence", {}).get("excerpt", "")
    # Antigravity and OpenCode (Go or otherwise) meter each model separately; a limit there says
    # nothing about the account's other models, so only other adapters' account limits widen.
    model_quota = adapter in {"agy", "opencode"}
    if not model_quota and row["status"] == "usage_limited" and re.search(
        r"(?:usage|session|weekly|account|plan) limit|individual quota reached|insufficient_quota", excerpt, re.I
    ):
        model = "*"
    with locked(path.with_name("cooldowns.lock")):
        values = _load_cooldowns(path, at)
        resolver = identity_resolver()
        scope = resolver.scope(adapter)
        scope.update({field: row[field] for field in ('account', 'host') if field in row})
        # An attempt that captured an earlier id of this machine keys as the current id, like the
        # loaded entries; a peer's id stays the peer's.
        scope['host'] = resolver.same_host(scope['host'])
        key = cooldown_key(adapter, model, scope)
        if row["status"] == "ok":
            # A substitution proves the answering model recovered, not the requested one.
            # An account-wide `*` entry clears only for an attempt that began after it was
            # recorded (a stale in-flight success must not clear a newer limit) and never on
            # model_quota adapters. Accepted risk: a lighter model succeeding while a heavier
            # one is still limited costs one attempt, which re-arms the cooldown.
            wide_key = cooldown_key(adapter, "*", scope)
            wide = values.get(wide_key)
            try:
                clears_wide = (not model_quota and wide is not None
                               and parse_time(row["started_at"]) > parse_time(wide["recorded_at"]))
            except (KeyError, ValueError, TypeError, AttributeError):
                clears_wide = False
            gone = {key} if model != "*" else set()
            if clears_wide:
                gone.add(wide_key)
            gone &= values.keys()
            if not gone:
                return
            for item in gone:
                del values[item]
        else:
            policy = pacing_policy()
            backoff = None
            if row["status"] == "rate_limited":
                prior = values.get(key, {}).get("backoff_seconds", 0)
                if not isinstance(prior, (int, float)) or not math.isfinite(prior) or prior < 0:
                    prior = 0
                backoff = min(policy["cap_seconds"], max(policy["base_delay_seconds"], prior * 2))
            try:
                until = parse_time(row["reset_at"]) if row.get("reset_at") else None
            except (ValueError, TypeError):
                until = None
            delay = row.get("retry_after")
            if not isinstance(delay, (int, float)) or not math.isfinite(delay) or delay <= 0:
                delay = backoff or 3600
            if until is None or until <= at:
                until = at + timedelta(seconds=delay)
            # Provider reset/retry windows are lower bounds, as is local backoff.
            if backoff is not None:
                until = max(until, at + timedelta(seconds=backoff))
            values[key] = {
                **({"backoff_seconds": backoff} if backoff is not None else {}),
                "adapter": adapter,
                "model": model,
                **scope,
                "status": row["status"],
                "cooling_until": timestamp(until),
                "source_run": row["run_id"],
                "recorded_at": timestamp(at),
                "signature": row["evidence"]["signature"],
            }
        _store_cooldowns(path, values)


def append_index(row, run_dir, *, root=None):
    root = Path(root) if root is not None else run_root(row["cwd"]) / ".agent-run"
    path = root / "runs/index.jsonl"
    key = (row["run_id"], row["task_id"], row["attempt"])
    result = row.get("paths", {}).get("result")
    result_sha = row.get("result_sha256")
    if result and result_sha is None:
        candidate = Path(result)
        if not candidate.is_absolute():
            candidate = Path(run_dir) / candidate
        # Artifact custody was verified by the owner; index only run-local files.
        if (
            candidate.resolve().is_relative_to(Path(run_dir).resolve())
            and candidate.is_file()
            and not candidate.is_symlink()
        ):
            result_sha = "sha256:" + hashlib.sha256(candidate.read_bytes()).hexdigest()
    entry = {
        "run_id": row["run_id"],
        "task_id": row["task_id"],
        "attempt": row["attempt"],
        "dir": str(run_dir),
        "status": row["status"],
        "line": row["provenance"].get("line", ""),
        "result_sha256": result_sha,
        "ended_at": row["ended_at"],
    }
    with locked(path.with_suffix(".lock")):
        fd = store_open(path, os.O_RDWR | os.O_CREAT | os.O_APPEND, create_parents=True)
        with os.fdopen(fd, "a+") as stream:
            if os.fstat(stream.fileno()).st_nlink != 1:
                raise ValueError("index must not be hard-linked")
            stream.seek(0)
            for line in stream:
                try:
                    old = json.loads(line)
                except ValueError:
                    continue
                if (old.get("run_id"), old.get("task_id"), old.get("attempt")) == key:
                    return
            stream.seek(0, os.SEEK_END)
            stream.write(json.dumps(entry, separators=(",", ":")) + "\n")
            stream.flush()
            os.fsync(stream.fileno())
