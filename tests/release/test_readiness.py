import json
import sys
from pathlib import Path
from types import ModuleType
from typing import Any

import pytest

SHA = "a" * 40


def run_record(identifier: int, conclusion: str | None = "success", **extra: Any) -> dict[str, Any]:
    return {"id": identifier, "name": "CI", "path": ".github/workflows/ci.yml",
            "head_sha": SHA, "event": "push", "status": "completed",
            "head_repository": {"full_name": "owner/project"}, "conclusion": conclusion,
            "run_attempt": 1, **extra}


def test_bump_never_reuses_parent(readiness: ModuleType, monkeypatch: pytest.MonkeyPatch) -> None:
    calls: list[tuple[str, ...]] = []

    def git(*args: str) -> str:
        calls.append(args)
        return SHA

    monkeypatch.setattr(readiness, "git", git)
    assert readiness.commits_of("v1.0.0") == [SHA]
    assert calls == [("rev-parse", "v1.0.0^{commit}")]


@pytest.mark.parametrize("state", ["failure", "cancelled", "skipped", "timed_out"])
def test_latest_failure_masks_old_success(readiness: ModuleType, state: str) -> None:
    assert readiness.ci_conclusion([run_record(1), run_record(2, state)]) == state


def test_pending_and_forged_name_do_not_pass(readiness: ModuleType) -> None:
    assert readiness.ci_conclusion([run_record(2, status="in_progress", conclusion=None)]) == "in_progress"
    assert readiness.ci_conclusion([run_record(2, path=".github/workflows/untrusted.yml")]) == "missing"


def test_pagination_and_source_identity(readiness: ModuleType, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("GH_REPO", "owner/project")
    data = [{"workflow_runs": [run_record(1), run_record(2, head_sha="wrong")]},
            {"workflow_runs": [run_record(3), run_record(4, event="pull_request"),
                                run_record(5, head_repository={"full_name": "fork/project"})]}]

    def gh(*args: str) -> str:
        assert "--paginate" in args and "--slurp" in args
        return json.dumps(data)

    monkeypatch.setattr(readiness, "gh", gh)
    assert [r["id"] for r in readiness.workflow_runs([SHA])] == [3, 1]


@pytest.mark.parametrize("ci,sbom,passes", [("success", True, True), ("failure", True, False),
                                            ("missing", True, False), ("success", False, False)])
def test_strict_gate_has_no_campaign_requirement(
    readiness: ModuleType, monkeypatch: pytest.MonkeyPatch, tmp_path: Path,
    ci: str, sbom: bool, passes: bool,
) -> None:
    monkeypatch.setattr(sys, "argv", ["readiness_report.py", "v1.0.0", "--strict"])
    monkeypatch.setattr(readiness, "commits_of", lambda _: [SHA])
    monkeypatch.setattr(readiness, "workflow_runs", lambda _: [])
    monkeypatch.setattr(readiness, "ci_conclusion", lambda _: ci)
    monkeypatch.setattr(readiness, "evidence", lambda _: [])
    monkeypatch.setenv("ASSETS", str(tmp_path))
    if sbom:
        (tmp_path / "app-linux.cdx.json").write_text("{}", encoding="utf-8")
    if passes:
        readiness.main()
    else:
        with pytest.raises(SystemExit):
            readiness.main()


def test_evidence_deduplicates_artifacts(
    readiness: ModuleType, monkeypatch: pytest.MonkeyPatch,
) -> None:
    run = run_record(5, path=".github/workflows/fuzz.yml")
    artifact = {"name": "evidence-fuzz", "expired": False}
    monkeypatch.setattr(readiness, "pages", lambda _: [{"artifacts": [artifact, artifact]}])
    downloads = 0

    def gh(*args: str) -> str:
        nonlocal downloads
        downloads += 1
        target = Path(args[args.index("--dir") + 1])
        target.mkdir(parents=True)
        (target / "fuzz.json").write_text(
            json.dumps({"commit": SHA, "check": "fuzz", "run_attempt": 1}), encoding="utf-8",
        )
        return ""

    monkeypatch.setattr(readiness, "gh", gh)
    assert len(readiness.evidence([run])) == 1
    assert downloads == 1


def test_unreadable_background_evidence_does_not_block(
    readiness: ModuleType, monkeypatch: pytest.MonkeyPatch, tmp_path: Path,
) -> None:
    monkeypatch.setattr(sys, "argv", ["readiness_report.py", "v1.0.0", "--strict"])
    monkeypatch.setattr(readiness, "commits_of", lambda _: [SHA])
    monkeypatch.setattr(readiness, "workflow_runs", lambda _: [run_record(1)])
    monkeypatch.setenv("ASSETS", str(tmp_path))
    (tmp_path / "app.cdx.json").write_text("{}", encoding="utf-8")

    def evidence(_: Any) -> Any:
        raise ValueError("invalid background artifact")

    monkeypatch.setattr(readiness, "evidence", evidence)
    readiness.main()
