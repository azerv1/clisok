"""Acquire pinned inputs, then scan locally without security-service API calls.

Run prepare with network access, scan inside a network namespace. No .env file is read.
Only public tool/advisory downloads occur during prepare; findings stay on the runner.
"""

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import tarfile
import time
import tomllib
import urllib.request
import zipfile
from datetime import date
from pathlib import Path
from typing import Any

CACHE = Path("target/security-cache")
TOOLS = {
    "osv-scanner": (
        "https://github.com/google/osv-scanner/releases/download/v2.7.0/osv-scanner_linux_amd64",
        "160c6253e2922e9e0a6a9771c5c4c40a4b3c6090b34a3cdd5e80c96cc2e4218b", None,
    ),
    "gitleaks": (
        "https://github.com/gitleaks/gitleaks/releases/download/v8.30.1/gitleaks_8.30.1_linux_x64.tar.gz",
        "551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb", "gitleaks",
    ),
    "minisign-windows": (
        "https://github.com/jedisct1/minisign/releases/download/0.12/minisign-0.12-win64.zip",
        "37b600344e20c19314b2e82813db2bfdcc408b77b876f7727889dbd46d539479",
        "minisign-win64/x86_64/minisign.exe",
    ),
    "minisign": (
        "https://github.com/jedisct1/minisign/releases/download/0.12/minisign-0.12-linux.tar.gz",
        "9a599b48ba6eb7b1e80f12f36b94ceca7c00b7a5173c95c3efc88d9822957e73",
        "minisign-linux/x86_64/minisign",
    ),
}


def download(url: str, destination: Path) -> None:
    """Download a public input, replacing it only when the transfer completes."""
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = destination.with_suffix(destination.suffix + ".part")
    with urllib.request.urlopen(url, timeout=120) as response, temporary.open("wb") as out:
        shutil.copyfileobj(response, out)
    temporary.replace(destination)


def install_tool(name: str) -> Path:
    """Install one hash-pinned Linux executable, without extracting archive paths."""
    if name == "minisign" and os.name == "nt":
        name = "minisign-windows"
    url, digest, member = TOOLS[name]
    archive = CACHE / "downloads" / name
    if not archive.exists() or hashlib.sha256(archive.read_bytes()).hexdigest() != digest:
        download(url, archive)
    if hashlib.sha256(archive.read_bytes()).hexdigest() != digest:
        raise ValueError(f"checksum mismatch for {name}")
    binary = CACHE / "bin" / ("minisign.exe" if name == "minisign-windows" else name)
    binary.parent.mkdir(parents=True, exist_ok=True)
    if url.endswith(".zip") and member:
        with zipfile.ZipFile(archive) as zipped:
            with zipped.open(member) as zip_source, binary.open("wb") as out:
                shutil.copyfileobj(zip_source, out)
    elif member:
        with tarfile.open(archive) as tar:
            info = tar.getmember(member)
            if not info.isfile():
                raise ValueError(f"{name}: archive member is not a regular file")
            source = tar.extractfile(info)
            if source is None:
                raise ValueError(f"{name}: missing executable")
            with source, binary.open("wb") as out:
                shutil.copyfileobj(source, out)
    else:
        shutil.copyfile(archive, binary)
    binary.chmod(0o755)
    return binary.resolve()


def validate_exceptions(today: date | None = None) -> None:
    """Every advisory exception needs an owner, rationale and unexpired date."""
    today = today or date.today()
    osv = tomllib.loads(Path("osv-scanner.toml").read_text(encoding="utf-8"))
    deny = tomllib.loads(Path("deny.toml").read_text(encoding="utf-8"))
    for entry in osv.get("IgnoredVulns", []):
        reason = entry.get("reason", "")
        expiry = entry.get("ignoreUntil")
        if isinstance(expiry, str):
            expiry = date.fromisoformat(expiry)
        if not entry.get("id") or not re.match(r"@[\w-]+: .+", reason):
            raise ValueError("OSV exceptions need an id and '@owner: rationale'")
        if not isinstance(expiry, date) or expiry <= today:
            raise ValueError("OSV exception missing expiry or already expired")
    for entry in deny.get("advisories", {}).get("ignore", []):
        if not isinstance(entry, dict):
            raise ValueError("cargo-deny exceptions need an owner, expiry and rationale")
        match = re.fullmatch(r"@[\w-]+ until (\d{4}-\d{2}-\d{2}): .+", entry.get("reason", ""))
        if not entry.get("id") or not match or date.fromisoformat(match[1]) <= today:
            raise ValueError("cargo-deny exception missing owner/rationale or expired")


def run(*args: str, timeout: int | None = None) -> None:
    executable = shutil.which(args[0])
    if executable is None:
        raise RuntimeError(f"{args[0]} not found")
    subprocess.run([executable, *args[1:]], check=True, timeout=timeout)


