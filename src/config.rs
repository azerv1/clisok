//! `.clisok.toml`: defaults for `check` and `list` flags, severity per code,
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
use crate::output::OutputFormat;

/// The project file's name.
pub const FILE_NAME: &str = ".clisok.toml";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Trusted,
    Project,
}

/// Every setting, for the docs generator (a test): key, value type, meaning.
#[cfg_attr(not(test), allow(dead_code))]
pub const KEYS: &[(&str, &str, &str)] = &[
    (
        "check.skip",
        "list of strings",
        "Don't run these tools, like `--skip`. Project entries add to the user list.",
    ),
    (
        "check.timeout",
        "integer",
        "Seconds before a test run is killed, like `--timeout`.",
    ),
    (
        "check.jobs",
        "integer",
        "Tools checked in parallel, like `--jobs`.",
    ),
    (
        "check.system",
        "boolean",
        "Also check the OS's own folders, like `--system`. Also applies to `list`.",
    ),
    (
        "check.output-format",
        "`full`, `concise`, `json`, `github` or `sarif`",
        "How `check` prints the report, like `--output-format`.",
    ),
    (
        "check.registry",
        "boolean",
        "Windows only: also check the saved PATH and App Paths, like `--registry`. User file or `--config` only.",
    ),
    (
        "check.extra",
        "list of strings",
        "Files or folders off PATH to check statically; never run. Globs allowed. Also listed by `list`. User file or `--config` only.",
    ),
    (
        "severity.CLInnn",
        "\"error\", \"warn\" or \"off\"",
        "Override a code's severity. `off` drops it, like `--ignore`.",
    ),
    (
        "fix.fixable",
        "list of codes",
        "Only these codes' fixes may be applied by `--fix`; default all. Project and user lists intersect; an empty list allows none.",
    ),
    (
        "fix.unfixable",
        "list of codes",
        "These codes' fixes are never applied; they stay advice. Project entries add to the user list.",
    ),
    (
        "fix.unsafe-fixes",
        "boolean",
        "With `--fix`, also apply unsafe fixes, like `--unsafe-fixes`. User file or `--config` only.",
    ),
];

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    check: CheckFile,
    #[serde(default)]
    severity: HashMap<Code, Severity>,
    #[serde(default)]
    fix: FixFile,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct FixFile {
    fixable: Option<Vec<Code>>,
    unfixable: Option<Vec<Code>>,
    unsafe_fixes: Option<bool>,
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
    #[serde(rename = "output-format")]
    output_format: Option<OutputFormat>,
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
    pub output_format: Option<OutputFormat>,
    pub severity: HashMap<Code, Severity>,
    pub fixable: Option<Vec<Code>>,
    pub unfixable: Option<Vec<Code>>,
    pub unsafe_fixes: Option<bool>,
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
            Some(f) => vec![(f.clone(), Source::Trusted)],
            None => {
                let cwd = std::env::current_dir().unwrap_or_default();
                let user = user_file();
                discover(&cwd, user.clone(), home().as_deref())
                    .into_iter()
                    .map(|f| {
                        let source = if Some(&f) == user.as_ref() {
                            Source::Trusted
                        } else {
                            Source::Project
                        };
                        (f, source)
                    })
                    .collect()
            }
        };
        let mut config = Config::default();
        for (f, source) in files {
            let src = fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
            config.merge(parse(&src, &f, &env_var, source)?, f, source);
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

/// `%APPDATA%\clisok\config.toml`, or `$XDG_CONFIG_HOME/clisok/config.toml`
/// (default `~/.config`).
fn user_file() -> Option<PathBuf> {
    let dir = if cfg!(windows) {
        PathBuf::from(env_var("APPDATA")?)
    } else {
        env_var("XDG_CONFIG_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .or_else(|| Some(home()?.join(".config")))?
    };
    Some(dir.join("clisok").join("config.toml"))
}

/// The files to read, in order: the user file, then the nearest
/// `.clisok.toml` from `start` upward, stopping at a `.git` folder, home,
/// or (on Unix) before a directory owned by someone else.
fn discover(start: &Path, user: Option<PathBuf>, home: Option<&Path>) -> Vec<PathBuf> {
    discover_owned(start, user, home, &owned_directory)
}

fn discover_owned(
    start: &Path,
    user: Option<PathBuf>,
    home: Option<&Path>,
    owned: &dyn Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = user.into_iter().filter(|f| f.is_file()).collect();
    for dir in start.ancestors() {
        if !owned(dir) {
            break;
        }
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

#[cfg(unix)]
fn owned_directory(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no preconditions and does not modify process state.
    let uid = unsafe { libc::geteuid() };
    fs::metadata(dir).is_ok_and(|m| m.uid() == uid)
}

#[cfg(windows)]
fn owned_directory(_dir: &Path) -> bool {
    true
}

impl Config {
    pub fn report_files(&self) {
        for file in &self.files {
            eprintln!("clisok: config: {}", file.display());
        }
    }

    /// Later defaults win, but project lists may only narrow user restrictions.
    fn merge(&mut self, mut other: Config, file: PathBuf, source: Source) {
        if source == Source::Project {
            union(&mut self.skip, other.skip.take());
            union(&mut self.unfixable, other.unfixable.take());
            if let (Some(user), Some(project)) = (&mut self.fixable, &other.fixable) {
                user.retain(|code| project.contains(code));
                other.fixable = None;
            }
        }
        self.skip = other.skip.or(self.skip.take());
        self.timeout = other.timeout.or(self.timeout);
        self.jobs = other.jobs.or(self.jobs);
        self.system = other.system.or(self.system);
        self.registry = other.registry.or(self.registry);
        self.extra = other.extra.or(self.extra.take());
        self.output_format = other.output_format.or(self.output_format);
        self.severity.extend(other.severity);
        self.fixable = other.fixable.or(self.fixable.take());
        self.unfixable = other.unfixable.or(self.unfixable.take());
        self.unsafe_fixes = other.unsafe_fixes.or(self.unsafe_fixes);
        self.files.push(file);
    }
}

fn union<T: PartialEq>(existing: &mut Option<Vec<T>>, incoming: Option<Vec<T>>) {
    if let Some(incoming) = incoming {
        let values = existing.get_or_insert_with(Vec::new);
        for value in incoming {
            if !values.contains(&value) {
                values.push(value);
            }
        }
    }
}

/// Parse one file and expand its variables. Errors name the file and line.
fn parse(
    src: &str,
    path: &Path,
    lookup: &dyn Fn(&str) -> Option<String>,
    source: Source,
) -> Result<Config, String> {
    let mut file: File = toml::from_str(src).map_err(|e| format!("{}: {e}", path.display()))?;
    if source == Source::Project {
        // Filter before expansion: ignored project paths must not require variables.
        for (key, present) in [
            ("fix.unsafe-fixes", file.fix.unsafe_fixes.take().is_some()),
            ("check.registry", file.check.registry.take().is_some()),
            ("check.extra", file.check.extra.take().is_some()),
        ] {
            if present {
                eprintln!(
                    "clisok: {}: warning: ignoring {key} in project config; use the user config or --config",
                    path.display()
                );
            }
        }
    }
    let expand_all = |list: Option<Vec<Spanned<String>>>| -> Result<Option<Vec<String>>, String> {
        list.map(|items| {
            items
                .into_iter()
                .map(|s| {
                    let line = src[..s.span().start].matches('\n').count() + 1;
                    expand(s.get_ref(), lookup)
                        .map_err(|e| format!("{}:{line}: {e}", path.display()))
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
        output_format: c.output_format,
        severity: file.severity,
        fixable: file.fix.fixable,
        unfixable: file.fix.unfixable,
        unsafe_fixes: file.fix.unsafe_fixes,
        files: Vec::new(),
    })
}

/// Expand `${VAR}`, `${VAR:-default}` and `$$`. A variable that is unset (or
/// empty) and has no default is an error, as is any other use of `$`.
pub use clisok::parsing::expand;

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(
        src: &str,
        path: &Path,
        lookup: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Config, String> {
        super::parse(src, path, lookup, Source::Trusted)
    }

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
        assert_eq!(
            expand("cost $$5 ${HOME}", &vars).unwrap(),
            "cost $5 /home/me"
        );
        assert_eq!(expand("plain", &vars).unwrap(), "plain");
    }

    #[test]
    fn rejects_unset_and_malformed_variables() {
        assert!(
            expand("${NOPE}", &vars)
                .unwrap_err()
                .contains("NOPE is not set")
        );
        assert!(expand("$HOME/bin", &vars).is_err());
        assert!(expand("${HOME", &vars).is_err());
        assert!(expand("${}", &vars).is_err());
        assert!(expand("${A B}", &vars).is_err());
    }

    #[test]
    fn parses_a_file() {
        let src = "[check]\nskip = [\"${NOPE:-gradle}\"]\ntimeout = 5\nextra = [\"${HOME}/tools\"]\n\n[severity]\nCLI007 = \"error\"\nCLI008 = \"off\"\n";
        let c = parse(src, Path::new("t.toml"), &vars).unwrap();
        assert_eq!(c.skip, Some(vec!["gradle".to_string()]));
        assert_eq!(c.timeout, Some(5));
        assert_eq!(c.extra, Some(vec!["/home/me/tools".to_string()]));
        assert_eq!(c.severity[&Code::Cli007], Severity::Error);
        assert_eq!(c.severity[&Code::Cli008], Severity::Off);
    }

    #[test]
    fn parses_the_output_format() {
        let c = parse(
            "[check]\noutput-format = \"github\"\n",
            Path::new("t.toml"),
            &vars,
        )
        .unwrap();
        assert_eq!(c.output_format, Some(OutputFormat::Github));
        assert!(
            parse(
                "[check]\noutput-format = \"text\"\n",
                Path::new("t.toml"),
                &vars
            )
            .is_err()
        );
    }

    #[test]
    fn parses_the_fix_table() {
        let src = "[fix]\nfixable = [\"CLI001\", \"cli007\"]\nunfixable = [\"CLI301\"]\nunsafe-fixes = true\n";
        let c = parse(src, Path::new("t.toml"), &vars).unwrap();
        assert_eq!(c.fixable, Some(vec![Code::Cli001, Code::Cli007]));
        assert_eq!(c.unfixable, Some(vec![Code::Cli301]));
        assert_eq!(c.unsafe_fixes, Some(true));
        assert!(
            parse(
                "[fix]\nunfixable = [\"CLI999\"]\n",
                Path::new("t.toml"),
                &vars
            )
            .unwrap_err()
            .contains("CLI999")
        );
        assert!(parse("[fix]\nunsafe_fixes = true\n", Path::new("t.toml"), &vars).is_err());
    }

    #[test]
    fn errors_name_the_file_and_line() {
        let err = parse(
            "[check]\n\nskip = [\"a\", \"${NOPE}\"]\n",
            Path::new("t.toml"),
            &vars,
        )
        .unwrap_err();
        assert!(
            err.starts_with("t.toml:3: variable NOPE is not set"),
            "{err}"
        );
        assert!(
            parse("[check]\nbogus = 1\n", Path::new("t.toml"), &vars)
                .unwrap_err()
                .contains("bogus")
        );
        assert!(
            parse("[severity]\nCLI999 = \"off\"\n", Path::new("t.toml"), &vars)
                .unwrap_err()
                .contains("CLI999")
        );
        assert!(
            parse(
                "[severity]\nCLI007 = \"loud\"\n",
                Path::new("t.toml"),
                &vars
            )
            .is_err()
        );
    }

    #[test]
    fn later_file_wins_and_severities_merge() {
        let mut c = Config::default();
        let user = "[check]\ntimeout = 5\nskip = [\"a\"]\n[severity]\nCLI007 = \"error\"\n";
        let project = "[check]\nskip = [\"b\"]\n[severity]\nCLI008 = \"off\"\n";
        c.merge(
            parse(user, Path::new("u"), &vars).unwrap(),
            "u".into(),
            Source::Trusted,
        );
        c.merge(
            parse(project, Path::new("p"), &vars).unwrap(),
            "p".into(),
            Source::Trusted,
        );
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
        assert_eq!(
            discover(&deep, Some(user.clone()), None),
            std::slice::from_ref(&user)
        );

        fs::write(repo.join(FILE_NAME), "").unwrap();
        assert_eq!(
            discover(&deep, Some(user.clone()), None),
            [user, repo.join(FILE_NAME)]
        );
        assert_eq!(
            discover(&deep, Some(tmp.path().join("missing.toml")), None),
            [repo.join(FILE_NAME)]
        );
    }

    #[test]
    fn project_cannot_set_trusted_options_even_before_variable_expansion() {
        let src = "[check]\nregistry = true\nextra = ['${NOPE}']\n[fix]\nunsafe-fixes = true\n";
        let c = super::parse(src, Path::new("project"), &vars, Source::Project).unwrap();
        assert_eq!(c.registry, None);
        assert_eq!(c.extra, None);
        assert_eq!(c.unsafe_fixes, None);
        let c = parse(
            &src.replace("${NOPE}", "${HOME}"),
            Path::new("trusted"),
            &vars,
        )
        .unwrap();
        assert_eq!(c.registry, Some(true));
        assert_eq!(c.extra, Some(vec!["/home/me".into()]));
        assert_eq!(c.unsafe_fixes, Some(true));
    }

    #[test]
    fn project_lists_preserve_user_restrictions() {
        let user = "[check]\nskip = ['a']\ntimeout = 5\n[fix]\nfixable = ['CLI001', 'CLI007']\nunfixable = ['CLI301']\n";
        for (project, skips, fixable, unfixable) in [
            (
                "[check]\nskip = []\n[fix]\nunfixable = []\n",
                vec!["a"],
                vec![Code::Cli001, Code::Cli007],
                vec![Code::Cli301],
            ),
            (
                "[check]\nskip = ['b', 'a']\n[fix]\nfixable = ['CLI007', 'CLI301']\nunfixable = ['CLI007', 'CLI301']\n",
                vec!["a", "b"],
                vec![Code::Cli007],
                vec![Code::Cli301, Code::Cli007],
            ),
            (
                "[fix]\nfixable = []\n",
                vec!["a"],
                vec![],
                vec![Code::Cli301],
            ),
        ] {
            let mut c = parse(user, Path::new("user"), &vars).unwrap();
            let p = super::parse(project, Path::new("project"), &vars, Source::Project).unwrap();
            c.merge(p, "project".into(), Source::Project);
            assert_eq!(c.skip.unwrap(), skips);
            assert_eq!(c.fixable.unwrap(), fixable);
            assert_eq!(c.unfixable.unwrap(), unfixable);
            assert_eq!(c.timeout, Some(5));
        }
        // An absent user allowlist means unrestricted, not "allow no fixes".
        let mut c = Config::default();
        c.merge(
            parse("[fix]\nfixable = ['CLI007']\n", Path::new("p"), &vars).unwrap(),
            "p".into(),
            Source::Project,
        );
        assert_eq!(c.fixable, Some(vec![Code::Cli007]));
        c.merge(
            parse("[fix]\nfixable = ['CLI001']\n", Path::new("p"), &vars).unwrap(),
            "p".into(),
            Source::Project,
        );
        assert_eq!(c.fixable, Some(vec![]));
    }

    #[test]
    fn discovery_does_not_read_or_cross_a_foreign_owned_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let foreign = tmp.path().join("foreign");
        let start = foreign.join("owned");
        fs::create_dir_all(&start).unwrap();
        fs::write(foreign.join(FILE_NAME), "").unwrap();
        fs::write(tmp.path().join(FILE_NAME), "").unwrap();
        let user = tmp.path().join("user.toml");
        fs::write(&user, "").unwrap();
        assert_eq!(
            discover_owned(&start, Some(user.clone()), None, &|dir| dir != foreign),
            vec![user.clone()]
        );
        fs::remove_file(foreign.join(FILE_NAME)).unwrap();
        assert_eq!(
            discover_owned(&start, Some(user), None, &|dir| dir != foreign).len(),
            1
        );
        fs::write(start.join(FILE_NAME), "").unwrap();
        assert_eq!(
            discover_owned(&start, None, None, &|dir| dir != foreign),
            vec![start.join(FILE_NAME)]
        );
        assert!(discover_owned(&start, None, None, &|_| false).is_empty());
    }

    #[test]
    fn discovery_stops_at_home_before_looking_above_it() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let start = home.join("project");
        fs::create_dir_all(&start).unwrap();
        fs::write(tmp.path().join(FILE_NAME), "").unwrap();
        assert!(discover(&start, None, Some(&home)).is_empty());
        fs::write(home.join(FILE_NAME), "").unwrap();
        assert_eq!(
            discover(&start, None, Some(&home)),
            vec![home.join(FILE_NAME)]
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_discovery_checks_actual_directory_ownership() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(owned_directory(tmp.path()));
        assert!(!owned_directory(&tmp.path().join("missing")));
        // A privileged test run can also exercise a real foreign-owned directory.
        if unsafe { libc::geteuid() } == 0 {
            let foreign = tmp.path().join("foreign");
            let start = foreign.join("owned");
            fs::create_dir_all(&start).unwrap();
            fs::write(foreign.join(FILE_NAME), "").unwrap();
            std::os::unix::fs::chown(&foreign, Some(1), None).unwrap();
            assert!(!owned_directory(&foreign));
            assert!(discover(&start, None, None).is_empty());
        }
    }
}
