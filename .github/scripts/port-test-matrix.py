#!/usr/bin/env python3
"""Check that docs/port-test-matrix.md maps every upstream test to something real.

The matrix names, for each test function of the Python SDK's test suite, the
Rust tests that cover its behaviour or the row of the README's deviations table
that explains why there is none. A mapping that names a test which was renamed
or deleted, or a deviation row that was reworded, still reads as a mapping, so
nothing but a check notices that it has stopped being one. This is that check.

It fails on:

* a row whose target cell is empty, or holds neither Rust tests nor a
  deviation, and an excluded row without a reason;
* a Rust target ``path::name`` when ``path`` has no ``fn name`` carrying a test
  attribute (a directory is searched recursively; a directory that does not
  exist yet is reported and skipped, which is how a crate another change adds
  is named before it lands);
* a quoted deviation that is not the first cell of a row of the deviations
  table in README.md;
* per-file counts that differ from the ones the matrix states, and an upstream
  test named twice.

A row is one of three kinds: mapped to Rust tests, mapped to a deviation row,
or excluded with a reason (the functions of upstream files that test the Python
repository's own tooling).

With ``--upstream <checkout>`` it also checks that every upstream ``test_*``
function has exactly one row and that no row names a function upstream does not
define.

Run it from the repository root.
"""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

MATRIX = Path("docs/port-test-matrix.md")
README = Path("README.md")
DEVIATIONS_HEADING = "## Deviations from the Python SDK"

#: A section of the matrix: ``## `tests/test_x.py` ``.
SECTION = re.compile(r"^## `(tests/test_\w+\.py)`\s*$")
#: A Rust target: a backticked ``path::function``.
RUST_TARGET = re.compile(r"`([\w./-]+)::(\w+)`")
#: A deviation target: ``Deviation: "<first cell of a README row>"``.
DEVIATION_TARGET = re.compile(r'^Deviation: "(.+)"$')
#: How an excluded row's target starts; the file's reason follows.
EXCLUDED_PREFIX = "Excluded: "
#: A test function defined at module level of an upstream file.
UPSTREAM_TEST = re.compile(r"^(?:async )?def (test_\w+)\(", re.MULTILINE)
#: An attribute that makes the function after it a test.
TEST_ATTRIBUTE = re.compile(r"#\[(?:[\w:]+::)?test\b")


@dataclass
class Row:
    """One upstream test and what covers it."""

    file: str
    line: int
    name: str
    cases: str
    target: str


@dataclass
class Counts:
    """The numbers the matrix states for one upstream file."""

    functions: int
    rust: int
    deviation: int
    excluded: int


@dataclass
class Matrix:
    """Everything read from the matrix file."""

    counts: dict[str, Counts] = field(default_factory=dict)
    rows: list[Row] = field(default_factory=list)


def table_cells(line: str) -> list[str] | None:
    """The cells of a Markdown table row, or ``None`` for any other line.

    Args:
        line: One line of Markdown.

    Returns:
        The stripped cells, or ``None`` when the line is not a data row (the
        separator row included).
    """
    stripped = line.strip()
    if not (stripped.startswith("|") and stripped.endswith("|")):
        return None
    cells = [cell.strip() for cell in stripped[1:-1].split("|")]
    if all(set(cell) <= set("-: ") for cell in cells):
        return None
    return cells


def read_matrix(text: str) -> Matrix:
    """Parse the matrix.

    Args:
        text: The contents of docs/port-test-matrix.md.

    Returns:
        The stated counts and every row.
    """
    matrix = Matrix()
    section: str | None = None
    for number, line in enumerate(text.split("\n"), start=1):
        if line.startswith("## "):
            match = SECTION.match(line)
            section = match.group(1) if match else line[3:].strip()
            continue
        cells = table_cells(line)
        if cells is None or section is None:
            continue
        if section == "Counts" and cells[0].startswith("`tests/"):
            matrix.counts[cells[0].strip("`")] = Counts(*(int(cell) for cell in cells[1:5]))
        elif section.startswith("Excluded") and cells[0].startswith("`tests/"):
            reason = EXCLUDED_PREFIX + (cells[2] if len(cells) > 2 else "")
            matrix.rows.append(Row(cells[0].strip("`"), number, cells[1].strip("`"), "", reason))
        elif section.startswith("tests/") and cells[0].startswith("`test_"):
            target = cells[2] if len(cells) > 2 else ""
            matrix.rows.append(Row(section, number, cells[0].strip("`"), cells[1], target))
    return matrix


