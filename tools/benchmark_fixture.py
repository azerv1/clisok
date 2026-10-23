"""Benchmark isolated command fixtures; never scan the caller's PATH or perform repairs."""

import argparse
import json
import os
import statistics
import subprocess
import tempfile
import time
from pathlib import Path


def measure(binary: Path) -> list[dict[str, object]]:
    records: list[dict[str, object]] = []
    with tempfile.TemporaryDirectory(prefix="clisok-bench-") as temporary:
        root = Path(temporary)
        for count in (100, 1000, 10000):
            directory = root / str(count)
            directory.mkdir()
            for number in range(count):
                path = directory / (f"fixture-{number}.cmd" if os.name == "nt" else f"fixture-{number}")
                path.write_text("@rem fixture\n" if os.name == "nt" else "#!/bin/sh\n# fixture\n", encoding="utf-8")
                path.chmod(0o755)
            for jobs in (1, None):
                samples = []
                for _ in range(3):
                    start = time.perf_counter()
                    command = [str(binary), "check", "--allow-root", "--output-format", "json"]
                    if jobs:
                        command += ["--jobs", str(jobs)]
                    result = subprocess.run(
                        command, env={**os.environ, "PATH": str(directory)}, cwd=root,
                        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, check=False, timeout=180,
                    )
                    if result.returncode not in {0, 1}:
                        raise RuntimeError("fixture benchmark failed")
                    json.loads(result.stdout)
                    samples.append(time.perf_counter() - start)
                records.append({"commands": count, "jobs": jobs or "default",
                                "seconds": samples, "median_seconds": statistics.median(samples)})
    return records


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("binary", type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    output = {"check": "bench", "commit": os.environ.get("GITHUB_SHA", "local"),
              "run_attempt": int(os.environ.get("GITHUB_RUN_ATTEMPT", "1")),
              "artifact_bytes": binary.stat().st_size, "fixture_kind": "non-launcher scripts, static checks",
              "results": measure(binary)}
    Path("evidence").mkdir(exist_ok=True)
    Path("evidence/bench.json").write_text(json.dumps(output, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
