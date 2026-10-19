import hashlib
import json
import os
import subprocess
from datetime import date
from pathlib import Path
from typing import Any

import pytest
from conftest import load_tool


@pytest.mark.parametrize("caught,missed,timeout,score", [(8, 2, 0, (8, 2)), (8, 0, 2, (8, 2))])
def test_mutation_timeouts_are_not_caught(
    tmp_path: Path, caught: int, missed: int, timeout: int, score: tuple[int, int],
) -> None:
    path = tmp_path / "outcomes.json"
    path.write_text(json.dumps({"end_time": "2026-10-18T00:00:00Z", "total_mutants": 10,
                               "caught": caught, "missed": missed, "timeout": timeout, "unviable": 0,
                               "outcomes": [{"scenario": "Baseline", "summary": "Success"}]}))
    assert load_tool("mutation_score").counts(path) == score


@pytest.mark.parametrize("report", [
    {"caught": 100},
    {"end_time": "done", "outcomes": [{"scenario": "Baseline", "summary": "Failure"}]},
    {"end_time": "done", "total_mutants": 100, "caught": 1,
     "outcomes": [{"scenario": "Baseline", "summary": "Success"}]},
])
def test_invalid_mutation_evidence_rejected(tmp_path: Path, report: dict[str, Any]) -> None:
    path = tmp_path / "outcomes.json"
    path.write_text(json.dumps(report))
    with pytest.raises(ValueError):
        load_tool("mutation_score").counts(path)


def test_fuzz_uses_measured_usage(tmp_path: Path) -> None:
    path = tmp_path / "time.txt"
    path.write_text("Command exited with nonzero status 1\n3.5 1.5 2.5\n")
    assert load_tool("fuzz_evidence").usage(path) == (5.0, 2.5)
    path.write_text("nan 0 0\n")
    with pytest.raises(ValueError):
        load_tool("fuzz_evidence").usage(path)


