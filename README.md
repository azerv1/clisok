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

## What it checks

`check` examines PATH entries and the tools they expose, including launchers,
shims, DLLs/shared libraries, interpreters, and Python modules. It statically
inspects everything, then runs `--version` or `--help` only for eligible
headless tools. Installers, GUI programs, plain scripts, app execution aliases,
and OS-owned programs are not run. Use `--static` to disable all child-process
probes, or repeat `--skip NAME` to skip a tool.

On Windows, `--registry` also checks the saved user PATH and App Paths. On all
platforms, `--system` includes OS-owned folders for static inspection. `list`
shows commands and shadowed copies without running them. Options are command-line
only; clisok does not read configuration files.

## Output and exit status

`check` supports `--output-format full` (default), `concise`, `json`, `github`,
`csv`, and `sarif`. Progress is written to stderr. `list` supports `full` and `json`.
For text reports, use `--verbose` to include passing/skipped tools and shadowed copies, or
`--statistics` to summarize findings by code. Exit status is 0 when no problems
remain, 1 when findings remain, and 2 when clisok cannot run the requested check.

```sh
clisok check --output-format json > report.json
clisok check --output-format sarif > report.sarif
clisok check --output-format csv > findings.csv
```

Check JSON uses schema 3: `summary` contains tool counts by verdict, finding count,
and applied-fix count; `findings` is a flat list of PATH, registry, and tool
diagnostics; `tools` keeps every scanned tool with its name, path, kind, and verdict.
Nonzero timings and explanatory notes are included when available. `fixed` lists
applied repairs when present. Use `--verbose` to add available probe attempts and
captured output, launcher details, and shadowed paths. Empty optional fields are
omitted. This replaces schema 2's `path_problems`, `app_paths`, and nested tool
`reasons`; list JSON remains at schema 2.

CSV contains remaining findings only, one per row, with columns
`code,severity,scope,tool,location,origin,message,fix_applicability,fix_message`.
It uses UTF-8 and quotes fields containing commas, quotes, or newlines. Import it
with a comma delimiter in your spreadsheet. A clean check writes just the header.
SARIF includes only reported rules; `--verbose` adds their full explanations.
`--statistics` produces a JSON envelope with `statistics`, or CSV columns
`code,name,count,fixable`; combining statistics with SARIF is rejected.

## Diagnostic codes

Each finding has a stable code. Run `clisok rule list` to see every rule, or
`clisok rule show CLI007` to read its severity, meaning, and repair guidance.
`--ignore CODE` suppresses a finding code.

## Repairs

Plain `check` only reports proposed repairs. `--diff` previews changes without
applying them. `--fix` applies safe repairs; `--unsafe-fixes` allows repairs
that may change command resolution or remove entries that could return. Windows
repairs affect only saved user settings, back up the previous values under
`%LOCALAPPDATA%\clisok\backups\`, and never change the system PATH. There are
no repairs on Unix. Use `--show-fixes` to list applied changes.

## GitHub Action

Use clisok after setup steps to check a runner's PATH. Pass the same CLI flags
through `args`; `--output-format github` creates annotations.

```yaml
- uses: azerv1/clisok@0.3.1
  with:
    args: check --skip some-noisy-tool
```

The Action supports Linux and Windows runners. It downloads a release archive,
verifies its checksum and version, then runs the executable by absolute path.

## Install

Download a binary from [Releases](https://github.com/azerv1/clisok/releases):
Windows x86_64 `.zip`, or a static Linux x86_64 `.tar.gz`.

CI tests Windows Server 2025 and Ubuntu 24.04; other Windows/Linux versions are
not exhaustively tested. macOS and FreeBSD remain on the [roadmap](PLAN.md).
Report bugs and feature requests through GitHub issues.

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
