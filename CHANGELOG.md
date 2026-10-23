## Unreleased

### Breaking changes

- Remove TOML configuration files and the `--config` and `--no-config` flags. Options are supplied on the command line. Config-only severity overrides, fix allowlists and off-PATH extra locations are removed.
- JSON report schema is now 2: remove `config` from both reports and `extra` from `list`.

## 0.3.0 (2026-10-07)

### BREAKING CHANGE

- the old flat options now live under `pathsok check` (e.g. `pathsok check --json`); bare `pathsok` prints help instead of running a check.

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
