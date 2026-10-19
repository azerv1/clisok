"""Fail when CITATION.cff's version differs from the project version in Cargo.toml.

commitizen keeps them in step on `make bump` (version_files); this catches a hand edit of
either one. Stdlib only. Run by prek and the CI lint job.
"""

import re
import sys
import tomllib
from pathlib import Path


def project_version() -> str:
    """[package].version from Cargo.toml."""
    data = tomllib.loads(Path("Cargo.toml").read_text(encoding="utf-8"))
    return str(data["package"]["version"])


def main() -> None:
    """Compare and report."""
    text = Path("CITATION.cff").read_text(encoding="utf-8")
    match = re.search(r'(?m)^version:\s*"?([^"\s]+)"?\s*$', text)
    cited = match.group(1) if match else None
    expected = project_version()
    if cited != expected:
        sys.exit(
            f"check_citation: CITATION.cff version {cited!r} != Cargo.toml version {expected!r}"
        )


if __name__ == "__main__":
    main()
