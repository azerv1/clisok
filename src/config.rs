//! `.binok.toml`: defaults for `check` and `list` flags, severity per code,
//! and extra places to check. A user-wide file is read first, then the
//! project's; command-line flags beat both.
//!
//! String values in `skip` and `extra` may use `${VAR}` and `${VAR:-default}`
//! on every OS; `$$` is a literal `$`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml::Spanned;

use crate::codes::{Code, Severity};

/// The project file's name.
pub const FILE_NAME: &str = ".binok.toml";

/// Every setting, for the docs generator (a test): key, value type, meaning.
#[cfg_attr(not(test), allow(dead_code))]
pub const KEYS: &[(&str, &str, &str)] = &[
    ("check.skip", "list of strings", "Don't run --help for these tools, like `--skip`."),
    ("check.timeout", "integer", "Seconds before a --help run is killed, like `--timeout`."),
    ("check.jobs", "integer", "Tools checked in parallel, like `--jobs`."),
    ("check.system", "boolean", "Also check the OS's own folders, like `--system`. Also applies to `list`."),
    ("check.registry", "boolean", "Windows only: also check the saved PATH and App Paths, like `--registry`."),
    (
        "check.extra",
        "list of strings",
        "Files or folders off PATH to check statically; never run. Globs allowed. Also listed by `list`.",
    ),
    ("severity.PATHnnn", "\"error\", \"warn\" or \"off\"", "Override a code's severity. `off` drops it, like `--ignore`."),
];

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    check: CheckFile,
    #[serde(default)]
    severity: HashMap<Code, Severity>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckFile {
    skip: Option<Vec<Spanned<String>>>,
    timeout: Option<u64>,
    jobs: Option<usize>,
    system: Option<bool>,
    registry: Option<bool>,
    extra: Option<Vec<Spanned<String>>>,
}

/// Settings from every config file read, merged, with variables expanded.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Config {
    pub skip: Option<Vec<String>>,
    pub timeout: Option<u64>,
    pub jobs: Option<usize>,
    pub system: Option<bool>,
    #[cfg_attr(not(windows), allow(dead_code))]
    pub registry: Option<bool>,
    pub extra: Option<Vec<String>>,
    pub severity: HashMap<Code, Severity>,
    /// The files read, in order.
    pub files: Vec<PathBuf>,
}

/// `--config` and `--no-config`, shared by `check` and `list`.
#[derive(clap::Args, Debug, Default)]
pub struct ConfigArgs {
    /// Read settings from this file instead of the user and project config files
    #[arg(long, value_name = "FILE", conflicts_with = "no_config")]
    config: Option<PathBuf>,
    /// Ignore config files
    #[arg(long)]
    no_config: bool,
}