def deviation_rows(readme: str) -> set[str]:
    """The first cells of the README's deviations table.

    Args:
        readme: The contents of README.md.

    Returns:
        Every first cell of that table, header excluded.
    """
    _, found, rest = readme.partition(f"\n{DEVIATIONS_HEADING}\n")
    if not found:
        return set()
    firsts: set[str] = set()
    for line in rest.split("\n"):
        if line.startswith("## "):
            break
        cells = table_cells(line)
        if cells is not None:
            firsts.add(cells[0])
    firsts.discard("Python SDK")
    return firsts


def defines_test(path: Path, name: str) -> bool:
    """Whether ``path`` (a file, or a directory searched recursively) has a test
    function called ``name``.

    A function counts when an attribute such as ``#[test]`` or
    ``#[tokio::test]`` opens a line of the block directly above it, which ends
    at a blank line or at the end of the previous item, so a helper of the same
    name does not.

    Args:
        path: A Rust source file or a directory of them.
        name: The function name.

    Returns:
        True when such a test exists.
    """
    files = sorted(path.rglob("*.rs")) if path.is_dir() else [path]
    signature = re.compile(rf"^\s*(?:pub(?:\([\w:]+\))?\s+)?(?:async\s+)?fn\s+{name}\s*[(<]")
    for file in files:
        lines = file.read_text(encoding="utf-8").split("\n")
        for index, line in enumerate(lines):
            if not signature.match(line):
                continue
            above = index - 1
            while above >= 0:
                text = lines[above].strip()
                if not text or text.endswith(("}", ";")):
                    break
                if TEST_ATTRIBUTE.match(text):
                    return True
                above -= 1
    return False


def check_row(row: Row, deviations: set[str], pending: set[str]) -> tuple[str | None, list[str]]:
    """Check one row's target.

    Args:
        row: The row.
        deviations: The first cells of the README's deviations table.
        pending: Filled with the directories named by targets that do not
            exist yet.

    Returns:
        The row's kind (``"rust"``, ``"deviation"`` or ``"excluded"``,
        ``None`` when it is none of them) and one message per fault.
    """
    where = f"{MATRIX}:{row.line}: {row.file}::{row.name}"
    if row.target.startswith(EXCLUDED_PREFIX):
        if not row.target.removeprefix(EXCLUDED_PREFIX).strip():
            return "excluded", [f"{where}: excluded without a reason"]
        return "excluded", []
    faults: list[str] = []
    if not row.cases.isdigit() or int(row.cases) < 1:
        faults.append(f"{where}: the case count {row.cases!r} is not a positive number")
    if not row.target:
        return None, [*faults, f"{where}: no target"]

    deviation = DEVIATION_TARGET.match(row.target)
    if deviation:
        if deviation.group(1) not in deviations:
            faults.append(
                f"{where}: {deviation.group(1)!r} is not the first cell of a row of "
                f"{README}'s deviations table"
            )
        return "deviation", faults

    targets = RUST_TARGET.findall(row.target)
    leftover = RUST_TARGET.sub("", row.target).replace(",", "").strip()
    if not targets or leftover:
        return None, [*faults, f"{where}: {row.target!r} is neither Rust tests nor a deviation"]
    for path_text, name in targets:
        path = Path(path_text)
        if not path.exists():
            if path.suffix == "":
                pending.add(path_text)
                continue
            faults.append(f"{where}: {path_text} does not exist")
        elif not defines_test(path, name):
            faults.append(f"{where}: {path_text} has no test function `{name}`")
    return "rust", faults


