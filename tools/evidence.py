"""Record actual workflow states for optional background checks."""

import json
import os
import sys
from pathlib import Path


def main() -> None:
    check = sys.argv[1]
    if check not in {"assurance", "bench", "repro"}:
        raise ValueError("unknown background check")
    data = json.loads(os.environ.get("EVIDENCE_DATA", "{}"))
    record = {**data, "check": check, "commit": os.environ["GITHUB_SHA"],
              "run_attempt": int(os.environ.get("GITHUB_RUN_ATTEMPT", "1"))}
    directory = Path("evidence")
    directory.mkdir(exist_ok=True)
    (directory / f"{check}.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
