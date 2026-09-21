"""Behaviour of ``no-placeholders.py`` on probe files.

This file is scanned by the script it tests, so every word and attribute the
script refuses is assembled from pieces here and never written whole.
"""

import time
from pathlib import Path
from types import ModuleType

import pytest

IGN = "ign" + "ore"
CFG = "#[" + "cfg" + "_attr("
INNER_CFG = "#![" + "cfg" + "_attr("
UPPER_TASK = "TO" + "DO"
UPPER_FIX = "FIX" + "ME"
UPPER_X = "X" + "XX"
LOWER_TASK = "to" + "do"
LOWER_FIX = "fix" + "me"
SWITCHED_OFF = "a test switched off under a condition"


def scan(
    module: ModuleType, directory: Path, text: str, name: str = "probe.rs"
) -> list[str]:
    """Write one probe file and scan it.

    Args:
        module: The loaded ``no-placeholders.py``.
        directory: Where the probe file is written.
        text: The probe file's contents.
        name: The probe file's name.

    Returns:
        The script's fault lines, without the probe file's path.
    """
    path = directory / name
    path.write_text(text, encoding="utf-8")
    prefix = f"{path}:"
    found: list[str] = module.faults(str(path))
    return [fault.removeprefix(prefix) for fault in found]


LINE_PATTERN_CASES: dict[str, tuple[str, list[str]]] = {
    "capital task marker outside a comment": (
        f'let s = "{UPPER_TASK}";',
        [f"1:10: a task marker: {UPPER_TASK}"],
    ),
    "capital fix marker in prose": (
        f"Read this {UPPER_FIX} first.",
        [f"1:11: a task marker: {UPPER_FIX}"],
    ),
    "capital triple-x marker in prose": (
        f"see {UPPER_X} here",
        [f"1:5: a task marker: {UPPER_X}"],
    ),
    "capital task marker after a comment opener": (
        f"// {UPPER_TASK}: fix the loop",
        [f"1:4: a task marker: {UPPER_TASK}"],
    ),
    "lower-case marker after a line comment": (
        f"let x = 1; // {LOWER_TASK} later",
        [f"1:15: a task marker in a comment: {LOWER_TASK}"],
    ),
    "mixed-case marker after a line comment with no space": (
        "//" + "To" + "Do: later",
        ["1:3: a task marker in a comment: " + "To" + "Do"],
    ),
    "mixed-case fix marker after a hash": (
        "x = 1  # " + "Fix" + "Me",
        ["1:10: a task marker in a comment: " + "Fix" + "Me"],
    ),
    "marker after a block comment opener": (
        f"/* {LOWER_FIX} */",
        [f"1:4: a task marker in a comment: {LOWER_FIX}"],
    ),
    "marker after an HTML comment opener": (
        f"<!-- {LOWER_TASK} -->",
        [f"1:6: a task marker in a comment: {LOWER_TASK}"],
    ),
    "only the word after the first opener counts": (
        f"a {LOWER_TASK} b # c {LOWER_TASK}",
        [f"1:14: a task marker in a comment: {LOWER_TASK}"],
    ),
    "two words after one opener are two hits": (
        f"# {LOWER_TASK} and {LOWER_FIX}",
        [
            f"1:3: a task marker in a comment: {LOWER_TASK}",
            f"1:12: a task marker in a comment: {LOWER_FIX}",
        ],
    ),
    "panicking macro with parentheses": (
        f"    {LOWER_TASK}!()",
        [f"1:5: a macro that panics in place of code: {LOWER_TASK}!("],
    ),
    "panicking macro with braces": (
        f"{LOWER_TASK}!{{}}",
        [f"1:1: a macro that panics in place of code: {LOWER_TASK}!{{"],
    ),
    "panicking macro with a space before brackets": (
        f"{LOWER_TASK}! []",
        [f"1:1: a macro that panics in place of code: {LOWER_TASK}! ["],
    ),
    "unimplemented macro": (
        "unimple" + 'mented!("later")',
        ["1:1: a macro that panics in place of code: " + "unimple" + "mented!("],
    ),
    "test switched off": (
        f"#[{IGN}]",
        [f"1:1: a test switched off: #[{IGN}"],
    ),
    "test switched off with a reason": (
        f'#[{IGN} = ".."]',
        [f"1:1: a test switched off: #[{IGN}"],
    ),
    "test switched off with a spaced raw identifier": (
        f"#[ r#{IGN} ]",
        [f"1:1: a test switched off: #[ r#{IGN}"],
    ),
    "test switched off with a raw identifier": (
        f"#[r#{IGN}]",
        [f"1:1: a test switched off: #[r#{IGN}"],
    ),
}


