# Error codes

<!-- Generated from the source by `UPDATE_DOCS=1 cargo test`. Don't edit by hand. -->

Every finding has a code. A code keeps its meaning forever and is never reused, so scripts, ignore lists and config files can rely on it. The first digit names the group.

Change what a code does with `--ignore CODE` or a [`[severity]`](configuration.md) entry.

| Code | Default | Meaning |
|---|---|---|
| [BOK001](#bok001) | error | empty entry |
| [BOK002](#bok002) | error | directory does not exist |
| [BOK003](#bok003) | error | not a directory |
| [BOK004](#bok004) | error | contains an unexpanded variable |
| [BOK005](#bok005) | error | contains a quote character |
| [BOK006](#bok006) | error | has leading or trailing whitespace |
| [BOK007](#bok007) | warning | duplicate entry |
| [BOK008](#bok008) | warning | saved in PATH but missing from this terminal |
| [BOK009](#bok009) | error | saved PATH cut off by a length limit |
| [BOK101](#bok101) | error | target does not exist |
| [BOK102](#bok102) | error | library not found |
| [BOK103](#bok103) | error | interpreter does not exist |
| [BOK104](#bok104) | error | Python module not installed |
| [BOK105](#bok105) | error | command does not resolve through PATH |
| [BOK106](#bok106) | error | alias for an app that is no longer installed |
| [BOK107](#bok107) | warning | hidden behind an OS copy |
| [BOK201](#bok201) | error | failed to start |
| [BOK202](#bok202) | error | error output when run |
| [BOK203](#bok203) | tool error | timed out |
| [BOK204](#bok204) | tool error | exited with an error |
| [BOK301](#bok301) | error | App Paths target does not exist |

## BOK0xx — PATH entries

### BOK001 — empty entry {#bok001}

**Default:** error; fails the run

Two separators in a row, or one at the start or end of PATH. On Unix an empty entry means the current directory, so any folder you `cd` into can run commands. Remove it.

### BOK002 — directory does not exist {#bok002}

**Default:** error; fails the run

The entry names a folder that isn't there: an uninstalled program, a renamed folder, an unplugged drive or an offline network share. Remove it if the program is gone; otherwise reconnect the drive.

### BOK003 — not a directory {#bok003}

**Default:** error; fails the run

The entry names a file. PATH entries must be folders; point it at the folder that holds the program.

### BOK004 — contains an unexpanded variable {#bok004}

**Default:** error; fails the run

The entry still holds `%VAR%` or `$VAR` text, so the shell searches a folder literally named that. On Windows the variable is undefined, or the saved PATH is `REG_SZ`, which Windows never expands. On Unix the variable was quoted when PATH was set, or `~` was used where the shell doesn't expand it.

### BOK005 — contains a quote character {#bok005}

**Default:** error; fails the run

PATH entries don't need quotes, even for folders with spaces, and many programs read the quotes as part of the folder name. Remove them.

### BOK006 — has leading or trailing whitespace {#bok006}

**Default:** error; fails the run

Programs that don't trim entries read the spaces as part of the folder name and miss the folder. Remove them.

### BOK007 — duplicate entry {#bok007}

**Default:** warning; doesn't fail the run

The same folder appears earlier in PATH, so this copy does nothing. It only lengthens PATH, which has length limits on Windows. Remove the later one.

### BOK008 — saved in PATH but missing from this terminal {#bok008}

**Default:** warning; doesn't fail the run

The folder is in the saved (registry) PATH but not in this process's PATH, usually because the terminal started before PATH changed. Open a new terminal; also restart programs that start terminals, such as VS Code.

### BOK009 — saved PATH cut off by a length limit {#bok009}

**Default:** error; fails the run

The saved PATH ends at a known length limit (setx: 1024 characters; some installers: 1023, 4095 or 8191) or stops mid-name. Every entry after the cut was lost and can't be recovered from the value itself. Restore PATH from a backup, or reinstall the programs whose folders are missing.

## BOK1xx — tool checks

### BOK101 — target does not exist {#bok101}

**Default:** error; fails the run

A symlink, shim, launcher or alias points at a file that isn't there, so typing the command fails. Reinstall the tool with the program that installed it, or remove the leftover.

### BOK102 — library not found {#bok102}

**Default:** error; fails the run

A DLL (Windows) or shared library (Linux) the program loads at start isn't where the system loader looks, so the program won't start. Reinstall the program or the runtime that provides the library.

### BOK103 — interpreter does not exist {#bok103}

**Default:** error; fails the run

A script's shebang or a launcher's built-in path names an interpreter, usually Python, that is gone, often after a Python upgrade or a deleted virtual environment. Reinstall the tool, e.g. `uv tool install --reinstall NAME` or `pipx reinstall NAME`.

### BOK104 — Python module not installed {#bok104}

**Default:** error; fails the run

The launcher's Python exists but lacks the module its entry point imports. Reinstall the tool into that environment.

### BOK105 — command does not resolve through PATH {#bok105}

**Default:** error; fails the run

A script or shim runs another command by name, such as `node`, and no such command is on PATH. Install it or add its folder to PATH.

### BOK106 — alias for an app that is no longer installed {#bok106}

**Default:** error; fails the run

A Windows app execution alias is left over from a packaged (Store or MSIX) app that was uninstalled. Turn it off under Settings > Apps > Advanced app settings > App execution aliases.

### BOK107 — hidden behind an OS copy {#bok107}

**Default:** warning; doesn't fail the run

Typing the name runs the operating system's own copy, which comes earlier in PATH, so this copy never runs. Move its folder ahead of the OS folder in PATH, or call it by full path.

## BOK2xx — the --help run

### BOK201 — failed to start {#bok201}

**Default:** error; fails the run

The operating system refused to start the program: not a valid executable, wrong architecture, or no permission. Reinstall it.

### BOK202 — error output when run {#bok202}

**Default:** error; fails the run

Running it printed an error that means something it needs is missing, or the Windows loader stopped it (missing DLL, bad image). The message is quoted. Reinstall the tool or what it depends on.

### BOK203 — timed out {#bok203}

**Default:** tool error, shown as `[ERROR]`; doesn't fail the run

`--help` didn't finish within the timeout, even when retried on its own. The tool may wait for input or the network. Doesn't fail the run; use `--skip NAME` if it is expected.

### BOK204 — exited with an error {#bok204}

**Default:** tool error, shown as `[ERROR]`; doesn't fail the run

The tool started, but `--help`, `-h` and `--version` all ended in an error of its own. Usually it works and just refuses those flags. Doesn't fail the run.

## BOK3xx — Windows registry

### BOK301 — App Paths target does not exist {#bok301}

**Default:** error; fails the run

A registry App Paths entry, which `Win+R` and `start` use, points at a program that is gone. Delete the key or reinstall the program.
