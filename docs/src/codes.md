# Error codes

<!-- Generated from the source by `UPDATE_DOCS=1 cargo test`. Don't edit by hand. -->

Every finding has a code. A code keeps its meaning forever and is never reused, so scripts, ignore lists and config files can rely on it. The first digit names the group.

Change what a code does with `--ignore CODE` or a [`[severity]`](configuration.md) entry. The Fix column says what [`--fix`](fixes.md) can do about it.

| Code | Default | Fix | Meaning |
|---|---|---|---|
| [CLI001](#cli001) | error | safe | empty entry |
| [CLI002](#cli002) | error | advice | directory does not exist |
| [CLI003](#cli003) | error | advice | not a directory |
| [CLI004](#cli004) | error | advice | contains an unexpanded variable |
| [CLI005](#cli005) | error | unsafe | contains a quote character |
| [CLI006](#cli006) | error | unsafe | has leading or trailing whitespace |
| [CLI007](#cli007) | warning | safe | duplicate entry |
| [CLI008](#cli008) | warning | advice | saved in PATH but missing from this terminal |
| [CLI009](#cli009) | error | advice | saved PATH cut off by a length limit |
| [CLI101](#cli101) | error | advice | target does not exist |
| [CLI102](#cli102) | error | advice | library not found |
| [CLI103](#cli103) | error | advice | interpreter does not exist |
| [CLI104](#cli104) | error | advice | Python module not installed |
| [CLI105](#cli105) | error | advice | command does not resolve through PATH |
| [CLI106](#cli106) | error | advice | alias for an app that is no longer installed |
| [CLI107](#cli107) | warning | — | hidden behind an OS copy |
| [CLI201](#cli201) | error | — | failed to start |
| [CLI202](#cli202) | error | — | error output when run |
| [CLI203](#cli203) | tool error | — | timed out |
| [CLI204](#cli204) | tool error | — | exited with an error |
| [CLI301](#cli301) | error | unsafe | App Paths target does not exist |

## CLI0xx — PATH entries

### CLI001 — empty entry {#cli001}

**Default:** error; fails the run

Two separators in a row, or one at the start or end of PATH. On Unix an empty entry means the current directory, so any folder you `cd` into can run commands. Remove it.

**Fix:** Safe: `--fix` removes it from the saved user PATH (Windows). Elsewhere it is advice.

### CLI002 — directory does not exist {#cli002}

**Default:** error; fails the run

The entry names a folder that isn't there: an uninstalled program, a renamed folder, an unplugged drive or an offline network share. Remove it if the program is gone; otherwise reconnect the drive.

**Fix:** Advice only, shown as a `help:` line.

### CLI003 — not a directory {#cli003}

**Default:** error; fails the run

The entry names a file. PATH entries must be folders; point it at the folder that holds the program.

**Fix:** Advice only, shown as a `help:` line.

### CLI004 — contains an unexpanded variable {#cli004}

**Default:** error; fails the run

The entry still holds `%VAR%` or `$VAR` text, so the shell searches a folder literally named that. On Windows the variable is undefined, or the saved PATH is `REG_SZ`, which Windows never expands. On Unix the variable was quoted when PATH was set, or `~` was used where the shell doesn't expand it.

**Fix:** Advice only, shown as a `help:` line.

### CLI005 — contains a quote character {#cli005}

**Default:** error; fails the run

PATH entries don't need quotes, even for folders with spaces, and many programs read the quotes as part of the folder name. Remove them.

**Fix:** Unsafe: `--fix --unsafe-fixes` cleans it in the saved user PATH (Windows). A cleaned entry that names a real folder starts to work and can change which program runs. Elsewhere it is advice.

### CLI006 — has leading or trailing whitespace {#cli006}

**Default:** error; fails the run

Programs that don't trim entries read the spaces as part of the folder name and miss the folder. Remove them.

**Fix:** Unsafe: `--fix --unsafe-fixes` cleans it in the saved user PATH (Windows). A cleaned entry that names a real folder starts to work and can change which program runs. Elsewhere it is advice.

### CLI007 — duplicate entry {#cli007}

**Default:** warning; doesn't fail the run

The same folder appears earlier in PATH, so this copy does nothing. It only lengthens PATH, which has length limits on Windows. Remove the later one.

**Fix:** Safe: `--fix` removes it from the saved user PATH (Windows). Elsewhere it is advice.

### CLI008 — saved in PATH but missing from this terminal {#cli008}

**Default:** warning; doesn't fail the run

The folder is in the saved (registry) PATH but not in this process's PATH, usually because the terminal started before PATH changed. Open a new terminal; also restart programs that start terminals, such as VS Code.

**Fix:** Advice only, shown as a `help:` line.

### CLI009 — saved PATH cut off by a length limit {#cli009}

**Default:** error; fails the run

The saved PATH ends at a known length limit (setx: 1024 characters; some installers: 1023, 4095 or 8191) or stops mid-name. Every entry after the cut was lost and can't be recovered from the value itself. Restore PATH from a backup, or reinstall the programs whose folders are missing.

**Fix:** Advice only, shown as a `help:` line.

## CLI1xx — tool checks

### CLI101 — target does not exist {#cli101}

**Default:** error; fails the run

A symlink, shim, launcher or alias points at a file that isn't there, so typing the command fails. Reinstall the tool with the program that installed it, or remove the leftover.

**Fix:** Advice only, shown as a `help:` line.

### CLI102 — library not found {#cli102}

**Default:** error; fails the run

A DLL (Windows) or shared library (Linux) the program loads at start isn't where the system loader looks, so the program won't start. Reinstall the program or the runtime that provides the library.

**Fix:** Advice only, shown as a `help:` line.

### CLI103 — interpreter does not exist {#cli103}

**Default:** error; fails the run

A script's shebang or a launcher's built-in path names an interpreter, usually Python, that is gone, often after a Python upgrade or a deleted virtual environment. Reinstall the tool, e.g. `uv tool install --reinstall NAME` or `pipx reinstall NAME`.

**Fix:** Advice only, shown as a `help:` line.

### CLI104 — Python module not installed {#cli104}

**Default:** error; fails the run

The launcher's Python exists but lacks the module its entry point imports. Reinstall the tool into that environment.

**Fix:** Advice only, shown as a `help:` line.

### CLI105 — command does not resolve through PATH {#cli105}

**Default:** error; fails the run

A script or shim runs another command by name, such as `node`, and no such command is on PATH. Install it or add its folder to PATH.

**Fix:** Advice only, shown as a `help:` line.

### CLI106 — alias for an app that is no longer installed {#cli106}

**Default:** error; fails the run

A Windows app execution alias is left over from a packaged (Store or MSIX) app that was uninstalled. Turn it off under Settings > Apps > Advanced app settings > App execution aliases.

**Fix:** Advice only, shown as a `help:` line.

### CLI107 — hidden behind an OS copy {#cli107}

**Default:** warning; doesn't fail the run

Typing the name runs the operating system's own copy, which comes earlier in PATH, so this copy never runs. Move its folder ahead of the OS folder in PATH, or call it by full path.

**Fix:** None.

## CLI2xx — the test run

### CLI201 — failed to start {#cli201}

**Default:** error; fails the run

The operating system refused to start the program: not a valid executable, wrong architecture, or no permission. Reinstall it.

**Fix:** None.

### CLI202 — error output when run {#cli202}

**Default:** error; fails the run

Running it printed an error that means something it needs is missing, or the Windows loader stopped it (missing DLL, bad image). The message is quoted. Reinstall the tool or what it depends on.

**Fix:** None.

### CLI203 — timed out {#cli203}

**Default:** tool error, shown as `[ERROR]`; doesn't fail the run

The test run didn't finish within the timeout, even when retried on its own. The tool may wait for input or the network. Doesn't fail the run; use `--skip NAME` if it is expected.

**Fix:** None.

### CLI204 — exited with an error {#cli204}

**Default:** tool error, shown as `[ERROR]`; doesn't fail the run

The tool started, but `--version`, `-V`, `--help` and `-h` all ended in an error of its own. Usually it works and just refuses those flags. Doesn't fail the run.

**Fix:** None.

## CLI3xx — Windows registry

### CLI301 — App Paths target does not exist {#cli301}

**Default:** error; fails the run

A registry App Paths entry, which `Win+R` and `start` use, points at a program that is gone. Delete the key or reinstall the program.

**Fix:** Unsafe: `--fix --unsafe-fixes` deletes an HKCU key after exporting it to a `.reg` file; the program may only be on an unplugged drive. HKLM keys get advice.