def test_advisory_exception_policy(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.chdir(tmp_path)
    (tmp_path / "deny.toml").write_text("[advisories]\nignore = []\n")
    osv = tmp_path / "osv-scanner.toml"
    osv.write_text('[[IgnoredVulns]]\nid="GHSA-test"\nignoreUntil=2026-10-19\nreason="@owner: explained"\n')
    helper = load_tool("security_checks")
    helper.validate_exceptions(date(2026, 10, 18))
    with pytest.raises(ValueError):
        helper.validate_exceptions(date(2026, 10, 19))
    osv.write_text('[[IgnoredVulns]]\nid="GHSA-test"\nignoreUntil=2026-10-19\nreason="no owner"\n')
    with pytest.raises(ValueError):
        helper.validate_exceptions(date(2026, 10, 18))


def test_security_scan_does_not_fetch(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.chdir(tmp_path)
    helper = load_tool("security_checks")
    monkeypatch.setattr(helper, "validate_exceptions", lambda: None)
    cache = tmp_path / "cache"
    cache.mkdir()
    monkeypatch.setattr(helper, "CACHE", cache)
    with pytest.raises(FileNotFoundError):
        helper.scan()
    monkeypatch.setattr(helper, "download", lambda *_: pytest.fail("scan must not download"))
    (cache / "snapshot.json").write_text(json.dumps({"downloaded_at": 0}))
    with pytest.raises(ValueError, match="24 hours"):
        helper.scan()


def assets(tmp_path: Path) -> tuple[Path, Path]:
    root = tmp_path / "assets"
    root.mkdir()
    for name in ("app.tar.xz", "app-linux.cdx.json", "READINESS.md"):
        (root / name).write_text("fixture", encoding="utf-8")
    public = tmp_path / "minisign.pub"
    public.write_text("public fixture", encoding="utf-8")
    return root, public


def test_signing_requires_key_and_evidence(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    root, public = assets(tmp_path)
    monkeypatch.delenv("MINISIGN_SECRET_KEY", raising=False)
    helper = load_tool("release_manifest")
    with pytest.raises(ValueError, match="configure"):
        helper.sign_release(root, "unused", public)
    monkeypatch.setenv("MINISIGN_SECRET_KEY", "not-a-real-key")
    (root / "READINESS.md").unlink()
    with pytest.raises(ValueError, match="readiness"):
        helper.sign_release(root, "unused", public)


@pytest.mark.parametrize("failed_call", [1, 2])
def test_signing_and_verification_fail_closed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, failed_call: int,
) -> None:
    root, public = assets(tmp_path)
    helper = load_tool("release_manifest")
    for name in ("GITHUB_SHA", "GITHUB_REPOSITORY", "GITHUB_RUN_ID"):
        monkeypatch.setenv(name, "fixture")
    monkeypatch.setenv("MINISIGN_SECRET_KEY", "private-fixture-do-not-print")
    calls = 0

    def run(argv: list[str], **kwargs: Any) -> subprocess.CompletedProcess[bytes]:
        nonlocal calls
        calls += 1
        assert kwargs["capture_output"] is True
        return subprocess.CompletedProcess(argv, 1 if calls == failed_call else 0, b"", b"")

    monkeypatch.setattr(helper.subprocess, "run", run)
    with pytest.raises(RuntimeError, match="signing or verification failed"):
        helper.sign_release(root, "minisign", public)
    assert calls == failed_call
    manifest = (root / "SHA256SUMS").read_text(encoding="utf-8")
    assert hashlib.sha256(b"fixture").hexdigest() in manifest
    assert os.environ["MINISIGN_SECRET_KEY"] not in manifest


def test_real_minisign_and_tampering(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    if os.environ.get("RUN_MINISIGN_INTEGRATION") != "1":
        pytest.skip("enable public tool acquisition with RUN_MINISIGN_INTEGRATION=1")
    security = load_tool("security_checks")
    monkeypatch.setattr(security, "CACHE", tmp_path / "tools")
    binary = str(security.install_tool("minisign"))
    root, public = assets(tmp_path)
    public.unlink()
    secret = tmp_path / "temporary-test.key"
    generated = subprocess.run(
        [binary, "-G", "-W", "-p", str(public), "-s", str(secret)],
        capture_output=True, check=False,
    )
    assert generated.returncode == 0, "test key generation failed"
    monkeypatch.setenv("MINISIGN_SECRET_KEY", secret.read_text(encoding="utf-8"))
    for name in ("GITHUB_SHA", "GITHUB_REPOSITORY", "GITHUB_RUN_ID"):
        monkeypatch.setenv(name, "test-fixture")
    load_tool("release_manifest").sign_release(root, binary, public)
    manifest = root / "SHA256SUMS"
    assert manifest.with_suffix(".minisig").is_file()
    for line in manifest.read_text(encoding="utf-8").splitlines():
        digest, name = line.split("  ", 1)
        assert hashlib.sha256((root / name).read_bytes()).hexdigest() == digest
    manifest.write_text(manifest.read_text(encoding="utf-8") + "tampered\n", encoding="utf-8")
    verified = subprocess.run(
        [binary, "-V", "-p", str(public), "-m", str(manifest)], capture_output=True, check=False,
    )
    assert verified.returncode != 0


def test_static_scan_targets_never_include_env_paths(monkeypatch: pytest.MonkeyPatch) -> None:
    helper = load_tool("security_checks")
    monkeypatch.setattr(Path, "rglob", lambda base, _: [
        base / "safe.rs", base / ".env.rs", base / ".env-private" / "hidden.rs",
    ])
    monkeypatch.setattr(Path, "is_file", lambda _: True)
    monkeypatch.setattr(Path, "is_symlink", lambda _: False)
    assert helper.rust_sources() == [str(Path("src/safe.rs")), str(Path("tests/safe.rs"))]
    monkeypatch.setattr(Path, "rglob", lambda *_: [])
    with pytest.raises(ValueError, match="no Rust source"):
        helper.rust_sources()
