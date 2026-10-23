#!/usr/bin/env python3
"""PreToolUse Bash hook: refuse pattern-based and non-numeric process kills.

Reads the hook event on stdin. A command that would signal processes by name,
pattern, process group or shell expansion gets a deny decision; everything else,
including malformed input, passes. `deny_reason(command)` is the testable core.
"""

from __future__ import annotations

import json
import os
import re
import sys

MESSAGE = (
    "Refused: {why}. A pattern, group or expanded kill can hit another lane's "
    "process. Stop explicit PIDs with `scripts/reap.py <pid>… [--tree]`."
)
PATTERN_KILLERS = {"pkill", "killall", "killall5", "skill"}
WRAPPERS = {
    "sudo", "doas", "env", "nice", "ionice", "timeout", "xargs", "nohup",
    "command", "builtin", "exec", "time", "setsid", "stdbuf", "caffeinate",
}
SHELLS = {"sh", "bash", "zsh", "dash", "ksh"}
RESERVED = {
    "{", "}", "!", "if", "then", "else", "elif", "fi", "while", "until", "do",
    "done", "for", "in", "case", "esac", "function", "select",
}
# Options of each wrapper that consume the next word.
WRAPPER_VALUE_OPTIONS = {
    "sudo": set("ugpCDhRrTtU"), "doas": set("uC"), "env": set("uCS"),
    "nice": set("n"), "ionice": set("cnp"), "timeout": set("sk"),
    "xargs": set("IJnPLsdEa"), "stdbuf": set("ioe"),
}
WRAPPER_VALUE_LONG_OPTIONS = {
    "sudo": {"user", "group", "host", "prompt", "chdir", "role", "type", "other-user", "close-from"},
    "doas": set(), "env": {"unset", "chdir", "split-string"}, "nice": {"adjustment"},
    "ionice": {"class", "classdata"}, "timeout": {"signal", "kill-after"},
    "xargs": {"max-args", "max-procs", "max-lines", "delimiter", "arg-file", "replace"},
    "stdbuf": {"input", "output", "error"},
}
QUOTED_HEREDOC = re.compile(r"<<-?\s*(['\"])(\w+)\1[^\n]*\n.*?\n\s*\2(?:\n|$)", re.S)
MAX_DEPTH = 8
# Backstop on the raw text for what the parser misses (for example inside ${…}):
# pkill or killall as a command word, or after a wrapper on the same command.
RAW_PATTERN = re.compile(
    r"(?:^|[;&|({`\n]|\$\()\s*(?:\S*/)?(?:pkill|killall)\b"
    r"|\b(?:sudo|doas|env|xargs|nice|timeout|nohup|exec|command|time|setsid|stdbuf)\b"
    r"[^;&|\n]*?\s(?:\S*/)?(?:pkill|killall)\b"
)
NUMERIC = re.compile(r"\d+")


class Command:
    def __init__(self) -> None:
        self.words: list[str] = []
        self.bodies: list[str] = []  # here-document and here-string text

    def __bool__(self) -> bool:
        return bool(self.words)


def _skip_substitution(text: str, start: int) -> int:
    """Index just past the `)` that closes a `$(` or `(` opened before start."""
    depth, i, quote = 1, start, ""
    while i < len(text):
        c = text[i]
        if quote:
            if c == "\\" and quote == '"':
                i += 1
            elif c == quote:
                quote = ""
        elif c == "\\":
            i += 1
        elif c in "'\"":
            quote = c
        elif c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return len(text)


def _skip_backtick(text: str, start: int) -> int:
    i = start
    while i < len(text) and text[i] != "`":
        i += 2 if text[i] == "\\" else 1
    return min(i + 1, len(text))


def substitutions(text: str, double_quoted: bool = False) -> list[str]:
    """Command text of every `$(...)` and backtick substitution in text.

    Inside double quotes an apostrophe is a literal, not a single quote.
    """
    found, i, single = [], 0, False
    while i < len(text):
        c = text[i]
        if c == "\\":
            i += 2
            continue
        if c == "'" and not double_quoted:
            single = not single
        elif not single and text.startswith("$(", i) and not text.startswith("$((", i):
            end = _skip_substitution(text, i + 2)
            found.append(text[i + 2 : end - 1])
            i = end
            continue
        elif not single and c == "`":
            end = _skip_backtick(text, i + 1)
            found.append(text[i + 1 : end - 1])
            i = end
            continue
        i += 1
    return found


