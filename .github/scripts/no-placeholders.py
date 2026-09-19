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
from pathlib import Path

#: What opens a comment in the files this repository tracks: Rust, C and
#: CSS (``//``, ``/*``), Python, TOML, YAML and shell (``#``), Markdown and
#: HTML (``<!--``).
COMMENT = r"(?://|/\*|#|<!--)"

#: What is refused, each with the reason printed beside a hit.
#:
#: The patterns are assembled from pieces so that this file does not match
#: itself; spelled out whole, every one of them would be a hit here. The two
#: task markers are refused in capitals anywhere, and in any letter case after
#: a comment opener on the same line, as whole words only, so that an
#: identifier that merely contains one stays clean.
PLACEHOLDERS: tuple[tuple[re.Pattern[str], str], ...] = (
    (re.compile(r"\b" + "TO" + r"DO\b"), "a task marker"),
    (re.compile(r"\b" + "FIX" + r"ME\b"), "a task marker"),
    (re.compile(r"\b" + "X" + r"XX\b"), "a task marker"),
    (
        re.compile(
            COMMENT + r".*?\b(?:" + "to" + "do|" + "fix" + r"me)\b", re.IGNORECASE
        ),
        "a task marker in a comment",
    ),
    (
        re.compile(r"\b" + "to" + r"do!\s*[(\[{]"),
        "a macro that panics in place of code",
    ),
    (
        re.compile(r"\b" + "unimple" + r"mented!\s*[(\[{]"),
        "a macro that panics in place of code",
    ),
    (re.compile(r"#\[\s*" + "ign" + r"ore\b"), "a test switched off"),
    (
        re.compile(r"#\[\s*cfg_attr\s*\([^\]]*\b" + "ign" + r"ore\b"),
        "a test switched off under a condition",
    ),
)


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
