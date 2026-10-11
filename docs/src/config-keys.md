<!-- Generated from the source by `UPDATE_DOCS=1 cargo test`. Don't edit by hand. -->

| Key | Value | Meaning |
|---|---|---|
| `check.skip` | list of strings | Don't run --help for these tools, like `--skip`. |
| `check.timeout` | integer | Seconds before a --help run is killed, like `--timeout`. |
| `check.jobs` | integer | Tools checked in parallel, like `--jobs`. |
| `check.system` | boolean | Also check the OS's own folders, like `--system`. Also applies to `list`. |
| `check.registry` | boolean | Windows only: also check the saved PATH and App Paths, like `--registry`. |
| `check.extra` | list of strings | Files or folders off PATH to check statically; never run. Globs allowed. Also listed by `list`. |
| `severity.PATHnnn` | "error", "warn" or "off" | Override a code's severity. `off` drops it, like `--ignore`. |
