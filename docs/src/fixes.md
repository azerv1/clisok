# Fixes

Fixes work the way Ruff's do. A finding can carry a fix, and the fix's applicability decides whether clisok may apply it:

| Applicability | Shown as | Applied by |
|---|---|---|
| safe | `[*]` before the finding, then a `fix:` line | `--fix` |
| unsafe | a `fix (unsafe):` line | `--fix --unsafe-fixes` |
| advice | a `help:` line | never |

A safe fix changes nothing that works today. An unsafe fix may change which program runs, or remove something that might come back. The [codes page](codes.md) gives each code's fix.

```
PATH entries
[WARN]   CLI007 [*] (user) "C:\tools" — duplicate entry
         fix: remove it from the user PATH
[BROKEN] CLI006 (user) " C:\go\bin" — has leading/trailing whitespace
         fix (unsafe): trim the spaces in the user PATH

Summary: ...
[*] 1 fixable with --fix (1 more with --unsafe-fixes).
```

## What `--fix` changes

Only saved settings that belong to you, and only on Windows:

- **Saved user PATH** (`HKCU\Environment\Path`). Empty entries and later copies of a folder are removed (safe). Quotes and surrounding spaces are cleaned off (unsafe: an entry that starts to work comes before later entries). Every other entry, the `%VARIABLES%` and the value type (`REG_EXPAND_SZ`) are kept exactly as they were. Afterwards clisok tells running programs that the environment changed. Already open terminals keep their old PATH.
- **App Paths keys under HKCU** whose program is missing are deleted (unsafe: the program may be on an unplugged drive).

Saved-PATH and App Paths findings only appear with `--registry`. clisok never touches the system PATH or HKLM, never asks for admin rights, and never fixes anything on Unix. There a shell profile sets PATH, and it can't be edited safely.

Only findings in the report are fixed, so a code turned off with `--ignore` or `[severity]` is never fixed. After fixing, the report shows a `Fixed` section and the findings that are left, and the exit status follows what is left. A fix that fails is reported on stderr, and its finding stays in the report.

## Backups

Before each change, clisok saves the old value in `%LOCALAPPDATA%\clisok\backups\`:

- `user-Path-<time>.txt` holds the old user PATH. To restore it, paste it back in *Edit environment variables for your account*.
- `app-path-<name>-<time>.reg` holds a deleted App Paths key. To restore it, run `reg import <file>`.

## Choosing which fixes apply

In [`.clisok.toml`](configuration.md):

```toml
[fix]
fixable = ["CLI001", "CLI007"]   # only these codes' fixes are applied
unfixable = ["CLI301"]           # these codes' fixes are never applied
unsafe-fixes = true              # --fix also applies unsafe fixes
```

A fix that is left out still shows, as a `help:` line.