impl ConfigArgs {
    pub fn load(&self) -> Result<Config, String> {
        if self.no_config {
            return Ok(Config::default());
        }
        let files = match &self.config {
            Some(f) => vec![f.clone()],
            None => {
                let cwd = std::env::current_dir().unwrap_or_default();
                discover(&cwd, user_file(), home().as_deref())
            }
        };
        let mut config = Config::default();
        for f in files {
            let src = fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
            config.merge(parse(&src, &f, &env_var)?, f);
        }
        Ok(config)
    }
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn home() -> Option<PathBuf> {
    env_var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

/// `%APPDATA%\binok\config.toml`, or `$XDG_CONFIG_HOME/binok/config.toml`
/// (default `~/.config`).
fn user_file() -> Option<PathBuf> {
    let dir = if cfg!(windows) {
        PathBuf::from(env_var("APPDATA")?)
    } else {
        env_var("XDG_CONFIG_HOME").filter(|d| !d.is_empty()).map(PathBuf::from).or_else(|| Some(home()?.join(".config")))?
    };
    Some(dir.join("binok").join("config.toml"))
}

/// The files to read, in order: the user file, then the nearest
/// `.binok.toml` from `start` upward, stopping at a `.git` folder or home.
fn discover(start: &Path, user: Option<PathBuf>, home: Option<&Path>) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = user.into_iter().filter(|f| f.is_file()).collect();
    for dir in start.ancestors() {
        let candidate = dir.join(FILE_NAME);
        if candidate.is_file() {
            if !files.contains(&candidate) {
                files.push(candidate);
            }
            break;
        }
        if dir.join(".git").exists() || Some(dir) == home {
            break;
        }
    }
    files
}

impl Config {
    /// Lay a later file over this one: its settings win, severities merge per code.
    fn merge(&mut self, other: Config, file: PathBuf) {
        self.skip = other.skip.or(self.skip.take());
        self.timeout = other.timeout.or(self.timeout);
        self.jobs = other.jobs.or(self.jobs);
        self.system = other.system.or(self.system);
        self.registry = other.registry.or(self.registry);
        self.extra = other.extra.or(self.extra.take());
        self.severity.extend(other.severity);
        self.files.push(file);
    }
}

/// Parse one file and expand its variables. Errors name the file and line.
fn parse(src: &str, path: &Path, lookup: &dyn Fn(&str) -> Option<String>) -> Result<Config, String> {
    let file: File = toml::from_str(src).map_err(|e| format!("{}: {e}", path.display()))?;
    let expand_all = |list: Option<Vec<Spanned<String>>>| -> Result<Option<Vec<String>>, String> {
        list.map(|items| {
            items
                .into_iter()
                .map(|s| {
                    let line = src[..s.span().start].matches('\n').count() + 1;
                    expand(s.get_ref(), lookup).map_err(|e| format!("{}:{line}: {e}", path.display()))
                })
                .collect()
        })
        .transpose()
    };
    let c = file.check;
    Ok(Config {
        skip: expand_all(c.skip)?,
        timeout: c.timeout,
        jobs: c.jobs,
        system: c.system,
        registry: c.registry,
        extra: expand_all(c.extra)?,
        severity: file.severity,
        files: Vec::new(),
    })
}

/// Expand `${VAR}`, `${VAR:-default}` and `$$`. A variable that is unset (or
/// empty) and has no default is an error, as is any other use of `$`.
pub fn expand(s: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        rest = &rest[i + 1..];
        if let Some(r) = rest.strip_prefix('$') {
            out.push('$');
            rest = r;
            continue;
        }
        let Some(body) = rest.strip_prefix('{') else {
            return Err(format!("'$' must start ${{VAR}}, or be written $$: {s}"));
        };
        let Some(end) = body.find('}') else {
            return Err(format!("unclosed ${{: {s}"));
        };
        let (name, default) = match body[..end].split_once(":-") {
            Some((n, d)) => (n, Some(d)),
            None => (&body[..end], None),
        };
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!("bad variable name '{name}': {s}"));
        }
        match (lookup(name).filter(|v| !v.is_empty()), default) {
            (Some(v), _) => out.push_str(&v),
            (None, Some(d)) => out.push_str(d),
            (None, None) => return Err(format!("variable {name} is not set: {s}")),
        }
        rest = &body[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(name: &str) -> Option<String> {
        match name {
            "HOME" => Some("/home/me".into()),
            "EMPTY" => Some(String::new()),
            _ => None,
        }
    }

    #[test]
    fn expands_variables() {
        assert_eq!(expand("${HOME}/bin", &vars).unwrap(), "/home/me/bin");
        assert_eq!(expand("${NOPE:-gradle}", &vars).unwrap(), "gradle");
        assert_eq!(expand("${EMPTY:-x}", &vars).unwrap(), "x");
        assert_eq!(expand("cost $$5 ${HOME}", &vars).unwrap(), "cost $5 /home/me");
        assert_eq!(expand("plain", &vars).unwrap(), "plain");
    }

    #[test]
    fn rejects_unset_and_malformed_variables() {
        assert!(expand("${NOPE}", &vars).unwrap_err().contains("NOPE is not set"));
        assert!(expand("$HOME/bin", &vars).is_err());
        assert!(expand("${HOME", &vars).is_err());
        assert!(expand("${}", &vars).is_err());
        assert!(expand("${A B}", &vars).is_err());
    }

    #[test]
    fn parses_a_file() {
        let src = "[check]\nskip = [\"${NOPE:-gradle}\"]\ntimeout = 5\nextra = [\"${HOME}/tools\"]\n\n[severity]\nBOK007 = \"error\"\nBOK008 = \"off\"\n";
        let c = parse(src, Path::new("t.toml"), &vars).unwrap();
        assert_eq!(c.skip, Some(vec!["gradle".to_string()]));
        assert_eq!(c.timeout, Some(5));
        assert_eq!(c.extra, Some(vec!["/home/me/tools".to_string()]));
        assert_eq!(c.severity[&Code::Bok007], Severity::Error);
        assert_eq!(c.severity[&Code::Bok008], Severity::Off);
    }

    #[test]
    fn errors_name_the_file_and_line() {
        let err = parse("[check]\n\nskip = [\"a\", \"${NOPE}\"]\n", Path::new("t.toml"), &vars).unwrap_err();
        assert!(err.starts_with("t.toml:3: variable NOPE is not set"), "{err}");
        assert!(parse("[check]\nbogus = 1\n", Path::new("t.toml"), &vars).unwrap_err().contains("bogus"));
        assert!(parse("[severity]\nBOK999 = \"off\"\n", Path::new("t.toml"), &vars).unwrap_err().contains("BOK999"));
        assert!(parse("[severity]\nBOK007 = \"loud\"\n", Path::new("t.toml"), &vars).is_err());
    }

    #[test]
    fn later_file_wins_and_severities_merge() {
        let mut c = Config::default();
        let user = "[check]\ntimeout = 5\nskip = [\"a\"]\n[severity]\nBOK007 = \"error\"\n";
        let project = "[check]\nskip = [\"b\"]\n[severity]\nBOK008 = \"off\"\n";
        c.merge(parse(user, Path::new("u"), &vars).unwrap(), "u".into());
        c.merge(parse(project, Path::new("p"), &vars).unwrap(), "p".into());
        assert_eq!(c.timeout, Some(5));
        assert_eq!(c.skip, Some(vec!["b".to_string()]));
        assert_eq!(c.severity.len(), 2);
        assert_eq!(c.files, [PathBuf::from("u"), PathBuf::from("p")]);
    }

    #[test]
    fn discovery_stops_at_git_and_reads_user_file_first() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let deep = repo.join("a").join("b");
        fs::create_dir_all(&deep).unwrap();
        fs::create_dir(repo.join(".git")).unwrap();
        let user = tmp.path().join("user.toml");
        fs::write(&user, "").unwrap();
        // Above the repo: not found, because the search stops at .git.
        fs::write(tmp.path().join(FILE_NAME), "").unwrap();
        assert_eq!(discover(&deep, Some(user.clone()), None), std::slice::from_ref(&user));

        fs::write(repo.join(FILE_NAME), "").unwrap();
        assert_eq!(discover(&deep, Some(user.clone()), None), [user, repo.join(FILE_NAME)]);
        assert_eq!(discover(&deep, Some(tmp.path().join("missing.toml")), None), [repo.join(FILE_NAME)]);
    }
}
