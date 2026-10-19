# Security

Report vulnerabilities [privately on GitHub](https://github.com/azerv1/clisok/security/advisories/new).
Only the latest release receives fixes; the maintainer aims to acknowledge reports within seven days.

## Signing setup

Generate a dedicated release key:

```sh
minisign -G -W -p minisign.pub -s release.key
```

Commit `minisign.pub` and store the complete private `release.key` contents in the
repository's `MINISIGN_SECRET_KEY` Actions secret. Keep the private key out of Git.

## Verify a download

Use a previously trusted copy of `minisign.pub`:

```sh
minisign -V -p minisign.pub -m SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS
```

Both checks must pass for the files you use. A key downloaded alongside an
untrusted archive does not establish trust. The GitHub Action verifies the
signature, archive hash and requested version before executing the binary.

Provenance is maintainer-signed metadata. Windows binaries are not Authenticode-signed.
Security scans run locally in CI without networking after public tool/database
acquisition; reports stay in Actions artifacts.

## Runtime limits

Child output, script text and decompressed launcher text are capped at 1 MiB;
excess child output is drained and reported as truncated. Unsupported or oversized
identified launchers receive partial inspection and are not run. Repairs reserve
unique backups; preview writes nothing. Headless execution is not a sandbox for
malicious executables. See [README.md](README.md#releases) for current test coverage.
