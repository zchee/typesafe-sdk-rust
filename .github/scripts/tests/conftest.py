"""Fixtures that load the CI scripts, whose hyphenated names cannot be imported."""

import importlib.util
import sys
from collections.abc import Iterator
from pathlib import Path
from types import ModuleType

import pytest

SCRIPTS = Path(__file__).resolve().parent.parent
REPOSITORY = SCRIPTS.parent.parent


def load(name: str) -> ModuleType:
    """Import one script of ``.github/scripts`` by its path.

    The module is registered in ``sys.modules`` before it runs, because
    ``dataclasses`` looks a class's module up there.

    Args:
        name: The script's file name without ``.py``.

    Returns:
        The executed module.

    Raises:
        ImportError: If the script cannot be loaded.
    """
    module_name = name.replace("-", "_")
    spec = importlib.util.spec_from_file_location(module_name, SCRIPTS / f"{name}.py")
    if spec is None or spec.loader is None:
        raise ImportError(f"cannot load {name}.py from {SCRIPTS}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    spec.loader.exec_module(module)
    return module


@pytest.fixture(scope="session")
def no_placeholders() -> ModuleType:
    """The placeholder scan."""
    return load("no-placeholders")


@pytest.fixture(scope="session")
def text_hygiene() -> ModuleType:
    """The invisible and control character scan."""
    return load("text-hygiene")


@pytest.fixture(scope="session")
def port_test_matrix() -> ModuleType:
    """The upstream test matrix check."""
    return load("port-test-matrix")


@pytest.fixture
def repository(monkeypatch: pytest.MonkeyPatch) -> Iterator[Path]:
    """Run the test from the repository root, where CI runs the scripts."""
    monkeypatch.chdir(REPOSITORY)
    yield REPOSITORY
