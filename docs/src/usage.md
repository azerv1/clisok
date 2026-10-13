# Usage

## `clisok check`

Checks PATH entries, then every tool on PATH: static checks first, then a test run for the tools that are safe to run: `--version`, falling back to `-V`, `--help` and `-h` for tools that reject it. Installers, GUI programs, app execution aliases, plain scripts and the OS's own programs are never run.

```
[BROKEN] CLI002 (user) "C:\gone" — directory does not exist
Tools
[BROKEN] ruff  C:\Users\me\.local\bin\ruff.exe
         CLI103 interpreter does not exist: C:\Python311\python.exe
```

- **`--json`** — print the report as JSON on stdout; progress stays on stderr.
- **`-v`, `--verbose`** — also list passing and skipped tools, and shadowed copies.
- **`--skip NAME`** — don't run this tool. Repeatable.
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

Both commands print a `schema` version and the `command` that produced the report. The version goes up only when a field is removed or changes meaning; new fields don't change it. Each finding carries its `code` and its effective `severity`.
