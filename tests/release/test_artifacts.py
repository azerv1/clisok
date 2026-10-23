import hashlib
import io
import json
import sys
import tarfile
from pathlib import Path
from typing import Any

import pytest
import yaml
from conftest import ROOT, load_tool


def workflow(name: str) -> dict[str, Any]:
    return yaml.load((ROOT / ".github/workflows" / name).read_text(encoding="utf-8"), Loader=yaml.BaseLoader)


@pytest.mark.parametrize("sha", ["a" * 40, "ABCDEF0123" * 4])
def test_action_sha_requires_an_explicit_release(
    monkeypatch: pytest.MonkeyPatch, sha: str,
) -> None:
    helper = load_tool("action_install")
    monkeypatch.setenv("ACTION_REF", sha)
    monkeypatch.delenv("VERSION", raising=False)
    monkeypatch.setattr(sys, "argv", ["action_install"])

    def no_download(*_: str) -> str:
        raise AssertionError("SHA pin must fail before looking up a mutable release")

    monkeypatch.setattr(helper, "run", no_download)
    with pytest.raises(ValueError, match="pin a release tag, or pass `version:`"):
        helper.main()


def test_action_sha_accepts_an_explicit_release(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    helper = load_tool("action_install")
    monkeypatch.setenv("ACTION_REF", "a" * 40)
    monkeypatch.setenv("RUNNER_TEMP", str(tmp_path))
    monkeypatch.delenv("GITHUB_ENV", raising=False)
    monkeypatch.setattr(sys, "argv", ["action_install", "--assets", str(tmp_path), "--version", "0.3.0"])
    binary = tmp_path / "clisok"
    monkeypatch.setattr(helper, "install_archive", lambda *_: binary)

    def version(*args: str) -> str:
        assert args == (str(binary), "--version")
        return "clisok 0.3.0"

    monkeypatch.setattr(helper, "run", version)
    helper.main()


def test_reproducibility_builds_the_shipped_musl_target(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    helper = load_tool("reproducible")
    monkeypatch.chdir(tmp_path)
    (tmp_path / "Cargo.toml").write_text("fixture", encoding="utf-8")
    commands: list[tuple[str, ...]] = []

    def run(*args: str, cwd: Path | None = None, env: dict[str, str] | None = None) -> str:
        if args[:2] == ("git", "ls-files"):
            return "Cargo.toml"
        if args[:2] == ("git", "show"):
            return "1"
        commands.append(args)
        assert cwd is not None and env is not None
        target = "x86_64-unknown-linux-musl"
        assert args == ("cargo", "build", "--release", "--locked", "--target", target)
        binary = cwd / "target" / target / "release" / "clisok"
        binary.parent.mkdir(parents=True)
        binary.write_bytes(b"same build")
        return ""

    monkeypatch.setattr(helper, "run", run)
    helper.main()
    assert len(commands) == 2
    record = json.loads((tmp_path / "evidence/repro.json").read_text(encoding="utf-8"))
    assert record["target"] == "x86_64-unknown-linux-musl"
    assert record["result"] == "success"


def test_release_publishes_only_after_ci_and_smoke_tests() -> None:
    release = workflow("release.yml")["jobs"]
    assert set(release["smoke"]["needs"]) == {"plan", "build"}
    assert set(release["release"]["needs"]) == {"plan", "smoke", "ci"}
    assert release["ci"]["uses"] == "./.github/workflows/ci.yml"
    assert release["ci"]["permissions"]["contents"] == "read"
    assert release["release"]["if"] == (
        "always() && needs.plan.result == 'success' && needs.smoke.result == 'success' "
        "&& needs.ci.result == 'success'"
    )
    ci = workflow("ci.yml")
    assert "workflow_call" in ci["on"] and "workflow_dispatch" in ci["on"]
    assert ci["concurrency"]["group"].startswith("ci-")
    assert "tags" in ci["on"]["push"]
    assert set(ci["jobs"]["ci-ok"]["needs"]) == {"quality", "test", "audit"}
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


def test_native_tests_select_the_matrix_compiler_explicitly() -> None:
    job = workflow("ci.yml")["jobs"]["test"]
    rows = job["strategy"]["matrix"]["include"]
    assert {row["os"] for row in rows if row["toolchain"] == "1.88.0"} == {
        "ubuntu-24.04", "windows-2025",
    }
    commands = {step["run"] for step in job["steps"] if "run" in step}
    assert "rustc +${{ matrix.toolchain }} -V" in commands
    assert "cargo +${{ matrix.toolchain }} test --locked" in commands


def test_no_hosted_analysis_or_approval_requirement() -> None:
    prohibited = ("sigstore/", "slsa-framework/", "step-security/harden-runner",
                  "github/codeql-action", "ossf/scorecard-action", "google/osv-scanner-action")
    for path in (ROOT / ".github/workflows").glob("*.yml"):
        content = path.read_text(encoding="utf-8")
        assert all(item not in content for item in prohibited), path.name
        assert "security-events: write" not in content
        assert "environment:" not in content


def test_basic_security_checks_are_configured() -> None:
    ci = workflow("ci.yml")
    audit = str(ci["jobs"]["audit"])
    assert "cargo audit --deny warnings" in audit
    assert "fuzz/Cargo.lock" in audit
    assert "zizmor" in str(ci["jobs"]["quality"])
    dependabot = yaml.load(
        (ROOT / ".github/dependabot.yml").read_text(encoding="utf-8"), Loader=yaml.BaseLoader,
    )
    ecosystems = {(update["package-ecosystem"], update["directory"]) for update in dependabot["updates"]}
    assert ecosystems == {("cargo", "/"), ("cargo", "/fuzz"), ("github-actions", "/"), ("uv", "/")}


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
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    (tmp_path / "SHA256SUMS").write_text(f"{digest}  {archive.name}\n")
    with pytest.raises(ValueError):
        helper.install_archive(tmp_path, archive.name, tmp_path / "bin")


def test_valid_archive_installs_only_the_executable(tmp_path: Path) -> None:
    helper = load_tool("action_install")
    archive = tmp_path / "clisok.tar.gz"
    with tarfile.open(archive, "w:gz") as tar:
        payload = b"fixture"
        member = tarfile.TarInfo("clisok")
        member.size = len(payload)
        tar.addfile(member, io.BytesIO(payload))
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    (tmp_path / "SHA256SUMS").write_text(f"{digest}  {archive.name}\n")
    binary = helper.install_archive(tmp_path, archive.name, tmp_path / "bin")
    assert binary.read_bytes() == payload


def test_manifest_lists_every_release_file(tmp_path: Path) -> None:
    (tmp_path / "clisok-x86_64.zip").write_bytes(b"archive")
    load_tool("release_check").manifest(tmp_path)
    assert (tmp_path / "SHA256SUMS").read_text(encoding="utf-8") == (
        f"{hashlib.sha256(b'archive').hexdigest()}  clisok-x86_64.zip\n"
    )


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
    assert {"coverage", "report"} <= set(assurance)
    assert set(assurance["report"]["needs"]) == {"coverage"}


def test_fuzz_job_has_a_five_minute_total_budget() -> None:
    job = workflow("fuzz.yml")["jobs"]["fuzz"]
    assert int(job["timeout-minutes"]) <= 5
    assert int(workflow("fuzz.yml")["on"]["workflow_dispatch"]["inputs"]["seconds"]["default"]) < 300


def test_bump_hook_normalizes_only_known_metadata(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    helper = load_tool("bump_hook")
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("CZ_PRE_NEW_VERSION", "0.4.0")
    for name in ("Cargo.toml", "Cargo.lock", "CHANGELOG.md"):
        (tmp_path / name).write_bytes(b"metadata\r\n")
    monkeypatch.setattr(helper.shutil, "which", lambda _: "git")
    commands: list[list[str]] = []

    def run(command: list[str], **_: Any) -> Any:
        commands.append(command)
        return None

    monkeypatch.setattr(helper.subprocess, "run", run)
    helper.main()
    assert commands == [["git", "add", "--", "CHANGELOG.md", "Cargo.toml", "Cargo.lock"]]
    assert b"\r" not in (tmp_path / "Cargo.toml").read_bytes()
