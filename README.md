# clisok

Check that the executables on your system work: PATH entries, launchers and
shims, DLLs and shared libraries, interpreters, and a headless test run of each
tool that is safe to run. Runs on Windows and Linux.

```
clisok check            # check PATH entries and every tool on PATH
clisok check --output-format json  # same, as JSON on stdout
clisok check --registry --fix   # also apply safe fixes to the saved user PATH (Windows)
clisok list             # list every command and the copies it shadows; runs nothing
clisok rule list        # list every rule, its default severity and fix availability
clisok rule show CLI007 # explain a code
```

`clisok check` exits 0 when everything is fine, nonzero when it finds problems.
Every problem has a stable code like `CLI002`.

**Docs:** [azerv1.github.io/clisok](https://azerv1.github.io/clisok/) —
[usage](https://azerv1.github.io/clisok/usage.html),
[fixes](https://azerv1.github.io/clisok/fixes.html),
[error codes](https://azerv1.github.io/clisok/codes.html),
[GitHub Action](https://azerv1.github.io/clisok/github-action.html).

## Install

Download a binary from [Releases](https://github.com/azerv1/clisok/releases):
Windows x86_64 `.zip`, or a static Linux x86_64 `.tar.gz`.

CI tests Windows Server 2025 and Ubuntu 24.04; other Windows/Linux versions are
not exhaustively tested. macOS and FreeBSD remain on the [roadmap](PLAN.md).
Report bugs and feature requests through GitHub issues.

## GitHub Action

```yaml
- uses: azerv1/clisok@0.3.0
  with:
    args: check --skip some-noisy-tool
```

The step fails when `check` finds problems. Linux and Windows runners only.

## Development

Rust 1.98.1 is pinned; CI also tests Rust 1.88 as the minimum supported compiler.

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo audit                    # cargo install cargo-audit@0.22.2
cargo audit --file fuzz/Cargo.lock
cargo doc --no-deps --locked
cargo package --locked --allow-dirty
uv sync --locked --group dev
uv run ruff check tools tests/release
uv run mypy tools tests/release
uv run pytest
```

Tests use temporary PATH fixtures and disposable HKCU keys. Keep them isolated
from the real user PATH and registry settings. Use Conventional Commits.

For the first timed local CI run, use Docker and `act` from the repository root:

```sh
act push -W .github/workflows/ci.yml --matrix os:ubuntu-24.04 --matrix toolchain:1.98.1 -P ubuntu-24.04=ghcr.io/catthehacker/ubuntu:act-24.04 --action-offline-mode --concurrent-jobs 2
```

This runs quality checks, current-toolchain Linux tests, dependency audits and
the `ci-ok` gate. The full Windows/Linux and current/MSRV matrix remains required
on GitHub. Rust build outputs and uv downloads are cached; keep act's cache server
enabled for repeat runs. Release tags use fresh builds without these caches.
Offline mode reuses already downloaded actions and images
while still acquiring missing ones. See the [act usage guide](https://nektosact.com/usage/index.html).

The acceptance limit is 20 minutes total on this machine, including setup and
downloads; timing has not been measured yet. Workflow job timeouts apply on GitHub;
`act` [ignores them](https://nektosact.com/not_supported.html), so stop the local run
at 20 minutes and record the unfinished step if it overruns. Fuzzing, mutation
and reproducibility workflows are separate scheduled or manual runs.

## Releases

Before the first release, set the Git remote and run
`uv run python tools/github_setup.py` to configure branch/tag protection,
secret scanning and private vulnerability reporting. This changes GitHub settings
and requires admin rights; see [SECURITY.md](SECURITY.md) for private reports.

Run `uvx --from commitizen==4.19.2 cz bump`, review the version/changelog, then
`git push --follow-tags`. Tags with or without a `v` prefix must match Cargo.toml.
Distribution uses GitHub Releases; crates.io publishing remains disabled.

Releases require CI on the exact tagged commit and smoke tests of the final
Windows/Linux archives. The GitHub Action verifies the archive checksum and
requested version before execution.
Checksums detect corrupted or changed downloads. They are not independent proof
of who built a release, since the archive and manifest come from the same release.

Nightly checks run coverage (90% lines) and two-minute fuzz campaigns for Python
entry-module parsing. The whole fuzz job, including setup, has a five-minute
limit. Miri is omitted while the library has no unsafe code.
Weekly checks run parser mutation tests (80%), benchmarks, and two clean Linux
builds for reproducibility. These checks do not delay releases; missing results
are reported as missing. Fuzzing does not cover the native backends.

The 15–30 minute release target has not been measured on GitHub.

## License

Licensed under the [GNU General Public License](LICENSE), version 3 or (at your
option) any later version (`GPL-3.0-or-later`).
