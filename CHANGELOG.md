## 0.1.1 (2026-10-26)

First public release, for Windows x86_64 and Linux x86_64 (static musl).

- `clisok check` checks PATH entries and every tool on PATH: launchers and shims, DLLs and shared libraries, interpreters and Python entry modules. Each tool that is safe to start then gets a headless `--version` or `--help` test run. Installers, GUI programs, plain scripts, app execution aliases and OS-owned programs are never run, and `--static` runs nothing.
- `--registry` (Windows) also checks the saved user PATH, including cut-off values, and the App Paths registrations. `--system` adds OS-owned folders for static inspection.
- `clisok list` lists every command and the copies it shadows, without running anything.
- Every finding has a stable code: CLI0xx for PATH entries, CLI1xx for tools, CLI2xx for test runs, CLI3xx for the registry. `clisok rule list` and `clisok rule show CODE` explain them, and `--ignore CODE` drops one.
- Output formats: `full`, `concise`, `json`, `csv`, `github` annotations and `sarif`. `--statistics` counts findings per code.
- Windows repairs touch only the saved user PATH and App Paths, and back up the old values first. `--diff` previews, `--fix` applies safe fixes, `--unsafe-fixes` adds fixes that may change what runs, and `--show-fixes` lists what was applied.
- A GitHub Action downloads a release archive, verifies its checksum and version, then runs clisok.
