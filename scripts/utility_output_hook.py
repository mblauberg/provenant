#!/usr/bin/env python3
"""Optional Claude Code PostToolUse digest for oversized Bash results."""

from __future__ import annotations

import json
import os
from pathlib import Path
import shlex
import sys

import harness_profile
import utility


def checked_command(command, cwd):
    # Only remote GitHub reads and direct, explicit file readers have checkable
    # sources. Shells, interpreters, test runners and compound commands do not.
    if any(char in command for char in '$`;&|<>(){}\n*?[]'):
        return False
    words = shlex.split(command)
    if not words:
        return False
    if words[0] == 'gh':
        return (len(words) >= 3 and tuple(words[1:3]) in {
            ('run', 'view'), ('run', 'list'), ('pr', 'view'), ('pr', 'checks'),
            ('pr', 'list'), ('issue', 'view'), ('issue', 'list'),
        })
    if words[0] not in {'cat', 'head', 'tail'}:
        return False
    paths = []
    options = True
    index = 1
    while index < len(words):
        word = words[index]
        if options and word == '--':
            options = False
        elif options and word.startswith('-'):
            if words[0] in {'head', 'tail'} and word in {'-n', '-c'}:
                index += 1
                if index >= len(words) or not words[index].lstrip('+-').isdigit():
                    return False
            elif words[0] == 'cat' and word in {'-n', '-b', '-s', '-v', '-e', '-t', '-u'}:
                pass
            else:
                return False
        else:
            candidate = Path(word).expanduser()
            path = utility.input_path(candidate if candidate.is_absolute() else cwd / candidate, workspace=cwd)
            if not path.is_file():
                return False
            paths.append(path)
        index += 1
    return bool(paths)


def handle(payload, *, digest=None):
    """Return supported hook JSON on success; no output means keep the original."""
    try:
        cwd = Path(payload['cwd']).resolve()
        knobs = harness_profile.profiles.current(cwd=cwd).knobs
        if (not knobs['utility.digest_tool_output'] or os.environ.get('PROVENANT_UTILITY_ACTIVE') == '1'
                or payload.get('hook_event_name') != 'PostToolUse' or payload.get('tool_name') != 'Bash'):
            return None
        response = payload['tool_response']
        stdout, stderr = response.get('stdout', ''), response.get('stderr', '')
        if not isinstance(stdout, str) or not isinstance(stderr, str) or response.get('isImage'):
            return None
        source = stdout + ('\n' if stdout and stderr else '') + stderr
        if len(source.encode('utf-8')) <= knobs['utility.output_threshold_bytes']:
            return None
        # Bash has no input-file provenance. In a project declaring protected paths,
        # even an innocuous command can indirectly read them: leave every result alone.
        if utility.provider_exec.protected_paths(cwd) or utility.provider_exec.credential_path(cwd):
            return None
        if not checked_command(payload['tool_input']['command'], cwd):
            return None
        directory = utility.layout.new_run_dir(cwd=cwd, kind='dispatch', slug='tool-output')
        directory.chmod(0o700)
        path = directory / 'output.txt'
        with path.open('x', encoding='utf-8') as stream:
            os.chmod(path, 0o600)
            stream.write(source)
        result = (digest or utility.execute)(
            'digest', source, question='Summarise the result, failures and next action. Cite source lines.',
            confidential=True,
        )
        pointers = ', '.join(f'{path}:L{line}' for line in result['lines'])
        context = f"{result['answer']}\nFull Bash output: {pointers}\nRead only relevant lines from the file."
        return {'hookSpecificOutput': {
            'hookEventName': 'PostToolUse', 'additionalContext': context,
            'updatedToolOutput': {**response, 'stdout': context, 'stderr': ''},
        }}
    except Exception:
        return None


def main():
    try:
        result = handle(json.load(sys.stdin))
        if result is not None:
            print(json.dumps(result))
    except Exception:
        pass
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
