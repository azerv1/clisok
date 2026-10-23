<!-- Generated from the source by `UPDATE_DOCS=1 cargo test`. Don't edit by hand. -->

| Key | Value | Meaning |
|---|---|---|
| `check.skip` | list of strings | Don't run these tools, like `--skip`. Project entries add to the user list. |
| `check.timeout` | integer | Seconds before a test run is killed, like `--timeout`. |
| `check.jobs` | integer | Tools checked in parallel, like `--jobs`. |
| `check.system` | boolean | Also check the OS's own folders, like `--system`. Also applies to `list`. |
| `check.output-format` | `full`, `concise`, `json`, `github` or `sarif` | How `check` prints the report, like `--output-format`. |
| `check.registry` | boolean | Windows only: also check the saved PATH and App Paths, like `--registry`. User file or `--config` only. |
| `check.extra` | list of strings | Files or folders off PATH to check statically; never run. Globs allowed. Also listed by `list`. User file or `--config` only. |
| `severity.CLInnn` | "error", "warn" or "off" | Override a code's severity. `off` drops it, like `--ignore`. |
| `fix.fixable` | list of codes | Only these codes' fixes may be applied by `--fix`; default all. Project and user lists intersect; an empty list allows none. |
| `fix.unfixable` | list of codes | These codes' fixes are never applied; they stay advice. Project entries add to the user list. |
| `fix.unsafe-fixes` | boolean | With `--fix`, also apply unsafe fixes, like `--unsafe-fixes`. User file or `--config` only. |
