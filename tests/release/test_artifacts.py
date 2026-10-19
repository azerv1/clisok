import hashlib
import io
import json
import subprocess
import tarfile
import zipfile
from pathlib import Path
from typing import Any

import pytest
import yaml
from conftest import ROOT, load_tool


def workflow(name: str) -> dict[str, Any]:
    return yaml.load((ROOT / ".github/workflows" / name).read_text(encoding="utf-8"), Loader=yaml.BaseLoader)


def test_release_cannot_announce_before_verified_artifacts() -> None:
    release = workflow("release.yml")["jobs"]
    assert set(release["sign"]["needs"]) == {"plan", "build", "sbom"}
    assert set(release["smoke"]["needs"]) == {"plan", "sign"}
    assert set(release["release"]["needs"]) == {"plan", "smoke"}
    assert "needs.smoke.result == 'success'" in release["release"]["if"]
    assert "--strict" in str(release["sign"])
    ci = workflow("ci.yml")
    assert "tags" in ci["on"]["push"]
    assert set(ci["jobs"]["ci-ok"]["needs"]) == {"quality", "test", "security"}
    protection = json.loads((ROOT / ".github/branch-protection.json").read_text())
    assert protection["required_pull_request_reviews"] is None
    assert protection["required_status_checks"]["contexts"] == ["ci-ok"]


def test_only_current_release_platforms_are_claimed() -> None:
    declared = json.loads((ROOT / ".github/platforms.json").read_text())
    actual = workflow("release.yml")["jobs"]["build"]["strategy"]["matrix"]["include"]
    assert {(p["runner"], p["target"]) for p in declared["release"]} == {
        (p["os"], p["target"]) for p in actual
    }
    assert set(workflow("action-test.yml")["jobs"]["test"]["strategy"]["matrix"]["os"]) == {
        p["runner"] for p in declared["release"]
    }


def test_no_hosted_analysis_or_approval_requirement() -> None:
    prohibited = ("sigstore/", "slsa-framework/", "step-security/harden-runner",
                  "github/codeql-action", "ossf/scorecard-action", "google/osv-scanner-action")
    for path in (ROOT / ".github/workflows").glob("*.yml"):
        content = path.read_text(encoding="utf-8")
        assert all(item not in content for item in prohibited), path.name
        assert "security-events: write" not in content
        assert "environment:" not in content
    assert "unshare --net" in str(workflow("ci.yml"))


@pytest.mark.parametrize("state", ["modified", "missing", "duplicate"])
def test_checksums_reject_modified_missing_or_duplicate_archives(tmp_path: Path, state: str) -> None:
    name = "clisok-test.zip"
    (tmp_path / name).write_bytes(b"archive")
    digest = hashlib.sha256(b"archive").hexdigest()
    line = f"{digest}  {name}\n"
    if state == "modified":
        (tmp_path / name).write_bytes(b"tampered")
    elif state == "missing":
        line = f"{digest}  other.zip\n"
    else:
        line *= 2
    (tmp_path / "SHA256SUMS").write_text(line)
    with pytest.raises(ValueError):
        load_tool("action_install").verify_checksum(tmp_path, name)


@pytest.mark.parametrize("kind", ["link", "duplicate", "oversized"])
def test_archive_extraction_rejects_unsafe_members(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, kind: str,
) -> None:
    helper = load_tool("action_install")
    archive = tmp_path / "clisok.tar.gz"
    with tarfile.open(archive, "w:gz") as tar:
        member = tarfile.TarInfo("clisok")
        if kind == "link":
            member.type = tarfile.SYMTYPE
            member.linkname = "/unexpected"
        elif kind == "oversized":
            monkeypatch.setattr(helper, "MAX_EXE_BYTES", 1)
            member.size = 2
        else:
            member.size = 2
        tar.addfile(member, io.BytesIO(b"xx") if member.isfile() else None)
        if kind == "duplicate":
            tar.addfile(member, io.BytesIO(b"xx"))
    monkeypatch.setattr(helper, "run", lambda *_: "")
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    (tmp_path / "SHA256SUMS").write_text(f"{digest}  {archive.name}\n")
    public = tmp_path / "minisign.pub"
    public.write_text("fixture")
    with pytest.raises(ValueError):
        helper.install_archive(tmp_path, archive.name, tmp_path / "bin", public, "unused")