def parse(text: str) -> tuple[list[Command], list[str]]:
    """Split a shell line into simple commands plus nested command strings.

    Quotes, escapes, comments, redirections, `;` `&` `&&` `||` `|` newlines,
    parentheses, substitutions and here-documents are handled. Parameter
    expansion is not performed: a word keeps its `$name` text.
    """
    commands: list[Command] = []
    nested: list[str] = []
    current = Command()
    word: list[str] = []
    in_word = False
    skip_next = ""  # "redirect" or "here" while the next word is consumed
    pending: list[tuple[Command, str, bool]] = []  # here-documents awaiting a body
    i, n = 0, len(text)

    def end_word() -> None:
        nonlocal word, in_word, skip_next
        if not in_word:
            return
        value = "".join(word)
        word, in_word = [], False
        if skip_next == "redirect":
            skip_next = ""
        elif skip_next == "here":
            skip_next = ""
            current.bodies.append(value)
        elif skip_next:
            pending.append((current, value, skip_next == "heredoc-raw"))
            skip_next = ""
        else:
            current.words.append(value)

    def end_command() -> None:
        nonlocal current
        end_word()
        if current:
            commands.append(current)
        elif current.bodies:
            commands.append(current)
        current = Command()

    while i < n:
        c = text[i]
        if c == "\\":
            if i + 1 < n and text[i + 1] == "\n":
                i += 2
                continue
            in_word = True
            word.append(text[i + 1] if i + 1 < n else "")
            i += 2
        elif c == "'":
            end = text.find("'", i + 1)
            end = n if end < 0 else end
            in_word = True
            word.append(text[i + 1 : end])
            if skip_next == "heredoc":
                skip_next = "heredoc-raw"
            i = end + 1
        elif c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            body = text[i + 1 : j]
            nested.extend(substitutions(body))
            in_word = True
            word.append(re.sub(r"\\(.)", r"\1", body))
            if skip_next == "heredoc":
                skip_next = "heredoc-raw"
            i = j + 1
        elif text.startswith("$'", i):
            end = i + 2
            while end < n and text[end] != "'":
                end += 2 if text[end] == "\\" else 1
            in_word = True
            word.append(text[i + 2 : end])
            i = end + 1
        elif text.startswith("$((", i):
            end = _skip_substitution(text, i + 2)
            in_word = True
            word.append(text[i:end])
            i = end
        elif text.startswith("$(", i) or (c in "<>" and text.startswith("(", i + 1)):
            start = i + 2
            end = _skip_substitution(text, start)
            nested.append(text[start : end - 1])
            if c == "$":
                in_word = True
                word.append("$(…)")
            i = end
        elif c == "`":
            end = _skip_backtick(text, i + 1)
            nested.append(text[i + 1 : end - 1])
            in_word = True
            word.append("`…`")
            i = end
        elif text.startswith("${", i):
            end = text.find("}", i)
            end = n - 1 if end < 0 else end
            in_word = True
            word.append(text[i : end + 1])
            i = end + 1
        elif c == "#" and not in_word:
            while i < n and text[i] != "\n":
                i += 1
        elif c in " \t":
            end_word()
            i += 1
        elif c == "\n":
            end_command()
            for owner, delimiter, raw in pending:
                lines, i = [], i + 1
                while i < n:
                    eol = text.find("\n", i)
                    eol = n if eol < 0 else eol
                    line = text[i:eol]
                    i = eol + 1
                    if line.strip("\t") == delimiter:
                        break
                    lines.append(line)
                body = "\n".join(lines)
                owner.bodies.append(body)
                if not raw:
                    nested.extend(substitutions(body))
                i -= 1
            pending.clear()
            i += 1
        elif c in "<>":
            digits = "".join(word)
            if in_word and digits.isdigit():
                word, in_word = [], False
            else:
                end_word()
            if text.startswith("<<<", i):
                skip_next, i = "here", i + 3
            elif text.startswith("<<", i):
                skip_next, i = "heredoc", i + 2
                if text.startswith("-", i):
                    i += 1
            else:
                skip_next = "redirect"
                i += 1
                while i < n and text[i] in "<>&|":
                    i += 1
        elif c in ";&|()":
            if c == "&" and text.startswith(">", i + 1):
                skip_next, i = "redirect", i + 2
                continue
            end_command()
            skip_next = ""
            i += 1
            while i < n and text[i] in ";&|" and c in ";&|":
                i += 1
        else:
            in_word = True
            word.append(c)
            i += 1
    end_command()
    return commands, nested


def _strip_reserved(words: list[str]) -> list[str]:
    while words and words[0] in RESERVED:
        words = words[1:]
    while words and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*=.*", words[0]):
        words = words[1:]
    return words