@pytest.mark.parametrize(
    ("text", "expected"),
    LINE_PATTERN_CASES.values(),
    ids=LINE_PATTERN_CASES.keys(),
)
def test_line_patterns_flag(
    no_placeholders: ModuleType, tmp_path: Path, text: str, expected: list[str]
) -> None:
    """Each line pattern reports its hit at its line and column."""
    found = scan(no_placeholders, tmp_path, text + "\n")

    assert found == expected


CLEAN_LINES: dict[str, str] = {
    "a marker word inside a longer identifier": (
        f"let {LOWER_TASK}s = 1; // {LOWER_TASK}s and {LOWER_TASK}_list"
    ),
    "a lower-case marker outside any comment": f"let {LOWER_TASK} = 1;",
    "a lower-case marker in a string with no comment": f'let s = "{LOWER_TASK}";',
    "a capital marker inside a longer word": f"{UPPER_TASK}S and {UPPER_FIX}D",
    "a fix marker prefix in a comment": f"// a {LOWER_FIX}ish name",
    "a glued marker in a comment": f"//x{LOWER_TASK}",
    "a lower-case triple-x in a comment": "// " + "x" + "xx",
    "a panicking macro name without its delimiter": f"see {LOWER_TASK}! here",
    "an attribute whose name merely starts with the word": f"#[{IGN}d]",
    "a raw identifier that merely starts with the word": f"#[r#{IGN}_me]",
    "a raw identifier that merely extends the word": f"#[r#{IGN}d]",
}


@pytest.mark.parametrize("text", CLEAN_LINES.values(), ids=CLEAN_LINES.keys())
def test_line_patterns_leave_clean(
    no_placeholders: ModuleType, tmp_path: Path, text: str
) -> None:
    """Spellings that only resemble a placeholder are not reported."""
    found = scan(no_placeholders, tmp_path, text + "\n")

    assert found == []


CFG_ATTR_CASES: dict[str, tuple[str, list[str]]] = {
    "one-line form": (
        f"#[test]\n{CFG}unix, {IGN})]\nfn t() {{}}",
        [f"2:1: {SWITCHED_OFF}: {CFG}unix, {IGN}"],
    ),
    "rustfmt's multi-line form": (
        (
            f"{CFG}\n"
            '    all(target_os = "macos", target_arch = "aarch64"),\n'
            f'    {IGN} = "reason"\n'
            ")]"
        ),
        [
            (
                f"1:1: {SWITCHED_OFF}: {CFG} "
                f'all(target_os = "macos", target_arch = "aarch64"), {IGN}'
            )
        ],
    ),
    "a line comment holding a hash inside the attribute": (
        f'{CFG}\n    target_os = "macos", // issue #123\n    {IGN}\n)]',
        [f'1:1: {SWITCHED_OFF}: {CFG} target_os = "macos", // issue #123 {IGN}'],
    ),
    "a raw string with hashes": (
        f'{CFG}target_os = r#"macos"#, {IGN})]',
        [f'1:1: {SWITCHED_OFF}: {CFG}target_os = r#"macos"#, {IGN}'],
    ),
    "a nested block comment holding a parenthesis": (
        f"{CFG}unix, /* outer /* ) nested */ still ) outer */ {IGN})]",
        [
            (
                f"1:1: {SWITCHED_OFF}: {CFG}unix, "
                f"/* outer /* ) nested */ still ) outer */ {IGN}"
            )
        ],
    ),
    "char literals holding a parenthesis and a quote": (
        f"{CFG}windows, some_attr(')', '\"'), {IGN})]",
        [f"1:1: {SWITCHED_OFF}: {CFG}windows, some_attr(')', '\"'), {IGN}"],
    ),
    "the word with a reason": (
        f'{CFG}unix, {IGN} = "reason")]',
        [f"1:1: {SWITCHED_OFF}: {CFG}unix, {IGN}"],
    ),
    "a nested cfg_attr": (
        f'{CFG}unix, cfg_attr(target_os = "macos", {IGN}))]',
        [f'1:1: {SWITCHED_OFF}: {CFG}unix, cfg_attr(target_os = "macos", {IGN}'],
    ),
    "the inner form inside a function body": (
        f"fn inner() {{\n    {INNER_CFG}unix, {IGN})]\n}}",
        [f"2:5: {SWITCHED_OFF}: {INNER_CFG}unix, {IGN}"],
    ),
    "a raw identifier": (
        f"{CFG}unix, r#{IGN})]",
        [f"1:1: {SWITCHED_OFF}: {CFG}unix, r#{IGN}"],
    ),
    "whitespace between every token": (
        "# [ " + "cfg" + f"_attr ( unix , {IGN} ) ]",
        ["1:1: " + SWITCHED_OFF + ": # [ " + "cfg" + f"_attr ( unix , {IGN}"],
    ),
    "a quoted opener before a real attribute": (
        f'pub const OPENER: &str = "{CFG}";\n#[test]\n{CFG}unix, {IGN})]\nfn t() {{}}',
        [f"3:1: {SWITCHED_OFF}: {CFG}unix, {IGN}"],
    ),
    "a commented opener with a stray quote before a real attribute": (
        f'// see {CFG}" for the form\n#[test]\n{CFG}unix, {IGN})]\nfn t() {{}}',
        [f"3:1: {SWITCHED_OFF}: {CFG}unix, {IGN}"],
    ),
    "two attributes on one line": (
        f"fn a() {{}} {CFG}unix, {IGN})] {CFG}windows, {IGN})]",
        [
            f"1:11: {SWITCHED_OFF}: {CFG}unix, {IGN}",
            f"1:37: {SWITCHED_OFF}: {CFG}windows, {IGN}",
        ],
    ),
}


