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

#: The word that switches a test off, as a pattern: plain, or the raw
#: identifier ``r#ignore``, which rustc accepts alike.
IGNORE = r"\b(?:r#)?" + "ign" + r"ore\b"

#: A ``cfg_attr`` attribute that may switch a test off, from its outer or
#: inner opener (with the whitespace Rust allows between its tokens) through
#: its arguments to that word, which is the ``hit`` group. Where the word is
#: not among them the arguments are read to the first ``]`` or to the end of
#: the file, the group stays ``None``, and the attribute is no hit.
#:
#: The opener alone makes the whole pattern match, so ``finditer`` resumes
#: after what a match consumed and reads a file once however many openers it
#: holds. A form that searches each opener for the word instead rescans the
#: rest of the file for every opener that has none, which is quadratic.
#:
#: Rust source is richer than this: an attribute's arguments may hold a
#: string, a char literal or a comment, and a ``]`` or the word inside one is
#: read here as if it were code. So a ``]`` before the word ends the
#: arguments and hides an attribute that does switch a test off, erring
#: towards silence; and the word inside a literal is reported although it
#: switches nothing off, erring towards noise.
CFG_ATTR = re.compile(
    r"#\s*+(?:!\s*+)?\[\s*+cfg_attr\s*+\("
    r"(?:(?!" + IGNORE + r")[^\]])*+"
    r"(?P<hit>" + IGNORE + r")?"
)

#: The most characters of an attribute a hit shows; a longer one is cut.
SHOWN_WIDTH = 100


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
    for attribute in CFG_ATTR.finditer(text):
        if attribute.group("hit") is None:
            continue
        start = attribute.start()
        number += text.count("\n", counted, start)
        line_start = text.rfind("\n", counted, start) + 1 or line_start
        counted = start
        where = f"{path}:{number}:{start - line_start + 1}"
        shown = " ".join(attribute.group(0).split())
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