def check_upstream(matrix: Matrix, upstream: Path) -> list[str]:
    """Compare the rows with the test functions the upstream files define.

    Every upstream ``test_*`` function must have exactly one row - mapped to a
    Rust test, mapped to a deviation, or excluded with a reason - and every row
    must name an upstream function. A second row for one function is reported
    by :func:`main` whether or not upstream is given.

    Args:
        matrix: The parsed matrix.
        upstream: A checkout of the upstream repository.

    Returns:
        One message per fault.
    """
    files = sorted(
        path.relative_to(upstream).as_posix() for path in upstream.glob("tests/test_*.py")
    )
    if not files:
        return [f"{upstream}: no tests/test_*.py found"]
    defined = {
        (file, name)
        for file in files
        for name in UPSTREAM_TEST.findall((upstream / file).read_text(encoding="utf-8"))
    }
    named = {(row.file, row.name) for row in matrix.rows}
    faults = [f"{MATRIX}: upstream {file}::{name} has no row" for file, name in defined - named]
    faults.extend(
        f"{MATRIX}: {file}::{name} is not an upstream test" for file, name in named - defined
    )
    faults.sort()
    faults.extend(
        f"{MATRIX}: upstream {file} has no line in the Counts table"
        for file in files
        if file not in matrix.counts
    )
    return faults


def main(argv: list[str]) -> int:
    """Check the matrix, and with ``--upstream`` compare it with upstream.

    Args:
        argv: The command-line arguments, without the program name.

    Returns:
        The process exit status: 0 when every check passes.
    """
    parser = argparse.ArgumentParser(description="Check docs/port-test-matrix.md.")
    parser.add_argument("--upstream", type=Path, help="a checkout of typesafe-sdk-python")
    arguments = parser.parse_args(argv)

    matrix = read_matrix(MATRIX.read_text(encoding="utf-8"))
    deviations = deviation_rows(README.read_text(encoding="utf-8"))
    faults: list[str] = []
    if not deviations:
        faults.append(f"{README}: no {DEVIATIONS_HEADING!r} table")
    if not matrix.counts:
        faults.append(f"{MATRIX}: no Counts table")

    pending: set[str] = set()
    tally: dict[str, Counts] = {}
    seen: set[tuple[str, str]] = set()
    for row in matrix.rows:
        if (row.file, row.name) in seen:
            faults.append(f"{MATRIX}:{row.line}: {row.file}::{row.name} has a second row")
        seen.add((row.file, row.name))
        kind, row_faults = check_row(row, deviations, pending)
        faults.extend(row_faults)
        counts = tally.setdefault(row.file, Counts(0, 0, 0, 0))
        counts.functions += 1
        counts.rust += kind == "rust"
        counts.deviation += kind == "deviation"
        counts.excluded += kind == "excluded"

    for file in sorted(set(matrix.counts) | set(tally)):
        stated, counted = matrix.counts.get(file), tally.get(file, Counts(0, 0, 0, 0))
        if stated is None:
            faults.append(f"{MATRIX}: {file} has rows but no line in the Counts table")
        elif stated != counted:
            faults.append(
                f"{MATRIX}: {file} states {stated.functions} functions / {stated.rust} Rust / "
                f"{stated.deviation} deviation / {stated.excluded} excluded, the rows give "
                f"{counted.functions} / {counted.rust} / {counted.deviation} / "
                f"{counted.excluded}"
            )

    if arguments.upstream is not None:
        faults.extend(check_upstream(matrix, arguments.upstream))

    for fault in faults:
        print(fault)
    for path in sorted(pending):
        print(f"note: {path} does not exist yet; its targets are not checked")
    rust = sum(counts.rust for counts in tally.values())
    deviation = sum(counts.deviation for counts in tally.values())
    excluded = sum(counts.excluded for counts in tally.values())
    summary = (
        f"{len(matrix.rows)} upstream tests in {len(tally)} files: {rust} to Rust tests, "
        f"{deviation} to deviations, {excluded} excluded as Python tooling"
    )
    if faults:
        print(f"\n{len(faults)} fault(s); {summary}")
        return 1
    print(summary)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