@pytest.mark.parametrize(
    ("text", "expected"),
    CFG_ATTR_CASES.values(),
    ids=CFG_ATTR_CASES.keys(),
)
def test_cfg_attr_flags(
    no_placeholders: ModuleType, tmp_path: Path, text: str, expected: list[str]
) -> None:
    """A test switched off under a condition is reported at its opener."""
    found = scan(no_placeholders, tmp_path, text + "\n")

    assert found == expected


@pytest.mark.parametrize("name", ["probe.rs", "probe.md"])
def test_cfg_attr_quoted_whole_is_flagged_in_every_kind_of_file(
    no_placeholders: ModuleType, tmp_path: Path, name: str
) -> None:
    """A complete attribute quoted in a comment is reported, by decision."""
    text = f"// see {CFG}unix, {IGN})] for the form\n"

    found = scan(no_placeholders, tmp_path, text, name)

    assert found == [f"1:8: {SWITCHED_OFF}: {CFG}unix, {IGN}"]


CFG_ATTR_CLEAN: dict[str, str] = {
    "a path": f'{CFG}windows, path = "x.rs")]',
    "the word as a lifetime": (
        f"{CFG}unix, allow(dead_code))]\nfn f<'{IGN}>(v: &'{IGN} str) {{}}"
    ),
    "the word inside a longer name": f"{CFG}unix, allow(dead_code))]\nlet {IGN}d = 1;",
    "the attribute's text without its opener": f'let s = "cfg_attr(unix, {IGN})";',
}


@pytest.mark.parametrize("text", CFG_ATTR_CLEAN.values(), ids=CFG_ATTR_CLEAN.keys())
def test_cfg_attr_leaves_clean(
    no_placeholders: ModuleType, tmp_path: Path, text: str
) -> None:
    """An attribute that does not switch a test off is not reported."""
    found = scan(no_placeholders, tmp_path, text + "\n")

    assert found == []


CFG_ATTR_HIDDEN: dict[str, str] = {
    "a closing bracket inside a string": f'{CFG}feature = "a]b", {IGN})]',
    "a closing bracket inside a block comment": f"{CFG}unix, /* ] */ {IGN})]",
}


@pytest.mark.parametrize("text", CFG_ATTR_HIDDEN.values(), ids=CFG_ATTR_HIDDEN.keys())
def test_cfg_attr_hidden_by_a_bracket_is_not_reported(
    no_placeholders: ModuleType, tmp_path: Path, text: str
) -> None:
    """A known limit: a ``]`` before the word hides an attribute that does
    switch a test off.
    """
    found = scan(no_placeholders, tmp_path, text + "\n")

    assert found == []


