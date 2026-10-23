# Cross-Platform `clisok` Commands and Safe Repair

This is the broader roadmap. Current releases and CI cover Windows x86_64 and
Linux x86_64; the full compatibility matrix below remains planned. Publication
setup and the current release process are in [README.md](README.md#releases).

## Summary

Implement first-class support for Windows 11, macOS, FreeBSD, Ubuntu, Debian, Fedora, Linux Mint, Arch, and Manjaro. Windows 10 is best-effort.

**Core flow** — CLI parsing → discovery → inspection → ownership detection → repair planning → safe application → complete recheck → reporting.

Linux, macOS, and FreeBSD require separate dependency-resolution logic. The current Unix implementation cannot be reused unchanged because it assumes Linux ELF and `ld.so` behavior.

## Phases

**Release order** — CLI and inventory → macOS and FreeBSD → repair framework + uv → npm and pnpm → cargo. Each phase ships as a usable release, and the CI matrix grows with each one. Windows user PATH repair, planned last as `--fix-paths`, landed early as Ruff-style fixes (see **Fixes**).

1. **CLI and inventory** — `check` and `list` subcommands, inventory models, versioned JSON schema; Linux and Windows.
2. **macOS and FreeBSD** — static inspection on macOS; full checks on FreeBSD.
3. **Repair framework + uv** — `--reinstall-broken` and the uv tool adapter, on top of the fix framework (done).
4. **npm and pnpm** — global packages, including fnm-managed Node layouts.
5. **cargo** — `cargo install` binaries.

**Later, on demand** — Scoop, pipx, bun, Homebrew, and mise adapters. None is installed on the development machine, so none can be tested against real installs yet.

## Public CLI

- **`clisok`** — print help and exit successfully. The old flat flags move under `check`; used bare, they get clap's standard unknown-argument error. clisok was never released, so old invocations get no special handling.

- **`clisok check [existing options]`** — preserve the current PATH, launcher, dependency, and test-run checks (`--version`, then `--help`), including `--output-format`, `--verbose`, `--skip`, `--jobs`, `--timeout`, Windows `--registry`, and `--system` (now on every platform).
  - **Repair preview** (done) — each finding may carry a fix, printed under it; JSON carries the same. Plain `check` is the preview. See **Fixes**.
  - **Hidden behind an OS copy** — when an OS-owned copy is active and hides a user-installed copy, report a tool-level `[WARN]`. The OS copy is never run, the hidden copy gets static inspection only, and the exit code is unaffected. Today the whole command is dropped (`src/discover.rs:58`).

- **Error codes** (done) — every finding has a permanent code: CLI0xx PATH entries, CLI1xx tool checks, CLI2xx the test run, CLI3xx registry. A code is never renumbered or reused. `--ignore CODE` drops a code; the list lives in `src/codes.rs`.

- **Settings** — command-line flags only; TOML configuration support was removed. Expected-tool rules are out of scope: that is pathlint's job.

- **Docs** (done) — an mdBook site in `docs/`, deployed to GitHub Pages. The codes page and the config key table are generated from the source; a test fails when they are stale.

- **Name and scope** (done) — renamed from pathsok, then binok, to clisok, since the tool checks the system's CLI tools, not only PATH. Codes moved from `PATHnnn` to `CLInnn` before any of them was released. PATH-entry checks and `list` stay even though pathlint and patholog also cover them: PATH entries are one input to the executables check.

- **`clisok list [--system] [--output-format full|json]`** — enumerate every active command and all shadowed copies without executing tools. By default, omit commands that exist only in OS-owned directories, but list any command with at least one non-OS copy, including when the OS copy is the active one. `--system` lists everything. OS copies are always marked.

- **`clisok check --fix`** — repair only generated artifacts with unambiguous manager ownership, using the manager's own offline operation where one exists. Remain offline and never modify an unowned executable. Orphan shims, whose package the manager confirms is absent, are skipped with the exact manager command to remove them, e.g. `npm rm -g foo`.

- **`clisok check --fix --reinstall-broken`** — reinstall through the owning manager: uv, npm, pnpm, or cargo. Reproduce what the manager recorded:
  - **Recorded spec** — where the manager records the spec the user typed, reproduce it verbatim. For uv that is the receipt's requirements, Python, and `--with` packages. The version may move within that spec; report old → new. Pinning the installed version instead would rewrite the receipt and stop `uv tool upgrade` from upgrading the tool.
  - **No recorded spec** — npm and cargo record none, so reinstall the installed version from `package.json` or `.crates2.json`; pinning has no side effect there.
  - **pnpm** — confirm which case applies when building its adapter; its global directory keeps a `package.json` that may hold the typed spec.

  Try `--offline` first where the manager supports it. Skip ambiguous or unreconstructable installations. Never invoke OS package managers, winget, or Chocolatey, request elevation, or repair system packages.

- **Fixes** (done, modeled on Ruff) — a fix's applicability is safe (`--fix`, `[*]`, `fix:`), unsafe (`--fix --unsafe-fixes`, `fix (unsafe):`) or advice (`help:`, never applied). `--ignore CODE` suppresses the finding and its fix. Rules live in `src/fix.rs`; `fix::safety` feeds the codes page. Windows only, HKCU only, backups under `%LOCALAPPDATA%\clisok\backups\`:
  - **Safe** — remove empty entries and later copies of a folder (within HKCU or of a machine PATH entry) from `HKCU\Environment\Path`.
  - **Unsafe** — clean quotes and surrounding whitespace off those entries (a cleaned entry can start to work and change resolution); delete HKCU App Paths keys whose target is missing (exported to `.reg` first).
  - **Advice only** — missing directories, non-directories and unexpanded variables (an unplugged drive or a manager bin directory not created yet may come back), and everything outside HKCU.
  - **Not yet** — adding existing bin directories confirmed by manager metadata, appended at the end so no command changes resolution.

  Entry order, value type and `%VARS%` are preserved and the change is broadcast. After fixing, the saved-settings findings are re-checked (tool results can't change: this process's PATH doesn't) and the exit status follows the final state. A failed fix is reported on stderr and its finding stays. On Unix every fix is advice, because the shell configuration that set PATH can't be identified safely.

`--reinstall-broken` requires `--fix`; its reinstalls will be unsafe fixes.

## Core Implementation

**Command path** — command dispatch → shared inventory → check/list behavior → reporting.

- **Command dispatch** — extract the existing checking pipeline and add explicit `check` and `list` subcommands.

- **Inventory models** — represent active paths, shadowed paths, OS ownership, manager ownership, repair plans, and repair outcomes independently of presentation.

- **Manager adapters** — identify ownership from canonical paths and manager metadata, never executable names alone. Conflicting ownership produces a skipped repair. Each adapter implements both tiers: offline repair where the manager has one, and reinstall behind `--reinstall-broken`.

- **Repair policy** — allow generated shim or launcher repair by default. Require `--reinstall-broken` for reinstallation and network access, and `--unsafe-fixes` for any fix that may change what runs. clisok never deletes or moves a file. Prohibit shell-profile editing, administrator access, and system-package changes.

- **PATH invariant (done)** — before a saved user PATH write, compare every command's first match under the old and new persisted PATH (machine + user, expanded). Safe changes that alter resolution abort with `skipped: would change resolution of X`. Unsafe changes list affected names in the applied-fix messages; they cannot bypass the check on safe removals in the same repair. Repairs retain the scanned value and type, target reported indexes, and save a backup before writing. There is no restore command.

- **Reporting** — every finding carries its `code` and effective `severity` (done). Add a JSON schema version, command mode, inventory entries, planned and applied repair actions, final verdicts, and backup location where applicable. Under `--output-format json`, stdout contains JSON only; progress and diagnostics remain on stderr. This already holds today (`src/main.rs:291`, `src/progress.rs`).

**Platform path** — common Unix behavior → OS-specific binary inspection → OS-specific safety rules.

- **Common Unix** — executable permissions, symlinks, and shebangs everywhere; process groups, timeouts, and headless spawning on Linux and FreeBSD only, since macOS never runs tools.

- **Linux** — ELF loading, `ld.so.conf`, Linux system directories, and WSL filtering.

- **macOS** — static checks only; `check` never test-runs a tool. No environment variable cuts a process off from WindowServer, and running a quarantined binary shows a Gatekeeper dialog.
  - **Inspection** — Mach-O and universal-binary parsing through `object`'s `macho` feature; `LC_LOAD_DYLIB` and `LC_RPATH`; `@rpath`, `@loader_path`, and `@executable_path`; application-bundle exclusions; macOS system directories.
  - **Shared cache** — since macOS 11, system dylibs live in the dyld shared cache, not on disk. Resolve them with `_dyld_shared_cache_contains_path`, or every binary looks broken.

- **FreeBSD** — ELF inspection using FreeBSD loader hints and defaults, plus base-system directory rules.

- **Windows** — retain PE, DLL, and App Execution Alias behavior; add user-registry PATH backup and repair.
  - **Value type** — read the Path value's type. A `REG_SZ` value containing `%VAR%` is a new PATH problem, because Windows expands only `REG_EXPAND_SZ`. `persisted_path()` (`src/platform/windows/registry.rs`) currently expands regardless of type. `--fix` reports this but never changes the type.
  - **Truncation** (done) — a saved PATH at a known cut length (setx 1024; NSIS 1023, 4095, 8191; old dialogs 2047) or ending mid-name is reported as cut off, because every entry after the cut is lost silently. The lost entries can't be recovered from the value itself, so it gets advice only; never treat the cut-off stub as an ordinary dead entry to remove.

- **System tools** — never execute OS-owned tools, even when `--system` includes them for static inspection or listing. On Windows the OS-owned directory is `%SystemRoot%`.

## Platform Matrix and CI

- **Windows** — targets every Microsoft-supported Windows 11 x64 feature release. Tested on GitHub-hosted `windows-2022` and `windows-2025` runners, plus a manual smoke run on a real Windows 11 machine with a fixture PATH before each release. Windows 10 22H2 is best-effort and untested.

- **macOS** — current major and previous two majors on Apple Silicon. Intel runs natively only while GitHub's `macos-15-intel` runner exists (retiring fall 2027); fixtures cover x86_64 and universal Mach-O files on Apple Silicon.

- **Ubuntu** — every LTS release in public standard support, excluding paid extended-support-only releases.

- **Debian** — stable and oldstable.

- **Fedora** — current and previous release.

- **Linux Mint** — newest point release from every supported major series, from the official `linuxmintd/mint<version>-amd64` images.

- **FreeBSD** — every production release supported by the FreeBSD Security Officer.

- **Arch and Manjaro** — digest-pinned current stable snapshots of the official `archlinux` and `manjarolinux/base` images, refreshed monthly.

**CI cadence** — pull request gate → nightly compatibility matrix → release artifact gate.

- **Pull requests** — run formatting, linting, unit tests, and representative native jobs on Ubuntu, macOS, Windows, and latest FreeBSD, plus smoke containers for Debian, Fedora, and Arch.

- **Nightly** — run the complete supported-version matrix, Mint and Manjaro images, and real supported-manager integration tests.

- **Releases** — require exact-tag CI and final-artifact/Action smoke tests on the currently implemented Windows/Linux targets. The complete compatibility matrix and automated Windows 11 VM coverage remain roadmap work; background campaigns do not gate the fast release path (see [README.md](README.md#releases)).

- **Maintenance** — update pinned OS images and rolling-distribution snapshots monthly through a reviewed compatibility-manifest change; do not use floating `latest` tags.

GitHub-hosted runners cover Ubuntu, macOS, and Windows Server rather than consumer Windows 11; the manual smoke run covers consumer-only behavior such as App Execution Aliases. See the [GitHub-hosted runner documentation](https://docs.github.com/en/actions/concepts/runners/github-hosted-runners) and [runner image catalogue](https://github.com/actions/runner-images).

FreeBSD runs as a real FreeBSD VM on GitHub's Ubuntu runners, through a FreeBSD VM action, rather than being approximated by a Linux container. See [Rust platform support](https://doc.rust-lang.org/rustc/platform-support.html).

## Distribution

**GitHub Releases only** — no crates.io or package-manager channels for now.

- **Windows** — x86_64 `.zip`.
- **Linux** — static `x86_64-unknown-linux-musl` `.tar.gz`. One binary runs on every Linux matrix member, with no glibc baseline to maintain.
- **macOS** — universal (Apple Silicon + Intel) `.tar.gz`.
- **FreeBSD** — `.tar.gz` built on the oldest supported release.

## Test and Acceptance Plan

- **CLI** — verify parsing, bare-command help, list ordering, shadow detection, OS-only filtering, the hidden-behind-OS-copy warning, repair previews in `check` output, JSON stability, and exit codes.

- **Inspection fixtures** — create hermetic PATHs containing valid tools, dangling symlinks, broken launchers, duplicate entries, spaces, non-UTF names where supported, missing libraries, missing interpreters, timeouts, and spawned child processes.

- **Manager adapters** — test recorded metadata fixtures and real nightly installations. Confirm recorded-spec reconstruction (uv receipts stay unpinned), ownership conflicts, offline default repairs, orphan-shim skips with suggested commands, and skips when reconstruction is incomplete.

- **Safety** — assert that `--fix` never deletes or moves a file, and never changes unowned files, system directories, shell profiles, machine registry values, or OS-managed packages.

- **Windows PATH** — test backup, rollback after simulated write failure, value-type preservation, `REG_SZ`-with-`%VAR%` detection, duplicate cleanup, manager-bin appending, the resolution invariant (a write that would change any command's resolution is aborted), and HKLM non-modification, on disposable hosted runners.

- **Native binaries** — test Linux ELF, macOS Mach-O and universal binaries including shared-cache resolution, FreeBSD ELF and loader hints, Windows PE and DLL resolution, and process-group termination on Linux, FreeBSD, and Windows.

- **Unknown formats** — report dependency inspection as unsupported or skipped rather than treating an unknown binary format as healthy.

- **Artifacts** — install and execute release artifacts on every matrix member, not merely compile them.

## Assumptions

- **BSD scope** — FreeBSD only; NetBSD and OpenBSD are out of scope.

- **Release policy** — test publicly supported releases and exclude paid extended-support variants. Windows 10 22H2 is best-effort: not tested and not release-blocking.

- **Architectures** — require x86_64 everywhere and Apple Silicon additionally on macOS. Windows and Linux ARM are out of scope.

- **Package managers** — adapters for uv, npm, pnpm, and cargo first; Scoop, pipx, bun, Homebrew, and mise later, on demand. winget and Chocolatey are out of scope because their installs often require elevation.
