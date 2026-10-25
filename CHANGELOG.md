## 0.3.1 (2026-10-25)

### BREAKING CHANGE

- --json is gone from check, list and rule; use --output-format json.
- the binary is now clisok, the config file .clisok.toml (user file under clisok/), and finding codes CLInnn instead of BINnnn.
- finding codes are BINnnn instead of BOKnnn, in output, --ignore and [severity] keys.
- the binary is now binok, the config file .binok.toml (user file under binok/), and finding codes BOKnnn instead of PATHnnn.

### Feat

- select version-first or help-first probes
- show check progress by package manager
- add static checks without probe execution
- **release**: sign and verify release artifacts end to end
- **rule**: add list and show subcommands
- **check**: report a missing call in a plain script as CLI108 warning
- **check**: add --output-format full|concise|json|github|sarif
- **check**: add --statistics
- **cli**: add clisok rule CODE
- **fix**: add --diff, --show-fixes and --exit-non-zero-on-fix
- **config**: add [fix] fixable, unfixable and unsafe-fixes
- **fix**: add --unsafe-fixes for user PATH cleanup and user App Paths keys
- **fix**: add --fix to remove empty and repeated user PATH entries on Windows
- **fix**: attach display-only fix advice to findings
- **exec**: probe tools with --version before --help
- **check**: check programs at config extra places
- **config**: add .pathsok.toml config file support
- **check**: add --ignore option to drop finding codes
- **codes**: add stable finding codes with severities
- **windows**: detect missing uv python trampoline targets
- **exec**: treat tools that run but refuse --help as started
- **action**: add composite GitHub Action to run pathsok
- **check**: detect saved PATH truncated by length limit

### Fix

- retain cumulative probe time across retries
- update progress in place without per-tool flashing
- detect copied launchers and fit progress to terminal width
- align release helpers with shipped artifacts
- require a release version for SHA-pinned Actions
- require explicit permission for privileged probes
- remove control characters from terminal report fields
- remove the ambiguous short version probe
- filter credential variables from probe environments
- skip Windows batch paths containing percent
- clean up detached Linux descendants and failed waits
- require a hidden Windows desktop for probes
- restrict Windows child handle inheritance
- handle Windows process containment and wait failures
- harden registry export and backup filenames
- include saved Windows settings for fix and diff
- verify command resolution before PATH repairs
- target scanned PATH entries and reject concurrent changes
- retain actionable saved PATH findings
- preserve env operand handling after option parsing
- retain partial probe output when pipe draining times out
- correct path encoding and interpreter argument edge cases
- expand saved Windows PATH using registry values and types
- preserve empty Unix PATH components in discovery
- anchor relative PATH entries to the invocation directory
- resolve ELF dependencies in loader order
- classify Linux loader version and symbol failures as broken
- report Unix signal crashes as CLI205
- restrict automatically discovered project configuration
- create private random probe work directories
- defer Python module probes until eligible runtime checks
- constrain Unix launcher commands and shebangs
- recognise only constrained Windows forwarding shims
- restrict Python probing to generated entry points
- harden inspection, execution, and repair of untrusted input
- **check**: spell the OS copy in CLI107 notes as it is on disk
- **check**: a run that printed output before timing out started
- **check**: don't test-run a tool whose static checks already failed
- **fix**: don't open the user PATH when nothing is fixable
- **exec**: judge Python tracebacks by their exception line
- **exec**: skip debug and info log lines in the broken-output scan
- **exec**: treat a flag repeated after a dot or slash as an echo
- **shebang**: only treat env setup plus exec as launcher
- **pe**: search System32\drivers when resolving imports

### Refactor

- remove TOML configuration support
- inject saved PATH finding inputs
- extract pure parsing into a library crate
- **cli**: rename binok to clisok and codes to CLInnn
- **codes**: rename BOKnnn codes to BINnnn
- **cli**: rename pathsok to binok and codes to BOKnnn

### Perf

- cache matchers for the three probe flags

## 0.3.0 (2026-10-07)

### BREAKING CHANGE

- The old flat options moved under `check`. With the current name and flags, use `clisok check --output-format json`; bare `clisok` prints help instead of running a check. The tool was previously named `pathsok`.

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
