# Usage

## `clisok check`

Checks PATH entries, then every tool on PATH: static checks first, then a test run for the tools that are safe to run: `--version`, falling back to `-V`, `--help` and `-h` for tools that reject it. Installers, GUI programs, app execution aliases, plain scripts and the OS's own programs are never run.

Python scripts are run only when they match a generated pip/distlib or uv console
entry-point template. Other Python bodies are inspected without being run,
including unrecognised bodies embedded in Windows Python launchers.

Windows batch wrappers are probed only for a recognised npm launcher template or
a single `.exe`/`.com` call forwarding `%*` without fixed arguments. Redirections,
extra commands and interpreter arguments keep a batch file in static checks.

Unix shell launchers must end by forwarding `"$@"`. Redirections and fixed
arguments to shell or scripting interpreters keep them in static checks. A file
under `node_modules` is treated as a Node entry point only with a Node shebang.

Static Python checks inspect files without starting the interpreter. If a module
cannot be found on disk, clisok probes the interpreter only when the launcher is
eligible to run, using `--timeout`. That probe is included in JSON `attempts`,
including for passing tools. Skipped tools note that the module was not probed.

Probes share a randomly named temporary working directory created for this run.
On Unix it is accessible only to its owner (mode `0700`). A directory creation
failure prevents the probe from starting. Cleanup runs after checks and on Ctrl+C.

```
[BROKEN] CLI002 (user) "C:\gone" — directory does not exist
         help: remove the entry if the program is gone; reconnect the drive or share if not (user PATH)
Tools
[BROKEN] ruff  C:\Users\me\.local\bin\ruff.exe
         CLI103 interpreter does not exist: C:\Python311\python.exe
         help: reinstall the tool so it uses an installed interpreter, or remove this launcher
```

- **`--output-format FORMAT`** — how to print the report; progress always goes to stderr:
  - `full` (default) — grouped by PATH entries, App Paths and tools, with fixes and a summary.
  - `concise` — one line per finding: `<tool path or PATH entry>: CODE message`, `[*]` when `--fix` fixes it.
  - `json` — the whole report; see [JSON](#json).
  - `github` — [GitHub Actions annotations](github-action.md), `::error` or `::warning` per finding.
  - `sarif` — SARIF 2.1.0 for code-scanning tools: every code is a rule, and a finding's fix is under `properties.fix`.
- **`--statistics`** — print how many findings each code has, most first, instead of the findings: `19  CLI107  [ ] hidden behind an OS copy`. `[*]` means `--fix` fixes all of them, `[-]` some. With `--output-format json`, a list of `code`, `name`, `count` and `fixable`. The exit status is the same as without it.
- **`-v`, `--verbose`** — also list passing and skipped tools, and shadowed copies.
- **`--skip NAME`** — don't run this tool. Repeatable.
- **`--fix`** — apply the safe fixes, marked `[*]` and shown as `fix:` lines. On Windows this implies `--registry` and removes empty and repeated entries from the saved user PATH. The old value is saved under `%LOCALAPPDATA%\clisok\backups\` first, and the report shows what is left afterwards. Advice (`help:` lines) is never applied.
- **`--unsafe-fixes`** — with `--fix`, also apply the unsafe fixes (`fix (unsafe):` lines): clean quotes and spaces off user PATH entries, which can make a dead entry live and change which program runs, and delete user App Paths keys whose program is missing, which may only be on an unplugged drive. Each key is exported to a `.reg` file in the backups folder first; `reg import` restores it.
- **`--diff`** — print what `--fix` (with `--unsafe-fixes`, if given) would change, as a diff, then exit without changing or running anything. On Windows this implies `--registry`. Exit status 1 when there is something to fix.
- **`--show-fixes`** — with `--fix`, list every applied fix in a `Fixed` section instead of only counting them.
- **`--exit-non-zero-on-fix`** — with `--fix`, exit 1 when anything was fixed, even if nothing is left.
- **`--ignore CODE`** — drop findings with this code, e.g. `CLI007`. Repeatable.
- **`--system`** — also check the OS's own folders, statically; nothing there is run.
- **`--registry`** — Windows only: also check the saved PATH and App Paths registrations.
- **`-j`, `--jobs N`** — tools checked in parallel; default one per CPU core.
- **`--timeout SECS`** — seconds before a test run is killed; default 30, minimum 1.

Exit status: 0 when nothing fails, 1 when something does, 2 when clisok itself can't run (for example, an empty PATH).

Options are supplied on the command line. clisok does not read user or project
configuration files. Use `--skip` to prevent a tool from running and `--ignore`
to suppress a code's findings.

On Unix, a probe terminated by a signal fails with `CLI205`. JSON attempts include
`signal` and retain the shell-compatible `exit` value (`128 + signal`). A probe
killed by its timeout remains a timeout finding.
If a descendant keeps the output pipe open, clisok retains the prefix already
captured and marks the attempt's `output_truncated` field as true.

Relative PATH entries are reported as `CLI010` warnings. On Unix they are resolved
against the directory where you invoked clisok, including in the probe's PATH.
Empty Unix PATH components, including a trailing `:`, search that invocation
directory and are reported as `CLI001`. `list` includes the commands they expose.

Windows saved PATH checks expand only `REG_EXPAND_SZ` values, using saved machine
environment variables followed by user overrides and the process's Windows/profile
variables. A `REG_SZ` value keeps `%VAR%` literally and reports `CLI004`. Repairs
use the same expansion and preserve the original value type.
Repairs edit only the reported entries and stop if the saved user PATH value or
its registry type changed since the scan. Variables that expand into several
directories are left for manual editing; a repair cannot remove just one of them.
Before a PATH repair, clisok compares command resolution under the expanded
machine and user PATH values before and after the edit. Safe repairs that change
a command's first match are skipped. Unsafe repairs list the changed command
names in JSON `fixed` messages and in the `--show-fixes` output.

## `clisok list`

Lists every command on PATH and the copies it shadows. Nothing is run. Commands found only in the OS's own folders are left out unless `--system` is given. OS copies are marked `[OS]`.

- **`--output-format full|json`** — print the list as text (default) or JSON.
- **`--system`** — include commands found only in the OS's own folders.

## `clisok rule`

### `clisok rule list`

Lists every built-in rule in code order, with columns for Code, Default, Fix and Title. The catalog is the same on every platform. Fix availability is `safe`, `unsafe`, `advice` or `—` when there is no fix.

- **`--output-format text|json`** — print the table (default) or the same JSON array as `clisok rule show --all --output-format json`, including full explanations.

### `clisok rule show CODE`

Explains a code: what it means, its default severity, what `--fix` can do about it, and how to resolve it. The same text is on the [codes page](codes.md).

- **`--all`** — explain every code instead of specifying one code.
- **`--output-format text|json`** — print as text (default) or JSON: `code`, `title`, `default`, `fix` (`safe`, `unsafe`, `display-only` or `null`), `fix_safety` and `explanation`. One code produces an object; `--all` produces an array.

`clisok rule CODE` and `clisok rule --all` remain available as shortcuts for `show`.

## JSON

Both commands print a `schema` version and the `command` that produced the report. The current version is 2: the config-file list was removed from both reports, and the off-PATH `extra` list was removed from `list`. The version goes up only when a field is removed or changes meaning; new fields don't change it. Each finding carries its `code` and its effective `severity`. A finding may also carry a `fix`: its `applicability` and a `message`. Applicability is `safe`, `unsafe` or `display-only`; display-only fixes are advice, printed as a `help:` line under the finding.
