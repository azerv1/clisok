"""Mutation score gate: mutation_score.py [report] [floor] [--allow-empty].

Reads cargo-mutants's report (mutants.out/outcomes.json), prints the detection rate and writes evidence/mutation.json.
Exits 1 below the floor, or when no mutant was tested at all
(a broken run must not pass as a perfect one) unless --allow-empty says that's expected: a PR
whose diff holds no mutable code. Detected: caught; undetected: missed or timed out. Unviable mutants (they don't
build) are not counted.
Stdlib only, so `make mutate` and CI share it.
"""

import json
import os
import sys
from pathlib import Path

REPORT = "mutants.out/outcomes.json"
FLOOR = 80.0


def counts(report: Path) -> tuple[int, int]:
    """(detected, undetected) mutants."""
    data = json.loads(report.read_text(encoding="utf-8"))
    if not data.get("end_time"):
        raise ValueError("mutation run is incomplete")
    baseline = [r for r in data.get("outcomes", []) if r.get("scenario") == "Baseline"]
    if not baseline or any(r.get("summary") != "Success" for r in baseline):
        raise ValueError("mutation baseline did not pass")
    caught, missed = int(data.get("caught", 0)), int(data.get("missed", 0))
    timeout, unviable = int(data.get("timeout", 0)), int(data.get("unviable", 0))
    if min(caught, missed, timeout, unviable) < 0:
        raise ValueError("invalid mutation counts")
    if caught + missed + timeout + unviable != data.get("total_mutants"):
        raise ValueError("mutation report totals do not agree")
    print(f"caught={caught}, missed={missed}, timeout={timeout}, unviable={unviable}")
    return caught, missed + timeout


def main() -> None:
    """Score, record, judge."""
    args = [arg for arg in sys.argv[1:] if arg != "--allow-empty"]
    allow_empty = len(args) < len(sys.argv) - 1
    report = Path(args[0] if args else REPORT)
    floor = float(args[1]) if len(args) > 1 else FLOOR
    detected, undetected = counts(report)
    tested = detected + undetected
    score = 100.0 * detected / tested if tested else 0.0
    print(f"mutation score {score:.1f}% ({detected}/{tested} detected, floor {floor:.0f}%)")
    if not tested and allow_empty:
        print("no mutants in scope")
        return
    evidence = Path("evidence")
    evidence.mkdir(exist_ok=True)
    record = {
        "check": "mutation",
        "tool": "cargo-mutants",
        "commit": os.environ.get("GITHUB_SHA", ""),
        "run_attempt": int(os.environ.get("GITHUB_RUN_ATTEMPT", "1")),
        "detected": detected,
        "tested": tested,
        "score": round(score, 1),
        "floor": floor,
        "result": "success" if tested and score >= floor else "failure",
    }
    (evidence / "mutation.json").write_text(json.dumps(record, indent=2), encoding="utf-8")
    if not tested or score < floor:
        sys.exit(1)


if __name__ == "__main__":
    main()
