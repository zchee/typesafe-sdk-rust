"""Behaviour of ``text-hygiene.py`` on probe files.

This file is scanned by the script it tests, so every character the script
refuses is written here as a code point and built with ``chr``.
"""

import re
from pathlib import Path
from types import ModuleType

import pytest

FORBIDDEN_BYTES = [*range(0x09), 0x0B, 0x0C, *range(0x0E, 0x20), 0x7F]

NAMED_CHARACTERS: dict[int, str] = {
    0x00A0: "NO-BREAK SPACE",
    0x200B: "ZERO WIDTH SPACE",
    0x200E: "LEFT-TO-RIGHT MARK",
    0x200F: "RIGHT-TO-LEFT MARK",
    0x2028: "LINE SEPARATOR",
    0x2029: "PARAGRAPH SEPARATOR",
    0xFEFF: "ZERO WIDTH NO-BREAK SPACE",
    0x202E: "RIGHT-TO-LEFT OVERRIDE",
    0x2060: "WORD JOINER",
    0xE0041: "TAG LATIN CAPITAL LETTER A",
}


def scan(module: ModuleType, directory: Path, raw: bytes) -> list[str]:
    """Write one probe file and scan it.

    Args:
        module: The loaded ``text-hygiene.py``.
        directory: Where the probe file is written.
        raw: The probe file's bytes.

    Returns:
        The script's fault lines, without the probe file's path.
    """
    path = directory / "probe.txt"
    path.write_bytes(raw)
    found: list[str] = module.faults(str(path))
    return [fault.removeprefix(str(path)) for fault in found]


@pytest.mark.parametrize("byte", FORBIDDEN_BYTES, ids=lambda byte: f"0x{byte:02X}")
def test_forbidden_byte_is_reported(
    text_hygiene: ModuleType, tmp_path: Path, byte: int
) -> None:
    """Each control byte is reported with its line and offset."""
    raw = b"clean\nab" + bytes([byte]) + b"c\n"

    found = scan(text_hygiene, tmp_path, raw)

    assert found == [f":2: byte 0x{byte:02X} at offset 8"]


@pytest.mark.parametrize(
    ("code_point", "name"),
    NAMED_CHARACTERS.items(),
    ids=[f"U+{code_point:04X}" for code_point in NAMED_CHARACTERS],
)
def test_invisible_character_is_reported_by_name(
    text_hygiene: ModuleType, tmp_path: Path, code_point: int, name: str
) -> None:
    """Each invisible character is reported at its line and column."""
    raw = ("clean\nab" + chr(code_point) + "c\n").encode()

    found = scan(text_hygiene, tmp_path, raw)

    assert found == [f":2:3: U+{code_point:04X} {name}"]


def test_layout_bytes_are_clean(text_hygiene: ModuleType, tmp_path: Path) -> None:
    """Tab, line feed and carriage return are layout, not faults."""
    raw = b"a\tb\r\nc\n"

    found = scan(text_hygiene, tmp_path, raw)

    assert found == []


def test_non_ascii_text_is_clean(text_hygiene: ModuleType, tmp_path: Path) -> None:
    """Visible non-ASCII letters and a combining mark are not faults."""
    raw = ("caf" + chr(0x00E9) + " a" + chr(0x0301) + " " + chr(0x65E5) + "\n").encode()

    found = scan(text_hygiene, tmp_path, raw)

    assert found == []


def test_non_utf8_file_is_reported(text_hygiene: ModuleType, tmp_path: Path) -> None:
    """A file that is not UTF-8 is reported at the first bad offset."""
    raw = b"ok\n\xff\x00\n"

    found = scan(text_hygiene, tmp_path, raw)

    assert found == [":2: byte 0x00 at offset 4", ": not UTF-8 at offset 3"]


def test_main_reports_and_fails(
    text_hygiene: ModuleType,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """``main`` prints each fault and a total, and returns 1."""
    dirty = tmp_path / "dirty.txt"
    dirty.write_bytes(("a" + chr(0x200B) + "\n").encode())
    clean = tmp_path / "clean.txt"
    clean.write_bytes(b"a\n")

    status = text_hygiene.main([str(dirty), str(clean)])
    captured = capsys.readouterr()

    assert status == 1
    assert captured.out == (
        f"{dirty}:1:2: U+200B ZERO WIDTH SPACE\n"
        "\n1 invisible or control character(s) in 2 file(s)\n"
    )


def test_main_passes_on_the_tracked_tree(
    text_hygiene: ModuleType,
    repository: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Every tracked file, this one included, is clean."""
    tracked = len(text_hygiene.tracked_files())

    status = text_hygiene.main([])
    captured = capsys.readouterr()

    assert status == 0
    assert re.fullmatch(rf"{tracked} file\(s\) clean\n", captured.out)
