#!/usr/bin/env python3
"""Refuse tracked files holding a placeholder for work not done.

A task marker left in a comment, a macro that panics where code should be, or
a test switched off with an attribute all compile, pass every lint this
repository runs and read as finished in a review. Nothing else here looks for
them, so this does.

Run it over the whole tree with no arguments, or over named paths.
"""

from __future__ import annotations

import re
import subprocess
import sys
from collections.abc import Iterator
from pathlib import Path

#: What opens a comment in the files this repository tracks: Rust, C and
#: CSS (``//``, ``/*``), Python, TOML, YAML and shell (``#``), Markdown and
#: HTML (``<!--``).
COMMENT = re.compile(r"//|/\*|#|<!--")

#: A task marker as a whole word in any letter case, refused after a comment
#: opener on the same line. The word is searched first and compared with the
#: line's first opener, so a line is read a constant number of times however
#: many openers it holds.
COMMENT_TASK = re.compile(r"\b(?:" + "to" + "do|" + "fix" + r"me)\b", re.IGNORECASE)

#: What is refused on any line, each with the reason printed beside a hit.
#:
#: The patterns are assembled from pieces so that this file does not match
#: itself; spelled out whole, every one of them would be a hit here. The two
#: task markers are refused in capitals anywhere, and in any letter case after
#: a comment opener (``COMMENT_TASK``), as whole words only, so that an
#: identifier that merely contains one stays clean.
PLACEHOLDERS: tuple[tuple[re.Pattern[str], str], ...] = (
    (re.compile(r"\b" + "TO" + r"DO\b"), "a task marker"),
    (re.compile(r"\b" + "FIX" + r"ME\b"), "a task marker"),
    (re.compile(r"\b" + "X" + r"XX\b"), "a task marker"),
    (
        re.compile(r"\b" + "to" + r"do!\s*[(\[{]"),
        "a macro that panics in place of code",
    ),
    (
        re.compile(r"\b" + "unimple" + r"mented!\s*[(\[{]"),
        "a macro that panics in place of code",
    ),
    (re.compile(r"#\[\s*" + "ign" + r"ore\b"), "a test switched off"),
)

#: The word that switches a test off inside a ``cfg_attr``.
IGNORE = "ign" + "ore"

#: The start of an outer or inner ``cfg_attr`` attribute, with the whitespace
#: Rust allows between its tokens. Every quantifier is possessive, so a failed
#: attempt never splits a run of whitespace two ways.
CFG_ATTR = re.compile(r"#\s*+(?:!\s*+)?\[\s*+cfg_attr\s*+\(")

#: One token of an attribute's arguments. The literals and comments are
#: opaque: nothing inside them opens or closes a delimiter or is a word. A
#: string or raw string left open runs to the end of the file. Every
#: repetition is possessive and every alternative starts on a character that
#: it then consumes, so a match never backtracks over what it has read.
TOKEN = re.compile(
    r"""
      (?P<space>\s++)
    | (?P<line_comment>//[^\n]*+)
    | (?P<block_comment>/\*)
    | (?P<raw_string>[bc]?r(?P<hashes>\#*+)")
    | (?P<string>[bc]?"(?:[^"\\]++|\\.?)*+"?)
    | (?P<char>b?'(?:[^\\'\n]|\\(?:x[0-9A-Fa-f]{2}|u\{[0-9A-Fa-f_]{1,6}\}|.))')
    | (?P<lifetime>'\w++)
    | (?P<raw_word>r\#(?P<raw_name>\w++))
    | (?P<word>\w++)
    | (?P<open>[(\[{])
    | (?P<close>[)\]}])
    | (?P<other>.)
    """,
    re.VERBOSE | re.DOTALL,
)

#: What opens or closes a block comment; Rust nests them.
BLOCK_COMMENT = re.compile(r"/\*|\*/")


def block_comment_end(text: str, start: int) -> int:
    """Where a block comment ends, counting the comments nested in it.

    Args:
        text: The whole file.
        start: The position just after the comment's ``/*``.

    Returns:
        The position just after its matching ``*/``, or the end of the file
        when the comment never closes.
    """
    depth = 1
    for mark in BLOCK_COMMENT.finditer(text, start):
        depth += 1 if mark.group(0) == "/*" else -1
        if depth == 0:
            return mark.end()
    return len(text)


