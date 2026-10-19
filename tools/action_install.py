"""Download and verify a clisok release before running any bytes from it.

Uses the public key committed with this action, never the copy in the release.
The verified executable stays off PATH so clisok sees the runner's original PATH.
"""

import argparse
import hashlib
import os
import re
import shutil
import stat
import subprocess
import tarfile
import tempfile
import zipfile
from pathlib import Path
from typing import IO

from security_checks import install_tool

TARGETS = {"Linux": ("x86_64-unknown-linux-musl", ".tar.gz", "clisok"),
           "Windows": ("x86_64-pc-windows-msvc", ".zip", "clisok.exe")}
VERSION = re.compile(r"v?[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?")
MAX_EXE_BYTES = 128 * 1024 * 1024


def run(*args: str) -> str:
    executable = shutil.which(args[0])
    if executable is None:
        raise RuntimeError(f"{args[0]} is required")
    result = subprocess.run([executable, *args[1:]], capture_output=True, text=True, check=False)
    if result.returncode:
        raise RuntimeError(f"{args[0]} failed")
    return result.stdout.strip()


def verify_checksum(directory: Path, name: str) -> None:
    entries: dict[str, str] = {}
    for line in (directory / "SHA256SUMS").read_text(encoding="utf-8").splitlines():
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9_.+-]+)", line)
        if match is None or match[2] in entries or match[2] in {".", ".."}:
            raise ValueError("invalid or duplicate release checksum entry")
        entries[match[2]] = match[1]
    if name not in entries:
        raise ValueError("release archive is absent from the signed manifest")
    digest = hashlib.sha256((directory / name).read_bytes()).hexdigest()
    if digest != entries[name]:
        raise ValueError("release archive checksum mismatch")


def copy_executable(source: IO[bytes], destination: Path) -> None:
    with destination.open("xb") as out:
        remaining = MAX_EXE_BYTES
        while chunk := source.read(min(65536, remaining + 1)):
            if len(chunk) > remaining:
                raise ValueError("release executable exceeds the size limit")
            out.write(chunk)
            remaining -= len(chunk)
    destination.chmod(0o755)


def install_archive(
    directory: Path, archive_name: str, output: Path, public_key: Path, minisign: str,
) -> Path:
    """Verify signature and digest, then extract only one regular executable."""
    if not public_key.is_file():
        raise ValueError("the action's committed minisign.pub is required")
    run(minisign, "-V", "-p", str(public_key), "-m", str(directory / "SHA256SUMS"))
    verify_checksum(directory, archive_name)
    output.mkdir(parents=True, exist_ok=False)
    archive = directory / archive_name
    if archive_name.endswith(".zip"):
        with zipfile.ZipFile(archive) as zipped:
            members = [m for m in zipped.infolist() if Path(m.filename).name == "clisok.exe"]
            if len(members) != 1 or members[0].is_dir():
                raise ValueError("release must contain exactly one executable")
            member = members[0]
            if stat.S_ISLNK(member.external_attr >> 16) or member.file_size > MAX_EXE_BYTES:
                raise ValueError("unsafe release executable")
            binary = output / "clisok.exe"
            with zipped.open(member) as source:
                copy_executable(source, binary)
    else:
        with tarfile.open(archive) as tar:
            tar_members = [m for m in tar.getmembers() if Path(m.name).name == "clisok"]
            if len(tar_members) != 1 or not tar_members[0].isfile() or tar_members[0].size > MAX_EXE_BYTES:
                raise ValueError("release must contain exactly one regular executable")
            tar_source = tar.extractfile(tar_members[0])
            if tar_source is None:
                raise ValueError("release executable missing")
            binary = output / "clisok"
            with tar_source:
                copy_executable(tar_source, binary)
    return binary.resolve()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--assets", type=Path, help="verify existing CI assets instead of downloading")
    parser.add_argument("--version", default=os.environ.get("VERSION", ""))
    parser.add_argument("--public-key", type=Path)
    args = parser.parse_args()
    runner = os.environ.get("RUNNER_OS", "Windows" if os.name == "nt" else "Linux")
    if runner not in TARGETS or os.environ.get("RUNNER_ARCH", "X64") != "X64":
        raise ValueError("clisok release assets support Linux/Windows x86_64")
    repository = os.environ.get("ACTION_REPOSITORY") or "azerv1/clisok"
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*/[A-Za-z0-9][A-Za-z0-9_.-]*", repository):
        raise ValueError("invalid action repository")
    version = args.version
    action_ref = os.environ.get("ACTION_REF", "")
    if not version and VERSION.fullmatch(action_ref):
        version = action_ref
    if not version:
        version = run("gh", "release", "view", "-R", repository, "--json", "tagName", "--jq", ".tagName")
    if not VERSION.fullmatch(version):
        raise ValueError("release version must be a semantic version tag")
    target, extension, _ = TARGETS[runner]
    name = f"clisok-{version}-{target}{extension}"
    work = Path(tempfile.mkdtemp(prefix="clisok-", dir=os.environ.get("RUNNER_TEMP")))
    assets = args.assets
    if assets is None:
        assets = work / "download"
        run("gh", "release", "download", version, "-R", repository, "--dir", str(assets),
            "--pattern", name, "--pattern", "SHA256SUMS", "--pattern", "SHA256SUMS.minisig")
    public_key = args.public_key or Path(__file__).resolve().parents[1] / "minisign.pub"
    binary = install_archive(assets, name, work / "bin", public_key, str(install_tool("minisign")))
    if run(str(binary), "--version") != f"clisok {version.removeprefix('v')}":
        raise ValueError("downloaded clisok version does not match the requested release")
    if github_env := os.environ.get("GITHUB_ENV"):
        if "\n" in str(binary) or "\r" in str(binary):
            raise ValueError("unsafe runner temporary path")
        with Path(github_env).open("a", encoding="utf-8") as stream:
            stream.write(f"CLISOK_EXE={binary}\n")
    print(binary)


if __name__ == "__main__":
    main()
