"""Record measured /usr/bin/time usage; never infer CPU time from requested cores."""

import json
import math
import os
from pathlib import Path


def usage(path: Path) -> tuple[float, float]:
    """Read time's final user/system/elapsed line, including failed commands."""
    user, system, elapsed = map(float, path.read_text(encoding="utf-8").splitlines()[-1].split())
    if not all(math.isfinite(v) and v >= 0 for v in (user, system, elapsed)):
        raise ValueError("invalid measured fuzz usage")
    return user + system, elapsed


def main() -> None:
    target = os.environ["TARGET"]
    if not target or any(c not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-" for c in target):
        raise ValueError("unsafe fuzz target name")
    path = Path("evidence/fuzz-time.txt")
    cpu, elapsed = usage(path) if path.exists() else (0.0, 0.0)
    record = {
        "check": "fuzz", "target": target, "commit": os.environ["GITHUB_SHA"],
        "run_attempt": int(os.environ.get("GITHUB_RUN_ATTEMPT", "1")),
        "trigger": os.environ["EVENT"], "mode": os.environ.get("MODE", "nightly"),
        "seconds": elapsed, "cpu_seconds": cpu,
        "requested_seconds": int(os.environ.get("SECONDS_PER_TARGET") or 0),
        "cores": int(os.environ.get("CORES") or 0), "result": os.environ["RESULT"],
    }
    Path("evidence").mkdir(exist_ok=True)
    Path(f"evidence/fuzz-{target}.json").write_text(json.dumps(record, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
