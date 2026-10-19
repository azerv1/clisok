"""Validate release tags, build archives and smoke-test only disposable PATH fixtures."""

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
import zipfile
from pathlib import Path


def version_for_tag(tag: str) -> str:
    version = tomllib.loads(Path("Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
    if not re.fullmatch(r"v?[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?", tag):
        raise ValueError("release requires a semantic version tag")
    if tag.removeprefix("v") != version:
        raise ValueError("release tag must match Cargo.toml")
    return str(version)


def package(tag: str, target: str) -> None:
    version_for_tag(tag)
    if target not in {"x86_64-pc-windows-msvc", "x86_64-unknown-linux-musl"}:
        raise ValueError("unsupported release target")
    windows = target.endswith("msvc")
    executable = "clisok.exe" if windows else "clisok"
    name = f"clisok-{tag}-{target}"
    destination = Path("dist")
    destination.mkdir(exist_ok=True)
    files = [(Path("target") / target / "release" / executable, executable)]
    files += [(Path(f), f) for f in ("README.md", "CHANGELOG.md", "LICENSE")]
    if windows:
        archive = destination / f"{name}.zip"
        with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as zipped:
            for path, filename in files:
                zipped.write(path, filename)
    else:
        archive = destination / f"{name}.tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            for path, filename in files:
                tar.add(path, arcname=filename, recursive=False)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    (destination / f"{archive.name}.sha256").write_text(
        f"{digest}  {archive.name}\n", encoding="utf-8", newline="\n",
    )


def execute(binary: Path, args: list[str], env: dict[str, str], cwd: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run([str(binary), *args], env=env, cwd=cwd, capture_output=True,
                          encoding="utf-8", text=True, timeout=20, check=False)


def smoke(binary: Path, version: str) -> None:
    """No real PATH repair, registry operations or arbitrary tool execution."""
    binary = binary.resolve()
    with tempfile.TemporaryDirectory(prefix="clisok-smoke-") as temporary:
        root = Path(temporary)
        commands = root / "commands"
        commands.mkdir()
        marker = root / "unexpected-execution"
        # An ordinary script must be inspected but never executed.
        script = commands / ("do-not-run.cmd" if os.name == "nt" else "do-not-run")
        prefix = "@echo off\n" if os.name == "nt" else "#!/bin/sh\n"
        script.write_text(prefix + f'echo unsafe > "{marker}"\n', encoding="utf-8")
        script.chmod(0o755)
        env = {**os.environ, "PATH": str(commands), "CLISOK_NO_PROGRESS": "1"}
        result = execute(binary, ["--version"], env, root)
        if result.returncode or result.stdout.strip() != f"clisok {version}":
            raise ValueError("artifact version mismatch")
        result = execute(binary, ["rule", "list", "--output-format", "json"], env, root)
        if result.returncode or not json.loads(result.stdout):
            raise ValueError("artifact rule listing failed")
        for subcommand in ("list", "check"):
            result = execute(binary, [subcommand, "--no-config", "--output-format", "json"], env, root)
            if result.returncode not in {0, 1}:
                raise ValueError(f"artifact {subcommand} failed")
            report = json.loads(result.stdout)
            if report.get("command") != subcommand:
                raise ValueError("unexpected artifact report")
            if subcommand == "check":
                fixtures = [t for t in report.get("tools", []) if Path(t["path"]).name == script.name]
                if len(fixtures) != 1 or fixtures[0]["kind"]["type"] != "script" or fixtures[0]["attempts"]:
                    raise ValueError("artifact did not statically skip the non-launcher fixture")
        result = execute(binary, ["check", "--no-config", "--diff"], env, root)
        if result.returncode not in {0, 1} or marker.exists():
            raise ValueError("artifact ran a non-launcher or preview changed files")
        if shutil.which("clisok", path=env["PATH"]):
            raise ValueError("smoke fixture unexpectedly changed command resolution")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="action", required=True)
    check = sub.add_parser("tag")
    check.add_argument("tag")
    archive = sub.add_parser("package")
    archive.add_argument("tag")
    archive.add_argument("target")
    test = sub.add_parser("smoke")
    test.add_argument("binary", type=Path)
    test.add_argument("version")
    args = parser.parse_args()
    if args.action == "tag":
        print(version_for_tag(args.tag))
    elif args.action == "package":
        package(args.tag, args.target)
    else:
        smoke(args.binary, args.version)


if __name__ == "__main__":
    main()
