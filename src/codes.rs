//! Stable codes for every finding, like `PATH002`. A code keeps its meaning
//! forever and is never reused, so scripts and ignore lists can rely on it.
//!
//! Ranges: PATH0xx PATH entries, PATH1xx static tool checks, PATH2xx the
//! `--help` run, PATH3xx registry.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// What a finding does to the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Fails the run.
    Error,
    /// Reported; doesn't fail the run.
    Warn,
    /// Dropped from the report.
    Off,
}

/// The text fields are for the docs.
#[allow(dead_code)]
pub struct CodeInfo {
    pub code: Code,
    pub number: u16,
    pub default: Severity,
    /// Shown as a tool error (`[ERROR]`) unless its severity is overridden.
    pub tool_error: bool,
    pub title: &'static str,
    pub help: &'static str,
}

macro_rules! codes {
    ($($var:ident = $num:literal, $sev:ident $(+ $te:ident)?, $title:literal, $help:literal;)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Code { $($var),* }

        /// Every code, in the order of `Code`'s variants.
        pub const CODES: &[CodeInfo] = &[$(CodeInfo {
            code: Code::$var,
            number: $num,
            default: Severity::$sev,
            tool_error: codes!(@te $($te)?),
            title: $title,
            help: $help,
        }),*];
    };
    (@te) => { false };
    (@te tool_error) => { true };
}

codes! {
    Path001 = 1, Error, "empty entry",
        "Two separators in a row, or one at the start or end of PATH. On Unix an empty entry means the current directory, so any folder you `cd` into can run commands. Remove it.";
    Path002 = 2, Error, "directory does not exist",
        "The entry names a folder that isn't there: an uninstalled program, a renamed folder, an unplugged drive or an offline network share. Remove it if the program is gone; otherwise reconnect the drive.";
    Path003 = 3, Error, "not a directory",
        "The entry names a file. PATH entries must be folders; point it at the folder that holds the program.";
    Path004 = 4, Error, "contains an unexpanded variable",
        "The entry still holds `%VAR%` or `$VAR` text, so the shell searches a folder literally named that. On Windows the variable is undefined, or the saved PATH is `REG_SZ`, which Windows never expands. On Unix the variable was quoted when PATH was set, or `~` was used where the shell doesn't expand it.";
    Path005 = 5, Error, "contains a quote character",
        "PATH entries don't need quotes, even for folders with spaces, and many programs read the quotes as part of the folder name. Remove them.";
    Path006 = 6, Error, "has leading or trailing whitespace",
        "Programs that don't trim entries read the spaces as part of the folder name and miss the folder. Remove them.";
    Path007 = 7, Warn, "duplicate entry",
        "The same folder appears earlier in PATH, so this copy does nothing. It only lengthens PATH, which has length limits on Windows. Remove the later one.";
    Path008 = 8, Warn, "saved in PATH but missing from this terminal",
        "The folder is in the saved (registry) PATH but not in this process's PATH, usually because the terminal started before PATH changed. Open a new terminal; also restart programs that start terminals, such as VS Code.";
    Path009 = 9, Error, "saved PATH cut off by a length limit",
        "The saved PATH ends at a known length limit (setx: 1024 characters; some installers: 1023, 4095 or 8191) or stops mid-name. Every entry after the cut was lost and can't be recovered from the value itself. Restore PATH from a backup, or reinstall the programs whose folders are missing.";
    Path101 = 101, Error, "target does not exist",
        "A symlink, shim, launcher or alias points at a file that isn't there, so typing the command fails. Reinstall the tool with the program that installed it, or remove the leftover.";
    Path102 = 102, Error, "library not found",
        "A DLL (Windows) or shared library (Linux) the program loads at start isn't where the system loader looks, so the program won't start. Reinstall the program or the runtime that provides the library.";
    Path103 = 103, Error, "interpreter does not exist",
        "A script's shebang or a launcher's built-in path names an interpreter, usually Python, that is gone, often after a Python upgrade or a deleted virtual environment. Reinstall the tool, e.g. `uv tool install --reinstall NAME` or `pipx reinstall NAME`.";
    Path104 = 104, Error, "Python module not installed",
        "The launcher's Python exists but lacks the module its entry point imports. Reinstall the tool into that environment.";
    Path105 = 105, Error, "command does not resolve through PATH",
        "A script or shim runs another command by name, such as `node`, and no such command is on PATH. Install it or add its folder to PATH.";
    Path106 = 106, Error, "alias for an app that is no longer installed",
        "A Windows app execution alias is left over from a packaged (Store or MSIX) app that was uninstalled. Turn it off under Settings > Apps > Advanced app settings > App execution aliases.";
    Path107 = 107, Warn, "hidden behind an OS copy",
        "Typing the name runs the operating system's own copy, which comes earlier in PATH, so this copy never runs. Move its folder ahead of the OS folder in PATH, or call it by full path.";
    Path201 = 201, Error, "failed to start",
        "The operating system refused to start the program: not a valid executable, wrong architecture, or no permission. Reinstall it.";
    Path202 = 202, Error, "error output when run",
        "Running it printed an error that means something it needs is missing, or the Windows loader stopped it (missing DLL, bad image). The message is quoted. Reinstall the tool or what it depends on.";
    Path203 = 203, Warn + tool_error, "timed out",
        "`--help` didn't finish within the timeout, even when retried on its own. The tool may wait for input or the network. Doesn't fail the run; use `--skip NAME` if it is expected.";
    Path204 = 204, Warn + tool_error, "exited with an error",
        "The tool started, but `--help`, `-h` and `--version` all ended in an error of its own. Usually it works and just refuses those flags. Doesn't fail the run.";
    Path301 = 301, Error, "App Paths target does not exist",
        "A registry App Paths entry, which `Win+R` and `start` use, points at a program that is gone. Delete the key or reinstall the program.";
}

