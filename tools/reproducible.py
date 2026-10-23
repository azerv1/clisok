"""Compare two clean native Linux release builds from permitted tracked inputs."""

import hashlib
import json
import os
import shutil
import subprocess
import tempfile
from pathlib import Path

TARGET = "x86_64-unknown-linux-musl"


def run(*args: str, cwd: Path | None = None, env: dict[str, str] | None = None) -> str:
    result = subprocess.run(args, cwd=cwd, env=env, capture_output=True, text=True, check=True)
    return result.stdout.strip()


def main() -> None:
    root = Path.cwd()
    names = run("git", "ls-files", "-z").split("\0")
    epoch = run("git", "show", "-s", "--format=%ct", "HEAD")
    allowed_roots = {"src", "tests", "benches"}
    allowed_files = {"Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "README.md",
                     "CHANGELOG.md", "LICENSE"}
    hashes = []
    with tempfile.TemporaryDirectory(prefix="clisok-repro-") as temporary:
        for attempt in ("first", "second"):
            build = Path(temporary) / attempt
            build.mkdir()
            for name in names:
                path = Path(name)
                if not name or path.is_absolute() or ".." in path.parts:
                    continue
                if any(part.startswith(".env") for part in path.parts):
                    continue
                if path.parts[0] not in allowed_roots and name not in allowed_files:
                    continue
                source = root / path
                if source.is_symlink():
                    raise ValueError("reproducibility inputs must be regular tracked files")
                destination = build / path
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(source, destination)
            env = {**os.environ, "SOURCE_DATE_EPOCH": epoch, "CARGO_INCREMENTAL": "0",
                   "CARGO_TARGET_DIR": str(build / "target"),
                   "RUSTFLAGS": f"--remap-path-prefix={build}=/source"}
            run("cargo", "build", "--release", "--locked", "--target", TARGET, cwd=build, env=env)
            hashes.append(hashlib.sha256((build / "target" / TARGET / "release/clisok").read_bytes()).hexdigest())
    record = {"check": "repro", "commit": os.environ.get("GITHUB_SHA", "local"),
              "run_attempt": int(os.environ.get("GITHUB_RUN_ATTEMPT", "1")),
              "target": TARGET, "sha256": hashes,
              "result": "success" if hashes[0] == hashes[1] else "failure"}
    Path("evidence").mkdir(exist_ok=True)
    Path("evidence/repro.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
    if hashes[0] != hashes[1]:
        raise ValueError("clean builds differ")


if __name__ == "__main__":
    main()
