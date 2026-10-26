# clisok

Are the CLIs OK? check.

Check that all the CLIs on your system work: PATH entries, launchers and
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
and OS-owned programs are not run.

On Windows, `--registry` also checks the saved user PATH and App Paths. On all
platforms, `--system` includes OS-owned folders for static inspection. `list`
shows commands and shadowed copies without running them. Options are command-line
only; clisok does not read configuration files.

## Output and exit status

`check` supports `--output-format full` (default), `concise`, `json`, `github`,
`csv`, and `sarif`.

```sh
clisok check --output-format json > report.json
clisok check --output-format csv > findings.csv
```

## Diagnostic codes

Each finding has a stable code. Run `clisok rule list` to see every rule, or
`clisok rule show CLI007` to read its severity, meaning, and repair guidance.
`--ignore CODE` suppresses a finding code.

## Repairs

Plain `check` only reports proposed repairs. `--diff` previews changes without
applying them. `--fix` applies safe repairs; `--unsafe-fixes` allows UNSAFE repairs
that may change command resolution or remove entries that could return. Windows
repairs affect only saved user settings, back up the previous values under
`%LOCALAPPDATA%\clisok\backups\`, and never change the system PATH. There are
no repairs on Unix. Use `--show-fixes` to list applied changes.

## GitHub Action

Use clisok after setup steps to check a runner's PATH. Pass the same CLI flags
through `args`; `--output-format github` creates annotations.

```yaml
- uses: azerv1/clisok@0.1.1
  with:
    args: check --skip some-noisy-tool
```

## Install

Download a binary from [Releases](https://github.com/azerv1/clisok/releases):
Windows x86_64 `.zip`, or a static Linux x86_64 `.tar.gz`.

Or build from a local clone with [rustup](https://rustup.rs/), which fetches the
pinned Rust 1.98.1:

```sh
git clone https://github.com/azerv1/clisok.git
cd clisok
cargo install --path . --locked
```

This puts `clisok` in `~/.cargo/bin`.

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

## License

Licensed under the [GNU General Public License](LICENSE), version 3 or (at your
option) any later version (`GPL-3.0-or-later`).
