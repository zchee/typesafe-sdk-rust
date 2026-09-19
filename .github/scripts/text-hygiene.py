#!/usr/bin/env python3
"""Refuse tracked files holding bytes a reader cannot see.

A NUL byte in a text file once made git treat it as binary, which hid a diff
nobody then reviewed; a non-breaking space or a zero-width joiner in a source
file survives every formatter and every linter and changes what the file means
to a compiler or a shell without changing what it looks like. Nothing else in
this repository's toolchain looks for either, so this does.

Every tracked file is expected to be UTF-8 text.

Run it over the whole tree with no arguments, or over named paths.
"""

import subprocess
import sys
import unicodedata
from pathlib import Path

#: Bytes that carry no glyph and no meaning in a text file. Tab (0x09), line
#: feed (0x0A) and carriage return (0x0D) are left out: they are layout.
FORBIDDEN_BYTES: frozenset[int] = frozenset(
    {*range(0x09), 0x0B, 0x0C, *range(0x0E, 0x20), 0x7F}
)

#: Characters that render as nothing, or as an ordinary space they are not.
#:
#: Keyed by code point, never by the character itself: writing one of these
#: into this file would put in it exactly what it exists to keep out, and no
#: reader of the source could see that it had happened.
FORBIDDEN_CHARS: dict[int, str] = {
    0x00A0: "NO-BREAK SPACE",
    0x200B: "ZERO WIDTH SPACE",
    0x200E: "LEFT-TO-RIGHT MARK",
    0x200F: "RIGHT-TO-LEFT MARK",
    0x2028: "LINE SEPARATOR",
    0x2029: "PARAGRAPH SEPARATOR",
    0xFEFF: "ZERO WIDTH NO-BREAK SPACE",
}


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
    """Everything wrong with one file.

    Args:
        path: The file to read, relative to the working directory.

    Returns:
        One human-readable line per fault, empty when the file is clean.
    """
    raw = Path(path).read_bytes()

    found: list[str] = []
    for offset, byte in enumerate(raw):
        if byte in FORBIDDEN_BYTES:
            line = raw.count(b"\n", 0, offset) + 1
            found.append(f"{path}:{line}: byte 0x{byte:02X} at offset {offset}")

    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as broken:
        found.append(f"{path}: not UTF-8 at offset {broken.start}")
        return found

    # Split on the line feed alone. `str.splitlines` also breaks on U+2028 and
    # U+2029, which would consume two of the very characters being looked for
    # before they could be reported.
    for number, line in enumerate(text.split("\n"), start=1):
        for column, character in enumerate(line, start=1):
            name = FORBIDDEN_CHARS.get(ord(character))
            if name is not None:
                found.append(f"{path}:{number}:{column}: U+{ord(character):04X} {name}")
            elif unicodedata.category(character) == "Cf":
                # Every other format character: a bidirectional override, a
                # variation selector, a tag character. None of them belongs in
                # source or prose here, and each of them is invisible.
                found.append(
                    f"{path}:{number}:{column}: U+{ord(character):04X} "
                    f"{unicodedata.name(character, 'unnamed format character')}"
                )
    return found


def main(argv: list[str]) -> int:
    """Scan the named files, or every tracked file when none are named.

    Args:
        argv: Paths to scan; empty means the whole tracked tree.

    Returns:
        The process exit status: 0 when every file is clean.
    """
    paths = argv or tracked_files()
    found = [fault for path in paths for fault in faults(path)]
    for fault in found:
        print(fault)
    if found:
        print(
            f"\n{len(found)} invisible or control character(s) in {len(paths)} file(s)"
        )
        return 1
    print(f"{len(paths)} file(s) clean")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
