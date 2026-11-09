"""Read selected native histories; private rows use telemetry session/message identities."""
import hashlib
import json
import re
from pathlib import Path

EXPOSURES = ('experiment', 'installed_release', 'instruction_version', 'work_mix', 'context')
USAGE = ('uncached_input', 'cache_read', 'cache_write', 'cache_write_1h', 'output', 'reasoning')


def _hash(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True).encode()).hexdigest()


def _text(content):
    if isinstance(content, str):
        return content
    return '\n'.join(p.get('text', '') for p in (content or [])
                     if p.get('type') in ('text', 'input_text', 'output_text'))


def _usage(raw, claude=False):
    if raw is None:
        return None
    if claude:
        split = raw.get('cache_creation', {})
        hour = split.get('ephemeral_1h_input_tokens')
        creation = raw.get('cache_creation_input_tokens')
        if hour is not None:
            creation = max(creation or 0, hour)
        return dict(zip(USAGE, (raw.get('input_tokens'), raw.get('cache_read_input_tokens'),
                    creation - (hour or 0) if creation is not None else None,
                    hour, raw.get('output_tokens'), None)))
    cached = raw.get('cached_input_tokens')
    written = raw.get('cache_write_input_tokens')
    total = raw.get('input_tokens')
    return dict(zip(USAGE, (max(0, total - (cached or 0) - (written or 0))
                if total is not None else None, cached, written, None,
                raw.get('output_tokens'), raw.get('reasoning_output_tokens'))))


def _sum_usage(a, b):
    if a is None:
        return b.copy() if b else None
    if b is None:
        return a
    return {k: None if a.get(k) is None and b.get(k) is None
            else (a.get(k) or 0) + (b.get(k) or 0) for k in USAGE}


def _merge_raw(a, b):
    """Claude streams cumulative per-message counters; telemetry ingest uses maxima."""
    out = dict(a or {})
    for k, v in (b or {}).items():
        if isinstance(v, dict):
            out[k] = _merge_raw(out.get(k), v)
        elif isinstance(v, (int, float)):
            out[k] = max(out.get(k, 0), v)
    return out


def _events(path):
    with Path(path).open() as stream:
        for line in stream:
            if line.strip():
                yield json.loads(line)


def _instructions(event, payload):
    captured = []
    for obj in (event, payload):
        for key in ('instructions', 'base_instructions', 'developer_instructions',
                    'user_instructions', 'instruction_attachments'):
            if obj.get(key) is not None:
                captured.append((key, obj[key]))
    return captured


