# Configuration

binok reads settings from TOML files. Command-line flags beat every file.

```toml
[check]
skip    = ["${NOISY_TOOL:-gradle}"]
timeout = 30
extra   = [
  "${HOME}/tools/*/bin",
  '${LOCALAPPDATA}\Programs\foo\foo.exe',
]

[severity]
BIN007 = "error"   # duplicates fail the run
BIN008 = "off"     # don't report stale terminals
```

## Where binok looks

1. **User file** — `%APPDATA%\binok\config.toml` on Windows, `$XDG_CONFIG_HOME/binok/config.toml` elsewhere (default `~/.config/binok/config.toml`).
2. **Project file** — `.binok.toml` in the current folder or the nearest folder above it. The search stops at a folder holding `.git`, or at your home folder.

The project file is read after the user file, and its settings replace the user file's. `[severity]` entries merge per code. `--config FILE` reads only that file; `--no-config` reads none. Both reports list the files read under `config` in JSON.

## Variables

Strings in `skip` and `extra` can use environment variables, with the same syntax on every OS:

- **`${VAR}`** — the variable's value. An unset or empty variable is an error naming the file and line.
- **`${VAR:-default}`** — the value, or `default` when the variable is unset or empty.
- **`$$`** — a literal `$`. Any other `$` is an error.

Windows paths have backslashes, which TOML's double-quoted strings treat as escapes. Write them in single quotes, `'C:\tools'`, or use forward slashes.

## Extra places

`extra` lists files and folders off PATH for binok to check too. Globs work: `*`, `?`, `[abc]` and `**`. A folder contributes the programs in it. Extra programs get the same static checks as programs on PATH, but they are never run, and they're reported in their own `Extra` section. A place that matches nothing is noted on stderr.

## Keys

{{#include config-keys.md}}