def test_real_signed_archive_installation_and_tampering(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    import os
    if os.environ.get("RUN_MINISIGN_INTEGRATION") != "1":
        pytest.skip("enable pinned public tool acquisition")
    security = load_tool("security_checks")
    monkeypatch.setattr(security, "CACHE", tmp_path / "tool-cache")
    minisign = str(security.install_tool("minisign"))
    public, secret = tmp_path / "minisign.pub", tmp_path / "temporary.key"
    generated = subprocess.run([minisign, "-G", "-W", "-p", str(public), "-s", str(secret)],
                               capture_output=True, check=False)
    assert generated.returncode == 0, "test key generation failed"
    monkeypatch.setenv("MINISIGN_SECRET_KEY", secret.read_text(encoding="utf-8"))
    for name in ("GITHUB_SHA", "GITHUB_REPOSITORY", "GITHUB_RUN_ID"):
        monkeypatch.setenv(name, "fixture")
    assets = tmp_path / "assets"
    assets.mkdir()
    archive = assets / "clisok-test.zip"
    with zipfile.ZipFile(archive, "w") as zipped:
        zipped.writestr("clisok.exe", b"verified-fixture")
    (assets / "app.cdx.json").write_text("{}")
    (assets / "READINESS.md").write_text("CI success")
    load_tool("release_manifest").sign_release(assets, minisign, public)
    installer = load_tool("action_install")
    binary = installer.install_archive(assets, archive.name, tmp_path / "installed", public, minisign)
    assert binary.read_bytes() == b"verified-fixture"
    archive.write_bytes(b"tampered")
    with pytest.raises(ValueError, match="checksum"):
        installer.install_archive(assets, archive.name, tmp_path / "tampered", public, minisign)
    assert not (tmp_path / "tampered").exists()


def test_tag_must_match_the_crate(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.chdir(tmp_path)
    (tmp_path / "Cargo.toml").write_text('[package]\nversion="1.2.3"\n')
    helper = load_tool("release_check")
    assert helper.version_for_tag("1.2.3") == "1.2.3"
    assert helper.version_for_tag("v1.2.3") == "1.2.3"
    for tag in ("main", "1.2.4", "--help"):
        with pytest.raises(ValueError):
            helper.version_for_tag(tag)


def test_background_checks_are_scheduled_and_never_release_dependencies() -> None:
    for name in ("assurance.yml", "fuzz.yml", "mutation.yml", "bench.yml", "reproducible.yml"):
        triggers = workflow(name)["on"]
        assert "schedule" in triggers and "workflow_dispatch" in triggers
        assert "push" not in triggers and "pull_request" not in triggers
    assurance = workflow("assurance.yml")["jobs"]
    assert {"coverage", "miri", "report"} <= set(assurance)
    assert "cargo +nightly miri test --lib" in str(assurance["miri"])


def test_bump_hook_dates_only_known_metadata(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    helper = load_tool("bump_hook")
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("CZ_PRE_NEW_VERSION", "0.4.0")
    for name in ("Cargo.toml", "Cargo.lock", "CHANGELOG.md"):
        (tmp_path / name).write_bytes(b"metadata\r\n")
    (tmp_path / "CITATION.cff").write_text('version: "0.4.0"\ndate-released: 2000-01-01\n')
    monkeypatch.setattr(helper.shutil, "which", lambda _: "git")
    commands: list[list[str]] = []

    def run(command: list[str], **_: Any) -> subprocess.CompletedProcess[bytes]:
        commands.append(command)
        return subprocess.CompletedProcess(command, 0)

    monkeypatch.setattr(helper.subprocess, "run", run)
    helper.main()
    citation = yaml.safe_load((tmp_path / "CITATION.cff").read_text())
    assert str(citation["date-released"]) != "2000-01-01"
    assert citation["version"] == "0.4.0"
    assert commands == [["git", "add", "--", "CITATION.cff", "CHANGELOG.md", "Cargo.toml", "Cargo.lock"]]
    assert b"\r" not in (tmp_path / "Cargo.toml").read_bytes()