def prepare() -> None:
    validate_exceptions()
    for tool in ("osv-scanner", "gitleaks"):
        install_tool(tool)
    download("https://osv-vulnerabilities.storage.googleapis.com/crates.io/all.zip",
             CACHE / "osv" / "osv-scalibr" / "crates.io" / "all.zip")
    run("cargo", "fetch", "--locked")
    run("cargo", "deny", "fetch")
    advisory_root = Path(os.environ.get("CARGO_HOME", str(Path.home() / ".cargo"))) / "advisory-dbs"
    snapshot: dict[str, Any] = {
        "downloaded_at": time.time(),
        "osv_sha256": hashlib.sha256(
            (CACHE / "osv/osv-scalibr/crates.io/all.zip").read_bytes()
        ).hexdigest(),
        "rustsec_directory": str(advisory_root),
    }
    (CACHE / "snapshot.json").write_text(json.dumps(snapshot, indent=2), encoding="utf-8")


def rust_sources() -> list[str]:
    """Enumerate explicit Rust inputs; avoid directory discovery and any .env path."""
    paths = []
    for directory in ("src", "tests"):
        for path in Path(directory).rglob("*.rs"):
            if any(part.startswith(".env") for part in path.parts):
                continue
            if path.is_symlink():
                raise ValueError("security analysis requires regular source files")
            if path.is_file():
                paths.append(str(path))
    if not paths:
        raise ValueError("no Rust source files to analyze")
    return sorted(paths)


def scan() -> None:
    """Never downloads: callers enforce network isolation around this command."""
    validate_exceptions()
    snapshot = json.loads((CACHE / "snapshot.json").read_text(encoding="utf-8"))
    age = time.time() - snapshot["downloaded_at"]
    if not 0 <= age <= 24 * 3600:
        raise ValueError("advisory snapshot must be no older than 24 hours")
    database = CACHE / "osv/osv-scalibr/crates.io/all.zip"
    if hashlib.sha256(database.read_bytes()).hexdigest() != snapshot["osv_sha256"]:
        raise ValueError("advisory database changed after acquisition")
    reports = Path("target/security-reports")
    reports.mkdir(parents=True, exist_ok=True)
    os.environ["OSV_SCANNER_LOCAL_DB_CACHE_DIRECTORY"] = str((CACHE / "osv").resolve())
    os.environ["SEMGREP_SEND_METRICS"] = "off"
    os.environ["SEMGREP_ENABLE_VERSION_CHECK"] = "0"
    run("cargo", "deny", "--offline", "check")
    lockfiles = ["--lockfile", "Cargo.lock"]
    if Path("fuzz/Cargo.lock").is_file():
        lockfiles += ["--lockfile", "fuzz/Cargo.lock"]
    run(str((CACHE / "bin/osv-scanner").resolve()), "scan", "source", "--offline", *lockfiles)
    # git's pathspec prevents .env blobs from reaching the scanner at all.
    log_options = "--all -- . ':(glob,exclude)**/.env*' ':(exclude).env*'"
    result = subprocess.run(
        [str((CACHE / "bin/gitleaks").resolve()), "git", ".", "--no-banner", "--redact",
         "--log-opts=" + log_options, "--report-format=json",
         "--report-path=" + str(reports / "gitleaks.json")],
        capture_output=True, check=False,
    )
    if result.returncode:
        raise RuntimeError("local secret scan failed; inspect the redacted artifact")
    run("semgrep", "scan", "--config", "security/semgrep.yml", "--metrics=off",
        "--disable-version-check", "--error", "--strict", "--oss-only", "--jobs=1",
        "--timeout=20", "--no-git-ignore", "--exclude=**/.env*", "--exclude=.env*",
        "--json-output=" + str(reports / "semgrep.json"), *rust_sources(), timeout=180)
    shutil.copyfile(CACHE / "snapshot.json", reports / "advisory-snapshot.json")


def scan_staged() -> None:
    """Pipe only permitted staged paths into the local, redacting scanner."""
    git = shutil.which("git")
    gitleaks = shutil.which("gitleaks")
    if git is None or gitleaks is None:
        raise RuntimeError("git and gitleaks must be installed")
    with subprocess.Popen(
        [git, "diff", "--cached", "--no-ext-diff", "--unified=0", "--", ".",
         ":(glob,exclude)**/.env*", ":(exclude).env*"], stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    ) as diff:
        result = subprocess.run(
            [gitleaks, "stdin", "--redact", "--no-banner"], stdin=diff.stdout,
            capture_output=True, check=False,
        )
        if diff.stdout is not None:
            diff.stdout.close()
        status = diff.wait()
    if status or result.returncode:
        raise RuntimeError("staged secret scan failed (output withheld)")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("action", choices=["prepare", "scan", "staged", "validate", "minisign"])
    action = parser.parse_args().action
    if action == "prepare":
        prepare()
    elif action == "scan":
        scan()
    elif action == "staged":
        scan_staged()
    elif action == "validate":
        validate_exceptions()
    else:
        print(install_tool("minisign"))


if __name__ == "__main__":
    main()