CFG_ATTR_IN_A_LITERAL: dict[str, tuple[str, list[str]]] = {
    "the word as a feature name": (
        f'{CFG}feature = "{IGN}", path = "x.rs")]',
        [f'1:1: {SWITCHED_OFF}: {CFG}feature = "{IGN}'],
    ),
    "the word in a doc string": (
        f'{CFG}docsrs, doc = "{IGN} this")]',
        [f'1:1: {SWITCHED_OFF}: {CFG}docsrs, doc = "{IGN}'],
    ),
    "the word in a raw string ending in a backslash": (
        f'{CFG}unix, doc = r"{IGN} \\")]',
        [f'1:1: {SWITCHED_OFF}: {CFG}unix, doc = r"{IGN}'],
    ),
    "the word in byte and C strings": (
        f'{CFG}unix, doc = b"{IGN}", doc = c"{IGN}", doc = br#"{IGN}"#)]',
        [f'1:1: {SWITCHED_OFF}: {CFG}unix, doc = b"{IGN}'],
    ),
}


@pytest.mark.parametrize(
    ("text", "expected"),
    CFG_ATTR_IN_A_LITERAL.values(),
    ids=CFG_ATTR_IN_A_LITERAL.keys(),
)
def test_cfg_attr_word_in_a_literal_is_reported(
    no_placeholders: ModuleType, tmp_path: Path, text: str, expected: list[str]
) -> None:
    """A known limit: the word inside a literal is reported although it
    switches no test off, once per attribute and shown up to the word.
    """
    found = scan(no_placeholders, tmp_path, text + "\n")

    assert found == expected


def test_cfg_attr_openers_that_never_close_are_read_once(
    no_placeholders: ModuleType, tmp_path: Path
) -> None:
    """Openers that never close hold no switched-off test, and cost one pass."""
    unit = f"{CFG}\n"
    text = unit * 200
    path = tmp_path / "unclosed.rs"
    path.write_text(text, encoding="utf-8")
    # A scan that resumes at each opener rather than after what it read takes
    # about 80 s on this file, and grows with its square.
    large = unit * (440 * 1024 // len(unit))

    found = scan(no_placeholders, tmp_path, text, "unclosed.rs")
    status = no_placeholders.main([str(path)])
    began = time.perf_counter()
    on_large = scan(no_placeholders, tmp_path, large, "large.rs")
    took = time.perf_counter() - began

    assert (found, status) == ([], 0)
    assert on_large == []
    assert took < 5


def test_cfg_attr_shown_text_is_capped(
    no_placeholders: ModuleType, tmp_path: Path
) -> None:
    """A long attribute shows 97 characters and an ellipsis."""
    condition = ", ".join(f'target_os = "os{number}"' for number in range(8))
    text = f"{CFG}any({condition}), {IGN})]\n"

    found = scan(no_placeholders, tmp_path, text)

    shown = found[0].removeprefix(f"1:1: {SWITCHED_OFF}: ")
    assert len(found) == 1
    assert shown == text[:97] + "..."
    assert len(shown) == no_placeholders.SHOWN_WIDTH


def test_non_utf8_file_is_left_to_text_hygiene(
    no_placeholders: ModuleType, tmp_path: Path
) -> None:
    """A file that is not UTF-8 gives no fault here."""
    path = tmp_path / "binary.rs"
    path.write_bytes(b"\xff" + f"#[{IGN}]\n".encode())

    found = no_placeholders.faults(str(path))

    assert found == []


def test_main_reports_and_fails(
    no_placeholders: ModuleType,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """``main`` prints each hit, counts them on stderr and returns 1."""
    path = tmp_path / "probe.rs"
    path.write_text(f"#[{IGN}]\n", encoding="utf-8")

    status = no_placeholders.main([str(path)])
    captured = capsys.readouterr()

    assert status == 1
    assert captured.out == f"{path}:1:1: a test switched off: #[{IGN}\n"
    assert captured.err == "1 placeholder(s) found\n"


def test_main_passes_on_the_tracked_tree(
    no_placeholders: ModuleType,
    repository: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """The tracked tree, this file included, holds no placeholder."""
    status = no_placeholders.main([])
    captured = capsys.readouterr()

    assert (status, captured.out, captured.err) == (0, "", "")
