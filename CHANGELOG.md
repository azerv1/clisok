## Unreleased

### Breaking changes

- Remove TOML configuration files and the `--config` and `--no-config` flags. Options are supplied on the command line. Config-only severity overrides, fix allowlists and off-PATH extra locations are removed.
- JSON report schema is now 2: remove `config` from both reports and `extra` from `list`.
- Privileged probes require `check --allow-root`; `check --static` remains available without it.

### Fixed

- Retried tools retain their cumulative probe time in JSON `ms`; final verdicts and attempt details still come from the retry.

- Progress updates overwrite changed rows in place and refresh at most ten times per second, avoiding a full display clear for every tool.

### Added

- Progress bars use the terminal width. Grouping recognizes copied uv/pipx launchers through their embedded interpreter paths and adds Go, Bun, .NET tools, WinGet and Conda layouts.

- Interactive progress shows completed checks and percentages by inferred package manager, plus an overall total.

- `check --static` inspects files without starting tools or interpreter probes.
- Windows repairs target scanned entries, detect concurrent PATH changes and protect command resolution. `--fix` and `--diff` include registry checks automatically.

## 0.3.0 (2026-10-07)

### BREAKING CHANGE

- The old flat options moved under `check`. With the current name and flags, use `clisok check --output-format json`; bare `clisok` prints help instead of running a check. The tool was previously named `pathsok`.

### Feat

- **check**: inspect user copies hidden behind OS copies
- **list**: add list subcommand for PATH commands
- **report**: add schema version and command to JSON
- **discover**: inventory PATH commands and add --system on windows
- **cli**: move check into a check subcommand

### Fix

- **report**: avoid panic when stdout pipe is closed

### Refactor

- **cli**: move the check pipeline out of main into check.rs

## 0.2.0 (2026-10-07)

### Feat

- **cli**: add --system flag and Ctrl+C child cleanup
- **unix**: add Linux support with ELF and shebang checks

### Refactor

- **spawn**: share headless env vars via exec
- **platform**: move windows modules under platform
- **platform**: move OS-specific code into platform
