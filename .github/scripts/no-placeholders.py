#!/usr/bin/env python3
"""Refuse tracked files holding a placeholder for work not done.

A task marker left in a comment, a macro that panics where code should be, or
a test switched off with an attribute all compile, pass every lint this
repository runs and read as finished in a review. Nothing else here looks for
them, so this does.

Run it over the whole tree with no arguments, or over named paths.
"""

import re
import subprocess
import sys
from collections.abc import Iterator
from pathlib import Path

#: What opens a comment in the files this repository tracks: Rust, C and
#: CSS (``//``, ``/*``), Python, TOML, YAML and shell (``#``), Markdown and
#: HTML (``<!--``).
COMMENT = re.compile(r"//|/\*|#|<!--")

#: A task marker as a whole word in any letter case, searched after a line's
#: first comment opener; one in capitals is left to ``PLACEHOLDERS``.
COMMENT_TASK = re.compile(r"\b(?:" + "to" + "do|" + "fix" + r"me)\b", re.IGNORECASE)

#: What is refused on any line, each with the reason printed beside a hit,
#: assembled from pieces so that this file does not match itself. A test
#: switched off outright is refused in the plain and the raw-identifier
#: spelling (``r#`` before the word), which rustc accepts alike.
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
    (
        re.compile(r"#\[\s*+(?:r#)?" + "ign" + r"ore\b"),
        "a test switched off",
    ),
)

#: The word that switches a test off inside a ``cfg_attr``.
IGNORE = "ign" + "ore"

#: The start of an outer or inner ``cfg_attr`` attribute, with the whitespace
#: Rust allows between its tokens. Every quantifier is possessive, so a failed
#: attempt never splits a run of whitespace two ways.
CFG_ATTR = re.compile(r"#\s*+(?:!\s*+)?\[\s*+cfg_attr\s*+\(")

#: One token of an attribute's arguments, read as Rust source. The literals
#: and comments are opaque: nothing inside them opens or closes a delimiter or
#: is a word. A ``\u{..}`` escape takes any run of hex digits and underscores,
#: since Rust caps its digits at six but not its underscores.
TOKEN = re.compile(
    r"""
      (?P<space>\s++)
    | (?P<line_comment>//[^\n]*+)
    | (?P<block_comment>/\*)
    | (?P<raw_string>[bc]?r(?P<hashes>\#*+)")
    | (?P<string>[bc]?"(?:[^"\\]++|\\.?)*+"?)
    | (?P<char>b?'(?:[^\\'\n]|\\(?:x[0-9A-Fa-f]{2}|u\{[0-9A-Fa-f_]*+\}|.))')
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

#: The characters the walks over one file's attribute arguments may read in
#: total: this many times the file's length, plus ``WALK_ALLOWANCE``. The
#: walks of a file's own attributes read it about once, so this leaves room
#: for three walks that run to its end, and a small file for many more.
WALK_BUDGET = 4

#: The part of the walks' budget that does not grow with the file: 64 KiB.
WALK_ALLOWANCE = 1 << 16

#: The most characters of an attribute a hit shows; a longer one is cut.
SHOWN_WIDTH = 100


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


def token_at(text: str, start: int) -> tuple[re.Match[str], int]:
    """The token that starts at a position, and where it ends.

    Args:
        text: The whole file.
        start: A position inside the file, where a token starts.

    Returns:
        The ``TOKEN`` match, and the position just after the token: after the
        matching ``*/`` of a block comment and after the closing quote and
        hashes of a raw string, or the end of the file when either never
        closes.

    Raises:
        AssertionError: If no token matches, which ``TOKEN``'s last
            alternative rules out.
    """
    token = TOKEN.match(text, start)
    if token is None:
        raise AssertionError(f"no token matches at offset {start}")
    match token.lastgroup:
        case "block_comment":
            return token, block_comment_end(text, token.end())
        case "raw_string":
            closing = '"' + token.group("hashes")
            found = text.find(closing, token.end())
            return token, len(text) if found < 0 else found + len(closing)
    return token, token.end()


def attribute_end(text: str, start: int, stop: int) -> tuple[int, int | None] | None:
    """Walk an attribute's arguments once, up to the parenthesis closing them.

    Args:
        text: The whole file.
        start: The position just after the ``(`` that opens the arguments.
        stop: The position at or after which the walk starts no token.

    Returns:
        The position just after the closing parenthesis (the end of the file
        when there is none), and the end of the first ``ignore`` word among
        the arguments outside every literal and comment, or ``None``; or
        ``None`` alone when the walk reached ``stop`` before either end.
    """
    depth = 1
    hit: int | None = None
    position = start
    while position < len(text):
        if position >= stop:
            return None
        token, position = token_at(text, position)
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
    return len(text), hit


def switched_off(text: str) -> Iterator[tuple[int, int | None]]:
    """Every ``cfg_attr`` attribute that switches a test off.

    rustfmt writes a long ``cfg_attr`` over several lines, and its arguments
    are token trees that may hold strings, raw strings, char literals and
    comments, any of which may hold a ``]``, a ``)``, a ``#`` or the word
    itself. So each opener (``#[`` or ``#![``, then ``cfg_attr`` and ``(``) is
    followed by a walk over its arguments' tokens to the parenthesis that
    closes them, and a hit is the word ``ignore`` as a token of its own
    (``ignore = "reason"``, a nested ``cfg_attr`` and the raw identifier
    ``r#ignore`` included) outside every literal and comment.

    Args:
        text: The whole file.

    Yields:
        The start of each such attribute and the end of its ``ignore``; last,
        if the budget runs out, the start of the opener whose walk it stopped,
        and ``None``.
    """
    budget = WALK_BUDGET * len(text) + WALK_ALLOWANCE
    for opener in CFG_ATTR.finditer(text):
        walked = attribute_end(text, opener.end(), opener.end() + budget)
        if walked is None:
            yield opener.start(), None
            return
        end, hit = walked
        budget -= end - opener.end()
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
                if hit.group(0).isupper():
                    continue
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
        where = f"{path}:{number}:{start - line_start + 1}"
        if end is None:
            found.append(
                f"{where}: cfg_attr attributes not read from here on: too many"
                " of the file's cfg_attr attributes never close"
            )
            continue
        shown = " ".join(text[start:end].split())
        if len(shown) > SHOWN_WIDTH:
            shown = shown[: SHOWN_WIDTH - 3] + "..."
        found.append(f"{where}: a test switched off under a condition: {shown}")
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
