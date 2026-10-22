"""Test release helpers without touching repository settings or the user's PATH."""

import sys
from pathlib import Path
from types import ModuleType

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))


def load_tool(name: str) -> ModuleType:
    path = ROOT / "tools" / f"{name}.py"
    module = ModuleType(name)
    module.__file__ = str(path)
    exec(compile(path.read_text(encoding="utf-8"), str(path), "exec"), module.__dict__)
    return module
