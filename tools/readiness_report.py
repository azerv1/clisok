"""Release readiness report: readiness_report.py <tag> [--strict] > READINESS.md.

Collects what CI recorded for the tagged commit: the conclusion of every workflow run on
it, and the evidence-* records those runs uploaded (fuzzing time, mutation score,
benchmarks, reproducibility). Writes Markdown listing what ran, with what result, and
what is missing. Nothing is filled in: a check without a record is reported as missing.

Only the exact tagged commit qualifies. Long-running checks are informational.
--strict exits 1 when exact-commit CI or release SBOMs are missing or failing.

Needs the gh CLI with GH_TOKEN (actions: read) and GH_REPO. Optional environment:
TOOLCHAIN (the release toolchain, as text), ASSETS (the directory of release files, to
list the SBOMs).
Stdlib only.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

Record = dict[str, Any]


def gh(*args: str) -> str:
    """Run the gh CLI and return its stdout."""
    exe = shutil.which("gh")
    if exe is None:
        sys.exit("readiness_report: gh not found on PATH")
    result = subprocess.run(
        [exe, *args], capture_output=True, text=True, encoding="utf-8", check=True
    )
    return result.stdout


def git(*args: str) -> str:
    """Run git and return its stdout, stripped."""
    exe = shutil.which("git")
    if exe is None:
        sys.exit("readiness_report: git not found on PATH")
    result = subprocess.run(
        [exe, *args], capture_output=True, text=True, encoding="utf-8", check=True
    )
    return result.stdout.strip()


def commits_of(tag: str) -> list[str]:
    """Only the exact tagged commit qualifies, including for version bumps."""
    return [git("rev-parse", tag + "^{commit}")]


WORKFLOWS = {
    ".github/workflows/ci.yml": "ci",
    ".github/workflows/fuzz.yml": "fuzz",
    ".github/workflows/mutation.yml": "mutation",
    ".github/workflows/bench.yml": "bench",
    ".github/workflows/reproducible.yml": "repro",
    ".github/workflows/assurance.yml": "assurance",
}


def pages(endpoint: str) -> list[Record]:
    """Read every API page."""
    return json.loads(gh("api", "--paginate", "--slurp", endpoint))


def workflow_runs(commits: list[str]) -> list[Record]:
    """Trusted exact-SHA runs; PR evidence cannot authorize a release."""
    sha = commits[0]
    runs = [
        r for page in pages("repos/{owner}/{repo}/actions/runs?per_page=100&head_sha=" + sha)
        for r in page.get("workflow_runs", [])
        if r.get("head_sha") == sha and r.get("path") in WORKFLOWS
        and r.get("event") in {"push", "schedule", "workflow_dispatch"}
        and r.get("head_repository", {}).get("full_name") == os.environ["GH_REPO"]
    ]
    latest = {r["id"]: r for r in sorted(runs, key=lambda r: r.get("run_attempt", 1))}
    return sorted(latest.values(), key=lambda r: (r["id"], r.get("run_attempt", 1)), reverse=True)


def evidence(runs: list[Record]) -> list[Record]:
    """The evidence-* records uploaded by those runs, each tagged with its workflow."""
    records: list[Record] = []
    with tempfile.TemporaryDirectory() as tmp:
        for run in runs:
            if run.get("status") != "completed":
                continue
            run_id = str(run["id"])
            artifacts = [
                a for page in pages("repos/{owner}/{repo}/actions/runs/" + run_id + "/artifacts?per_page=100")
                for a in page.get("artifacts", [])
            ]
            seen: set[str] = set()
            for artifact in artifacts:
                name = artifact["name"]
                if name in seen or not name.startswith("evidence-") or artifact.get("expired") or "/" in name or "\\" in name:
                    continue
                seen.add(name)
                target = Path(tmp, run_id, name)
                gh("run", "download", run_id, "--name", name, "--dir", str(target))
                for path in sorted(target.glob("*.json")):
                    record = json.loads(path.read_text(encoding="utf-8"))
                    if record.get("commit") != run["head_sha"]:
                        continue
                    if record.get("check") != WORKFLOWS[run["path"]]:
                        continue
                    if record.get("run_attempt", 1) != run.get("run_attempt", 1):
                        continue
                    record["workflow"] = run["path"]
                    record["workflow_result"] = run.get("conclusion")
                    records.append(record)
    return records


def ci_conclusion(runs: list[Record]) -> str:
    """Older successes never mask newer failures or unfinished attempts."""
    ci = sorted(
        [r for r in runs if r.get("path") == ".github/workflows/ci.yml"],
        key=lambda r: (r["id"], r.get("run_attempt", 1)), reverse=True,
    )
    if not ci:
        return "missing"
    run = ci[0]
    if run.get("status") != "completed":
        return str(run.get("status") or "unknown")
    return str(run.get("conclusion") or "unknown")


def fuzz_summary(records: list[Record]) -> tuple[list[str], float]:
    """Report lines, and CPU-hours spent by successful release-candidate runs."""
    fuzz = [r for r in records if r.get("check") == "fuzz"]
    if not fuzz:
        return ["- Fuzzing: **missing** (no fuzz record for these commits)"], 0.0
    lines = ["- Fuzzing:"]
    rc_cpu = 0.0
    for r in fuzz:
        hours = r.get("cpu_seconds", 0) / 3600
        lines.append(
            f"  - {r.get('target')} ({r.get('trigger')}): {r.get('seconds', 0):.1f} elapsed seconds, "
            f"{hours:.2f} measured CPU-h, {r.get('result')}"
        )
        if r.get("mode") == "release-candidate" and r.get("result") == "success":
            rc_cpu += hours
    return lines, rc_cpu


def mutation_summary(records: list[Record]) -> tuple[list[str], bool]:
    """Report lines, and whether a passing mutation score exists."""
    mutation = [r for r in records if r.get("check") == "mutation"]
    if not mutation:
        return ["- Mutation testing: **missing** (no record for these commits)"], False
    r = mutation[0]
    line = (
        f"- Mutation testing ({r.get('tool')}): {r.get('score')}% detected "
        f"({r.get('detected')}/{r.get('tested')}), floor {r.get('floor')}%, "
        f"{r.get('result')}"
    )
    return [line], r.get("result") == "success"


def other_summary(records: list[Record]) -> list[str]:
    """Benchmarks and reproducibility, reported as found."""
    lines = []
    for check, title in (("assurance", "Coverage and Miri"), ("bench", "Benchmarks"), ("repro", "Reproducibility")):
        found = [r for r in records if r.get("check") == check]
        if not found:
            lines.append(f"- {title}: **missing** (no record for these commits)")
            continue
        r = {k: v for k, v in found[0].items() if k not in {"check", "commit"}}
        lines.append(f"- {title}: `{json.dumps(r, sort_keys=True)}`")
    return lines


def sbom_lines() -> list[str]:
    """The SBOMs among the release files, if ASSETS names their directory."""
    assets = os.environ.get("ASSETS")
    if not assets:
        return ["- SBOM: not checked (ASSETS not set)"]
    sboms = sorted(p.name for p in Path(assets).glob("*.cdx.json"))
    return [f"- SBOM (CycloneDX): {', '.join(sboms) or '**missing**'}"]


def main() -> None:
    """Gather, write Markdown to stdout, and judge in --strict mode."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("tag")
    parser.add_argument("--strict", action="store_true")
    parser.add_argument("--wait-seconds", type=int, default=0)
    args = parser.parse_args()
    tag, strict = args.tag, args.strict
    commits = commits_of(tag)
    deadline = time.monotonic() + args.wait_seconds
    while True:
        runs = workflow_runs(commits)
        state = ci_conclusion(runs)
        if state not in {"missing", "queued", "in_progress", "waiting", "requested", "pending"}:
            break
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            break
        time.sleep(min(15, remaining))
    evidence_note = ""
    try:
        records = evidence(runs)
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError):
        records = []
        evidence_note = "- Background evidence could not be read (informational)."
    ci = ci_conclusion(runs)
    fuzz_lines, rc_cpu = fuzz_summary(records)
    mutation_lines, _ = mutation_summary(records)
    toolchain = os.environ.get("TOOLCHAIN", "not recorded")
    report = [
        f"# Release readiness: {tag}",
        "",
        "Commits: "
        + ", ".join(f"`{c}`" for c in commits)
        + f". Toolchain: {toolchain}.",
        "",
        "## Results (background checks do not gate publication)",
        "",
        f"- CI (native tests, lint, package/workflow checks, offline security scans): {ci}",
        evidence_note,
        *fuzz_lines,
        f"- Extended fuzzing: {rc_cpu:.2f} measured CPU-hours (informational)",
        *mutation_lines,
        *other_summary(records),
        *sbom_lines(),
        "",
        "## Workflow runs on these commits",
        "",
        *(
            [
                f"- {r['name']} ({r['event']}, {r['head_sha'][:7]}): {r.get('conclusion')}"
                for r in runs
            ]
            or ["- (none)"]
        ),
        "",
        "## Not covered by this report",
        "",
        "- No independent reviewer or approval is required.",
        "- Signing uses the maintainer's Minisign key; no external signing service.",
        "- Push-to-release goal: 15–30 minutes; not yet measured.",
        "- Native code signing: binaries are not Authenticode-signed or notarized.",
    ]
    print("\n".join(report))
    problems = []
    if ci != "success":
        problems.append(f"CI: {ci}")
    assets = os.environ.get("ASSETS")
    if not assets or not list(Path(assets).glob("*.cdx.json")):
        problems.append("release SBOMs missing")
    if strict and problems:
        sys.exit("readiness_report: not ready: " + "; ".join(problems))


if __name__ == "__main__":
    main()
