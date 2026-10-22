"""Commitizen pre-bump hook: normalize known metadata and stage it."""

import os
import re
import shutil
import subprocess
from pathlib import Path


def main() -> None:
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.+-]+)?", os.environ["CZ_PRE_NEW_VERSION"]):
        raise ValueError("invalid bump version")
    for name in ("Cargo.toml", "Cargo.lock", "CHANGELOG.md"):
        path = Path(name)
        text = path.read_text(encoding="utf-8").replace("\r\n", "\n")
        path.write_text(text, encoding="utf-8", newline="\n")
    git = shutil.which("git")
    if git is None:
        raise RuntimeError("git is required")
    subprocess.run([git, "add", "--", "CHANGELOG.md", "Cargo.toml", "Cargo.lock"], check=True)


if __name__ == "__main__":
    main()