def attribute_end(text: str, start: int) -> tuple[int, int | None]:
    """Walk an attribute's arguments once, up to the parenthesis closing them.

    Args:
        text: The whole file.
        start: The position just after the ``(`` that opens the arguments.

    Returns:
        The position just after the closing parenthesis (the end of the file
        when there is none), and the end of the first ``ignore`` word among
        the arguments outside every literal and comment, or ``None``.
    """
    depth = 1
    hit: int | None = None
    position = start
    while position < len(text):
        token = TOKEN.match(text, position)
        if token is None:
            raise AssertionError(f"no token matches at offset {position}")
        position = token.end()
        match token.lastgroup:
            case "open":
                depth += 1
            case "close":
                depth -= 1
                if depth == 0:
                    return position, hit
            case "word" | "raw_word" if hit is None:
                word = token.group("raw_name") or token.group("word")
                if word == IGNORE:
                    hit = position
            case "block_comment":
                position = block_comment_end(text, position)
            case "raw_string":
                closing = '"' + token.group("hashes")
                found = text.find(closing, position)
                position = len(text) if found < 0 else found + len(closing)
    return len(text), hit


def switched_off(text: str) -> Iterator[tuple[int, int]]:
    """Every ``cfg_attr`` attribute that switches a test off.

    rustfmt writes a long ``cfg_attr`` over several lines, and its arguments
    are token trees that may hold strings, raw strings, char literals and
    comments, any of which may hold a ``]``, a ``)``, a ``#`` or the word
    itself. So each opener (``#[`` or ``#![``, then ``cfg_attr`` and ``(``) is
    followed by a walk over its arguments' tokens to the parenthesis that
    closes them, and a hit is the word ``ignore`` as a token of its own
    (``ignore = "reason"``, a nested ``cfg_attr`` and the raw identifier
    ``r#ignore`` included) outside every literal and comment.

    The search for the next attribute resumes where the walk stopped, so no
    position is read by two walks and the file is read a constant number of
    times: an attribute left open ends at the end of the file, and whatever
    follows it is read once, as its arguments, never again for each opener it
    holds.

    Args:
        text: The whole file.

    Yields:
        The start of each such attribute and the end of its ``ignore``.
    """
    position = 0
    while (opener := CFG_ATTR.search(text, position)) is not None:
        position, hit = attribute_end(text, opener.end())
        if hit is not None:
            yield opener.start(), hit


def tracked_files() -> list[str]:
    """Every path git tracks, in the repository this is run from.

    Returns:
        The tracked paths, relative to the repository root.

    Raises:
        subprocess.CalledProcessError: If this is not a git repository.
    """
    listed = subprocess.run(
        ["git", "ls-files", "-z"], capture_output=True, check=True, text=True
    )
    return [path for path in listed.stdout.split("\0") if path]


def faults(path: str) -> list[str]:
    """Every placeholder in one file.

    A file that is not UTF-8 is left to the text-hygiene check, which reports
    it; it is skipped here rather than reported twice.

    Args:
        path: The file to read, relative to the working directory.

    Returns:
        One line per hit, empty when the file holds none.
    """
    try:
        text = Path(path).read_text(encoding="utf-8")
    except UnicodeDecodeError:
        return []
    found: list[str] = []
    for number, line in enumerate(text.split("\n"), start=1):
        for pattern, reason in PLACEHOLDERS:
            for hit in pattern.finditer(line):
                found.append(
                    f"{path}:{number}:{hit.start() + 1}: {reason}: {hit.group(0)}"
                )
        if (opener := COMMENT.search(line)) is not None:
            for hit in COMMENT_TASK.finditer(line, opener.end()):
                found.append(
                    f"{path}:{number}:{hit.start() + 1}: "
                    f"a task marker in a comment: {hit.group(0)}"
                )
    # The line and column of each hit are counted on from the previous one,
    # so many hits in one file, or on one long line, do not reread the text.
    number, line_start, counted = 1, 0, 0
    for start, end in switched_off(text):
        number += text.count("\n", counted, start)
        line_start = text.rfind("\n", counted, start) + 1 or line_start
        counted = start
        shown = " ".join(text[start:end].split())
        found.append(
            f"{path}:{number}:{start - line_start + 1}: "
            f"a test switched off under a condition: {shown}"
        )
    return found


def main(argv: list[str]) -> int:
    """Scan the named files, or every tracked file when none are named.

    Args:
        argv: Paths to scan; empty means the whole tracked tree.

    Returns:
        The process exit status: 0 when no file holds a placeholder.
    """
    paths = argv or tracked_files()
    found = [fault for path in paths for fault in faults(path)]
    for fault in found:
        print(fault)
    if found:
        print(f"{len(found)} placeholder(s) found", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
