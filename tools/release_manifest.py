"""Sign and verify exact release files with a maintainer-controlled Minisign key.

The private key arrives only in MINISIGN_SECRET_KEY. Never print it or subprocess output.
Commit minisign.pub before releasing. CI uses an unencrypted dedicated signing key
stored in a GitHub Actions secret; no external signing service or reviewer is involved.
"""

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import tempfile
from pathlib import Path


def sign_release(assets: Path, executable: str, public_key: Path) -> None:
    """Sign a flat manifest, verify it, and fail closed on absent release evidence."""
    key = os.environ.get("MINISIGN_SECRET_KEY")
    if not key or not public_key.is_file():
        raise ValueError("configure MINISIGN_SECRET_KEY and commit minisign.pub before releasing")
    files = [p for p in assets.iterdir() if p.is_file() and not p.name.endswith("-dist-manifest.json")]
    if any(p.is_symlink() or "\n" in p.name or "\r" in p.name for p in files):
        raise ValueError("release assets must be regular flat files with safe names")
    if not any(p.name.endswith((".tar.xz", ".tar.gz", ".zip")) for p in files):
        raise ValueError("release archive missing")
    if not any(p.name.endswith(".cdx.json") for p in files) or not (assets / "READINESS.md").is_file():
        raise ValueError("release SBOM or readiness report missing")
    provenance = {
        "format": "maintainer-build-record-v1", "commit": os.environ["GITHUB_SHA"],
        "repository": os.environ["GITHUB_REPOSITORY"], "run_id": os.environ["GITHUB_RUN_ID"],
        "run_attempt": os.environ.get("GITHUB_RUN_ATTEMPT", "1"),
        "claim": "Maintainer-signed build metadata; not independent attestation or SLSA certification",
    }
    (assets / "provenance.json").write_text(json.dumps(provenance, indent=2) + "\n", encoding="utf-8")
    shutil.copyfile(public_key, assets / "minisign.pub")
    files = sorted(p for p in assets.iterdir() if p.is_file()
                   and p.name not in {"SHA256SUMS", "SHA256SUMS.minisig"}
                   and not p.name.endswith("-dist-manifest.json"))
    manifest = assets / "SHA256SUMS"
    lines = [f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n" for p in files]
    manifest.write_text("".join(lines), encoding="utf-8", newline="\n")
    with tempfile.TemporaryDirectory() as tmp:
        secret = Path(tmp) / "signing.key"
        secret.write_text(key, encoding="utf-8")
        secret.chmod(0o600)
        for args in (
            ["-S", "-s", str(secret), "-m", str(manifest)],
            ["-V", "-p", str(public_key), "-m", str(manifest)],
        ):
            result = subprocess.run([executable, *args], stdin=subprocess.DEVNULL,
                                    capture_output=True, check=False)
            if result.returncode:
                raise RuntimeError("Minisign signing or verification failed")
    if not manifest.with_suffix(".minisig").is_file():
        raise RuntimeError("Minisign signature missing")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("assets", type=Path)
    parser.add_argument("--minisign", default="minisign")
    parser.add_argument("--public-key", type=Path, default=Path("minisign.pub"))
    args = parser.parse_args()
    executable = shutil.which(args.minisign)
    if executable is None:
        raise RuntimeError("minisign not found")
    sign_release(args.assets, executable, args.public_key)


if __name__ == "__main__":
    main()