def _source(source, releases):
    events = list(_events(source['path']))
    host = source['host']
    provider = ('claude' if any(e.get('type') == 'assistant' for e in events)
                else 'codex') if host == 'lane' else host
    session = source.get('session')
    # Provider-owned identifiers, shared with telemetry ingest and lane records.
    for event in events:
        if event.get('type') == 'session_meta':
            session = event['payload']['id']
        session = session or event.get('sessionId') or event.get('session_id')
        if event.get('type') == 'thread.started':
            session = session or event['thread_id']
    if session is None:
        raise ValueError('source needs its recorded session or lane attempt identity')
    state = {k: source.get(k) for k in EXPOSURES}
    project = source.get('project')
    captures = []
    compactions = source.get('compactions')
    user = None
    rows = []
    current = None
    pending_usage = None
    previous_total = {}
    seen_messages = {}
    seen_users = set()
    era = 0
    previous_exposure = None
    exposures = iter(sorted(source.get('exposures', []), key=lambda e: e['timestamp']))
    exposure = next(exposures, None)

    # Merge assistant stream copies before reading turns, retaining original order.
    if host == 'claude' or any(e.get('type') == 'assistant' for e in events):
        merged = []
        for event in events:
            if event.get('type') != 'assistant':
                merged.append(event)
                continue
            message = event['message']
            identity = message.get('id')
            if identity is not None and identity in seen_messages:
                prior = seen_messages[identity]['message']
                prior['usage'] = _merge_raw(prior.get('usage'), message.get('usage'))
                for key in ('content', 'stop_reason'):
                    if message.get(key) is not None:
                        prior[key] = message[key]
            else:
                merged.append(event)
                if identity is not None:
                    seen_messages[identity] = event
        events = merged

    def finish():
        nonlocal current, pending_usage
        if current is not None:
            current['usage'] = _sum_usage(current['usage'], pending_usage)
            rows.append(current)
            current = None
            pending_usage = None

    for index, event in enumerate(events):
        kind = event.get('type')
        measured_context = None
        payload = event.get('payload', {})
        timestamp = event.get('timestamp')
        while exposure is not None and timestamp is not None and exposure['timestamp'] <= timestamp:
            state.update({k: exposure[k] for k in EXPOSURES if k in exposure})
            exposure = next(exposures, None)
        state.update({k: payload[k] for k in EXPOSURES if k in payload})
        state.update({k: event[k] for k in EXPOSURES if k in event})
        if kind == 'session_meta':
            project = project or payload.get('cwd')
        if kind == 'turn_context':
            project = project or payload.get('cwd')
        captured = _instructions(event, payload)
        for material in captured:
            if material not in captures:
                captures.append(material)
        if kind == 'system' and event.get('subtype') == 'compact_boundary' or kind == 'compacted' or payload.get('type') == 'context_compacted':
            compactions = (compactions or 0) + 1
        message = event.get('message', {})
        role = message.get('role')
        content = message.get('content')
        if kind == 'response_item' and payload.get('type') == 'message':
            role, content = payload.get('role'), payload.get('content')
            # Developer/system messages are captured material, never installed-version evidence.
            if role in ('developer', 'system'):
                material = ('message', _text(content))
                if material not in captures:
                    captures.append(material)
        if kind == 'user':
            role = 'user'
        if kind == 'event_msg' and payload.get('type') == 'user_message':
            role, content = 'user', payload.get('message')
        if role == 'user':
            text = _text(content)
            for attachment in re.findall(r'<(?:INSTRUCTIONS|system-reminder)>(.*?)</(?:INSTRUCTIONS|system-reminder)>', text, re.S):
                material = ('attachment', attachment)
                if material not in captures:
                    captures.append(material)
            if not text:  # Tool results are not a new human turn.
                continue
            identity = event.get('uuid') or payload.get('id')
            if identity and identity in seen_users:
                continue
            if identity:
                seen_users.add(identity)
            if current is not None:
                current['next_user'] = text
                finish()
            elif rows and rows[-1]['next_user'] is None:
                rows[-1]['next_user'] = text
            user = text
            continue
        if kind == 'assistant':
            if current is not None:
                finish()
            raw = message.get('usage')
            if raw and all(k in raw for k in ('input_tokens', 'cache_read_input_tokens', 'cache_creation_input_tokens')):
                measured_context = sum(raw[k] for k in ('input_tokens', 'cache_read_input_tokens', 'cache_creation_input_tokens'))
            if message.get('model') == '<synthetic>':
                continue
            pending_usage = _sum_usage(pending_usage, _usage(message.get('usage'), True))
            if message.get('stop_reason') == 'tool_use':
                continue
            text = _text(content)
            identity = message.get('id') or event.get('uuid') or str(index)
        elif kind == 'response_item' and role == 'assistant':
            if payload.get('phase') not in (None, 'final_answer'):
                continue
            text = _text(content)
            identity = payload.get('id') or str(index)
        elif kind == 'event_msg' and payload.get('type') == 'agent_message':
            if payload.get('phase') not in (None, 'final_answer'):
                continue
            text = payload.get('message', '')
            identity = str(index)
        elif kind == 'item.completed' and event.get('item', {}).get('type') == 'agent_message':
            text = event['item']['text']
            identity = event['item'].get('id') or str(index)
        elif kind == 'event_msg' and payload.get('type') == 'token_count':
            info = payload.get('info') or {}
            last = info.get('last_token_usage') or {}
            if 'input_tokens' in last:
                if current is not None:
                    current['context'] = last['input_tokens']
            total = info.get('total_token_usage')
            if total is not None:
                delta = {k: v - previous_total.get(k, 0) for k, v in total.items() if isinstance(v, (int, float))}
                pending_usage = _sum_usage(pending_usage, _usage(delta))
                previous_total = total
            continue
        elif kind == 'turn.completed':
            pending_usage = _sum_usage(pending_usage, _usage(event.get('usage')))
            finish()
            continue
        else:
            continue
        if not text:
            continue
        # Codex emits the same final answer as event_msg and response_item.
        if current is not None and current['reply'] == text:
            continue
        # Keep the final answer of a provider turn (exec may stream commentary first).
        if current is not None and kind == 'assistant':
            finish()
        captured_hash = _hash(captures) if captures else None
        exposure_key = tuple(state[k] for k in EXPOSURES[:3]) + (captured_hash,)
        if previous_exposure is not None and exposure_key != previous_exposure:
            era += 1
        previous_exposure = exposure_key
        release = next((r['release'] for r in reversed(releases)
                        if timestamp is not None and r['timestamp'] <= timestamp), None)
        current = dict(id=_hash((provider, session, identity)), session=session, host=host,
                       project=project, timestamp=timestamp,
                       period=timestamp[:10] if timestamp else None, **state,
                       release_era_proxy=release, captured_instruction_hash=captured_hash,
                       within_session_era=era, compactions=compactions, user=user,
                       reply=text, next_user=None, usage=None,
                       sample=source.get('sample'), graph=source.get('graph'))
        if measured_context is not None:
            current['context'] = measured_context
    finish()
    return rows


def extract(manifest):
    """Return private reply rows. Sources are explicit; this function never scans homes."""
    releases = sorted(manifest.get('releases', []), key=lambda r: r['timestamp'])
    outcomes = []
    if manifest.get('outcomes'):
        outcomes = json.loads(Path(manifest['outcomes']).read_text())['rows']
    rows = {}
    for source in manifest['sources']:
        for row in _source(source, releases):
            matches = [o for o in outcomes
                       if row['sample'] is not None and o['evidence']['sample'] == row['sample']
                       and (row['graph'] is None or o['evidence'].get('graph') == row['graph'])]
            row['outcome'] = matches[0] if len(matches) == 1 else None
            # Same native history selected more than once contributes once.
            rows.setdefault(row['id'], row)
    return list(rows.values())
