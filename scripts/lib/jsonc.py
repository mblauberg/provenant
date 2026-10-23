"""Read JSONC configuration without changing comments or string contents."""

from __future__ import annotations

import json
from typing import Any


def parse_jsonc(text: str) -> tuple[Any, bool]:
    """Return the parsed value and whether the source contains comments."""
    chars = list(text)
    comment = False
    quoted = escaped = False
    index = 0
    while index < len(chars):
        current = chars[index]
        following = chars[index + 1] if index + 1 < len(chars) else ""
        if quoted:
            if escaped:
                escaped = False
            elif current == "\\":
                escaped = True
            elif current == '"':
                quoted = False
            index += 1
            continue
        if current == '"':
            quoted = True
            index += 1
            continue
        if current == "/" and following == "/":
            comment = True
            while index < len(chars) and chars[index] not in "\r\n":
                chars[index] = " "
                index += 1
            continue
        if current == "/" and following == "*":
            comment = True
            start = index
            index += 2
            while index + 1 < len(chars) and chars[index:index + 2] != ["*", "/"]:
                index += 1
            if index + 1 >= len(chars):
                raise json.JSONDecodeError("unterminated block comment", text, start)
            index += 2
            for position in range(start, index):
                if chars[position] not in "\r\n":
                    chars[position] = " "
            continue
        index += 1
    quoted = escaped = False
    for index, current in enumerate(chars):
        if quoted:
            if escaped:
                escaped = False
            elif current == "\\":
                escaped = True
            elif current == '"':
                quoted = False
        elif current == '"':
            quoted = True
        elif current == ",":
            following = index + 1
            while following < len(chars) and chars[following].isspace():
                following += 1
            if following < len(chars) and chars[following] in "}]":
                chars[index] = " "
    return json.loads("".join(chars)), comment