impl Code {
    pub fn info(self) -> &'static CodeInfo {
        &CODES[self as usize]
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PATH{:03}", self.info().number)
    }
}

impl FromStr for Code {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let unknown = || format!("unknown code '{s}' (codes look like PATH002)");
        let digits = s.get(..4).filter(|p| p.eq_ignore_ascii_case("PATH")).map(|_| &s[4..]).ok_or_else(unknown)?;
        if digits.len() != 3 {
            return Err(unknown());
        }
        let n: u16 = digits.parse().map_err(|_| unknown())?;
        CODES.iter().find(|c| c.number == n).map(|c| c.code).ok_or_else(unknown)
    }
}

impl Serialize for Code {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Code {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Severity overrides by code, from `--ignore` and the config file.
#[derive(Debug, Clone, Default)]
pub struct Severities(pub HashMap<Code, Severity>);

impl Severities {
    pub fn get(&self, code: Code) -> Option<Severity> {
        self.0.get(&code).copied()
    }

    /// The override, else the code's default.
    pub fn of(&self, code: Code) -> Severity {
        self.get(code).unwrap_or(code.info().default)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn table_matches_variants_and_numbers_are_unique() {
        let mut seen = HashSet::new();
        for (i, c) in CODES.iter().enumerate() {
            assert_eq!(c.code as usize, i, "{} out of order", c.code);
            assert!(seen.insert(c.number), "{} used twice", c.code);
        }
    }

    #[test]
    fn parses_and_prints() {
        for c in CODES {
            assert_eq!(c.code.to_string().parse::<Code>(), Ok(c.code));
        }
        assert_eq!("path007".parse::<Code>(), Ok(Code::Path007));
        assert_eq!(Code::Path301.to_string(), "PATH301");
        for bad in ["PATH7", "PATH0007", "PATH999", "E501", "PATH", ""] {
            assert!(bad.parse::<Code>().is_err(), "{bad}");
        }
    }
}
