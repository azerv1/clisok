# Usage

## `clisok check`

Checks PATH entries, then every tool on PATH: static checks first, then a test run for the tools that are safe to run: `--version`, falling back to `-V`, `--help` and `-h` for tools that reject it. Installers, GUI programs, app execution aliases, plain scripts and the OS's own programs are never run.

```
[BROKEN] CLI002 (user) "C:\gone" — directory does not exist
         help: remove the entry if the program is gone; reconnect the drive or share if not (user PATH)
Tools
[BROKEN] ruff  C:\Users\me\.local\bin\ruff.exe
         CLI103 interpreter does not exist: C:\Python311\python.exe
         help: reinstall the tool so it uses an installed interpreter, or remove this launcher
```

- **`--json`** — print the report as JSON on stdout; progress stays on stderr.
- **`-v`, `--verbose`** — also list passing and skipped tools, and shadowed copies.
- **`--skip NAME`** — don't run this tool. Repeatable.
- **`--fix`** — apply the safe fixes, marked `[*]` and shown as `fix:` lines. On Windows that removes empty and repeated entries from the saved user PATH (they need `--registry` to show up). The old value is saved under `%LOCALAPPDATA%\clisok\backups\` first, and the report shows what is left afterwards. Advice (`help:` lines) is never applied.
- **`--unsafe-fixes`** — with `--fix`, also apply the unsafe fixes (`fix (unsafe):` lines): clean quotes and spaces off user PATH entries, which can make a dead entry live and change which program runs, and delete user App Paths keys whose program is missing, which may only be on an unplugged drive. Each key is exported to a `.reg` file in the backups folder first; `reg import` restores it.
- **`--diff`** — print what `--fix` (with `--unsafe-fixes`, if given) would change, as a diff, then exit without changing or running anything. Exit status 1 when there is something to fix.
- **`--show-fixes`** — with `--fix`, list every applied fix in a `Fixed` section instead of only counting them.
- **`--exit-non-zero-on-fix`** — with `--fix`, exit 1 when anything was fixed, even if nothing is left.
- **`--ignore CODE`** — drop findings with this code, e.g. `CLI007`. Repeatable.
- **`--system`** — also check the OS's own folders, statically; nothing there is run.
- **`--registry`** — Windows only: also check the saved PATH and App Paths registrations.
- **`-j`, `--jobs N`** — tools checked in parallel; default one per CPU core.
- **`--timeout SECS`** — seconds before a test run is killed; default 30.
- **`--config FILE`**, **`--no-config`** — read settings from one file, or from none. See [Configuration](configuration.md).

Exit status: 0 when nothing fails, 1 when something does, 2 when clisok itself can't run (empty PATH, bad config).

## `clisok list`

Lists every command on PATH and the copies it shadows. Nothing is run. Commands found only in the OS's own folders are left out unless `--system` is given. OS copies are marked `[OS]`, and programs from the config's `extra` places are marked `[EXTRA]`.

- **`--json`** — print the list as JSON.
- **`--system`** — include commands found only in the OS's own folders.
- **`--config FILE`**, **`--no-config`** — as for `check`.

## JSON

Both commands print a `schema` version and the `command` that produced the report. The version goes up only when a field is removed or changes meaning; new fields don't change it. Each finding carries its `code` and its effective `severity`. A finding may also carry a `fix`: its `applicability` and a `message`. Right now every fix is `display-only` advice, printed as a `help:` line under the finding.