def _unwrap(words: list[str]) -> tuple[list[str], bool]:
    """Strip wrappers and their options; report whether xargs fed the command."""
    via_xargs = False
    words = _strip_reserved(words)
    while words and os.path.basename(words[0]) in WRAPPERS:
        wrapper = os.path.basename(words[0])
        via_xargs = via_xargs or wrapper == "xargs"
        words, takes = words[1:], WRAPPER_VALUE_OPTIONS.get(wrapper, set())
        while words and (words[0].startswith("-") or (
            wrapper in {"env", "nice"} and "=" in words[0]
        )):
            if words[0].startswith("--") and "=" not in words[0] and (
                words[0][2:] in WRAPPER_VALUE_LONG_OPTIONS.get(wrapper, set())
            ):
                words = words[2:]
                continue
            short = words[0][1:]
            skip = 2 if (
                not words[0].startswith("--") and len(short) == 1 and short in takes
            ) else 1
            words = words[skip:]
        if wrapper == "timeout":
            words = words[1:]  # the duration
        words = _strip_reserved(words)
    return words, via_xargs


def _kill_reason(args: list[str]) -> str | None:
    targets, options_done, i, signalled = [], False, 0, False
    while i < len(args):
        word = args[i]
        if not options_done and word == "--":
            options_done = True
        elif not options_done and word in {"-l", "-L", "-t"}:
            return None  # listing signals only
        elif not options_done and word in {"-s", "-n", "--signal"}:
            signalled = True
            i += 1
        elif not options_done and word.startswith("-") and len(word) > 1:
            if signalled and word[1:].isdigit():
                return "kill of a process group"
            signalled = True
        else:
            targets.append(word)
        i += 1
    for target in targets:
        if target.startswith("-"):
            return "kill of a process group"
        if not NUMERIC.fullmatch(target):
            return f"kill target {target!r} is not a literal PID"
        if int(target) == 0:
            return "kill 0 signals the caller's process group"
    return None


def _command_reason(command: Command, depth: int) -> str | None:
    words, via_xargs = _unwrap(command.words)
    if not words:
        return None
    name = os.path.basename(words[0])
    args = words[1:]
    if name in PATTERN_KILLERS:
        return f"{name} signals processes by name"
    if name == "kill":
        if via_xargs:
            return "xargs kill takes its targets from a pipe"
        return _kill_reason(args)
    if name in SHELLS:
        for index, arg in enumerate(args):
            if arg.startswith("-") and not arg.startswith("--") and "c" in arg[1:]:
                if index + 1 < len(args):
                    return deny_reason(args[index + 1], depth + 1)
                break
            if not arg.startswith("-"):
                break
        for body in command.bodies:
            reason = deny_reason(body, depth + 1)
            if reason:
                return reason
    if name == "eval" or name == ".":
        return deny_reason(" ".join(args), depth + 1) if name == "eval" else None
    if name in {"find", "fd"}:
        for index, arg in enumerate(args):
            if arg in {"-exec", "-execdir", "-ok", "-okdir", "-x", "--exec"}:
                inner = []
                for item in args[index + 1 :]:
                    if item in {";", "+"}:
                        break
                    inner.append(item)
                rest = Command()
                rest.words = inner
                reason = _command_reason(rest, depth + 1)
                if reason:
                    return reason
    return None


def executable_text(command: str) -> str:
    """Command text without what the shell will not run.

    Drops quoted here-document bodies and single-quoted spans; inside double
    quotes keeps only `$(…)` and backtick bodies, which still execute, filtered
    the same way. An unterminated quote runs to the end of the line, as the
    shell would refuse to run it.
    """
    text, out, i = QUOTED_HEREDOC.sub("", command), [], 0
    while i < len(text):
        c = text[i]
        if c == "\\":
            out.append(text[i : i + 2])
            i += 2
        elif c == "'":
            end = text.find("'", i + 1)
            i = len(text) if end < 0 else end + 1
            out.append(" ")
        elif c == '"':
            j = i + 1
            while j < len(text) and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            out.extend(f"\n{executable_text(inner)}\n" for inner in substitutions(text[i + 1 : j], True))
            out.append(" ")
            i = j + 1
        else:
            out.append(c)
            i += 1
    return "".join(out)


def deny_reason(command: str, depth: int = 0) -> str | None:
    """Why the command line must be refused, or None when it may run."""
    if depth > MAX_DEPTH:
        return "command nesting is too deep to check"
    if RAW_PATTERN.search(executable_text(command)):
        return "pkill or killall signals processes by name"
    commands, nested = parse(command)
    for item in commands:
        reason = _command_reason(item, depth)
        if reason:
            return reason
    for text in nested:
        reason = deny_reason(text, depth + 1)
        if reason:
            return reason
    return None


def main() -> int:
    try:
        event = json.load(sys.stdin)
        command = event["tool_input"]["command"]
        if event.get("tool_name") != "Bash" or not isinstance(command, str):
            return 0
    except (ValueError, KeyError, TypeError, AttributeError):
        return 0
    reason = deny_reason(command)
    if reason:
        print(json.dumps({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": MESSAGE.format(why=reason),
        }}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
