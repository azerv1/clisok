//! `binok check`: PATH entries, then every tool on PATH — static checks
//! first, then a test run (`--version`, then `--help`) for the ones that are safe to run.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;

use crate::codes::{Code, Severities, Severity};
use crate::config::{Config, ConfigArgs};
use crate::discover::{self, PathCommand, Tool};
use crate::exec::{self, Outcome, ToolErrorKind};
use crate::inspect::{self, Kind};
use crate::pathenv::{self, EntryProblem, PathIndex};
use crate::platform::{self, path_key};
use crate::report::{
    AppPathIssue, EntryIssue, Finding, Origin, Report, SCHEMA_VERSION, ToolResult, Verdict, print_stdout,
};
use crate::{pool, progress};

/// Installers and uninstallers can act on any flag, so they are never run.
static INSTALLER_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(uninst|^unins\d|^setup|^install)").unwrap());

#[derive(clap::Args)]
pub struct CheckArgs {
    /// Also check the saved (registry) PATH and App Paths registrations
    #[cfg(windows)]
    #[arg(long)]
    registry: bool,
    #[cfg_attr(windows, doc = "Also check Windows' own folder (%SystemRoot%);")]
    #[cfg_attr(unix, doc = "Also check the distro's own folders (/usr/bin, /bin, /usr/sbin, /sbin);")]
    /// static checks only, nothing there is run
    #[arg(long)]
    system: bool,
    /// Print the report as JSON
    #[arg(long)]
    json: bool,
    /// Also list passing and skipped tools, and shadowed copies
    #[arg(short, long)]
    verbose: bool,
    /// Don't run this tool (repeatable)
    #[arg(long, value_name = "NAME")]
    skip: Vec<String>,
    /// Drop findings with this code, e.g. BOK007 (repeatable)
    #[arg(long, value_name = "CODE")]
    ignore: Vec<Code>,
    /// Number of tools checked in parallel [default: one per CPU core]
    #[arg(short, long)]
    jobs: Option<usize>,
    /// Seconds before a test run is killed [default: 30]
    #[arg(long, value_name = "SECS")]
    timeout: Option<u64>,
    #[command(flatten)]
    config: ConfigArgs,
}

const DEFAULT_TIMEOUT: u64 = 30;

/// Workers per core. The tools being run are CPU-heavy (interpreter
/// startup), so more workers than cores only slows every tool down.
const JOBS_PER_CORE: usize = 1;
const MAX_JOBS: usize = 64;

/// Timeout for the second try of programs that timed out under full load.
const RETRY_TIMEOUT: Duration = Duration::from_secs(10);

struct Ctx<'a> {
    index: &'a PathIndex,
    dirs: &'a [PathBuf],
    skip: HashSet<String>,
    timeout: Duration,
    severities: Severities,
}

/// A tool after its static checks, with the decision whether to run it.
struct Prepared {
    res: ToolResult,
    skip_why: Option<&'static str>,
}

/// Static checks only: cheap, and decides whether a test run is safe.
fn prepare(tool: Tool, ctx: &Ctx) -> Prepared {
    let insp = inspect::inspect(&tool.path, ctx.index, ctx.dirs);
    let res = ToolResult {
        name: tool.name,
        path: tool.path,
        origin: Origin::Path,
        kind: insp.kind,
        verdict: Verdict::Pass,
        reasons: insp.reasons.into_iter().filter_map(|r| Finding::new(r, &ctx.severities)).collect(),
        note: None,
        code: None,
        shadowed: tool.shadowed,
        hidden_by: tool.hidden_by,
        attempts: Vec::new(),
        ms: 0,
    };

    let skip_why = if res.hidden_by.is_some() {
        Some("not run: an OS copy runs instead")
    } else if let Some(why) = platform::never_run(&res.path) {
        Some(why)
    } else if ctx.skip.contains(&res.name.to_lowercase()) {
        Some("not run: --skip")
    } else if matches!(res.kind, Kind::AppAlias { .. }) {
        Some("not run: app execution alias (starts a packaged app)")
    } else if INSTALLER_NAME.is_match(&res.name) {
        Some("not run: looks like an installer or uninstaller")
    } else if insp.gui {
        Some("not run: GUI program")
    } else if matches!(res.kind, Kind::Script) {
        Some("not run: script, not a launcher")
    } else if matches!(res.kind, Kind::Missing) {
        Some("not run: target is missing")
    } else {
        None
    };
    Prepared { res, skip_why }
}

/// A finding reported as the tool's note (hidden copy, tool error): its
/// verdict, unless the code's severity is overridden.
fn note_finding(res: &mut ToolResult, code: Code, note: String, default: Verdict, sev: &Severities) -> Verdict {
    match sev.get(code) {
        Some(Severity::Off) => {
            res.note = Some(format!("ignored {code}: {note}"));
            Verdict::Skip
        }
        over => {
            res.note = Some(note);
            res.code = Some(code);
            match over {
                Some(Severity::Error) => Verdict::Broken,
                Some(_) => Verdict::Warn,
                None => default,
            }
        }
    }
}

/// A tool at one of the config's `extra` places: static checks only.
fn prepare_extra(tool: Tool, ctx: &Ctx) -> Prepared {
    let mut p = prepare(tool, ctx);
    p.res.origin = Origin::Extra;
    p.skip_why = Some("not run: extra place, static checks only");
    p
}

/// Tools at the config's `extra` places that PATH doesn't already cover.
fn extra_tools(patterns: &[String], on_path: &[Tool]) -> Result<Vec<Tool>, String> {
    let known: HashSet<String> = on_path.iter().map(|t| path_key(&t.path)).collect();
    Ok(discover::extra_files(patterns)?
        .into_iter()
        .filter(|f| !known.contains(&path_key(f)))
        .map(|path| Tool {
            name: discover::file_command(&path),
            path,
            shadowed: Vec::new(),
            hidden_by: None,
        })
        .collect())
}

/// Do the test run (unless skipped) and settle the verdict.
fn run(p: Prepared, ctx: &Ctx) -> ToolResult {
    let mut res = p.res;
    // Typing the name never reaches this copy, so by default it can't fail
    // the run; its static findings are still listed.
    if let Some(os_copy) = &res.hidden_by {
        let note = format!("hidden behind {}, which runs instead", os_copy.display());
        res.verdict = note_finding(&mut res, Code::Bok107, note, Verdict::Warn, &ctx.severities);
        return res;
    }
    let outcome = match p.skip_why {
        Some(why) => Outcome::Skip(why.to_string()),
        None => {
            let (outcome, attempts) = exec::run_probe(&res.path, ctx.timeout);
            res.ms = attempts.iter().map(|a| a.ms).sum();
            res.attempts = attempts.into_iter().map(exec::trim_output).collect();
            outcome
        }
    };

    let run_verdict = match outcome {
        Outcome::Broken(r) => {
            res.reasons.extend(Finding::new(r, &ctx.severities));
            Verdict::Pass
        }
        Outcome::Skip(note) => {
            res.note = Some(note);
            Verdict::Skip
        }
        Outcome::Started(note) => {
            res.note = Some(note);
            Verdict::Pass
        }
        Outcome::ToolError(kind, msg) => {
            let code = match kind {
                ToolErrorKind::TimedOut => Code::Bok203,
                ToolErrorKind::Exit => Code::Bok204,
            };
            note_finding(&mut res, code, msg, Verdict::ToolError, &ctx.severities)
        }
        Outcome::Pass => Verdict::Pass,
    };
    // A failing finding outranks whatever the run showed: the path is still
    // dead. Findings set to warn make it a warning at most.
    res.verdict = if res.reasons.iter().any(|f| f.severity == Severity::Error) {
        Verdict::Broken
    } else if res.reasons.is_empty() {
        run_verdict
    } else {
        run_verdict.min(Verdict::Warn)
    };
    if res.verdict == Verdict::Pass {
        res.attempts.clear();
    }
    res
}

/// Expected cost, lowest first: interpreted launchers start slowest, so they
/// go first; skipped tools cost nothing and go last.
fn cost_rank(p: &Prepared) -> u8 {
    match (&p.skip_why, &p.res.kind) {
        (Some(_), _) => 2,
        (None, Kind::PyLauncher { .. } | Kind::Shim) => 0,
        (None, _) => 1,
    }
}

#[cfg(windows)]
fn check_registry(args: &CheckArgs, config: &Config) -> bool {
    args.registry || config.registry.unwrap_or(false)
}
#[cfg(unix)]
fn check_registry(_: &CheckArgs, _: &Config) -> bool {
    false
}

/// Severities from the config, with `--ignore` on top.
fn severities(args: &CheckArgs, config: &Config) -> Severities {
    let mut map = config.severity.clone();
    map.extend(args.ignore.iter().map(|&c| (c, Severity::Off)));
    Severities(map)
}

/// For each command, the copy that runs. OS copies only with --system, and
/// even then they are never run (`platform::never_run`). When an OS copy runs,
/// the user's copy it hides is added too, for static checks only.
fn tools_to_check(commands: &[PathCommand], include_system: bool) -> Vec<Tool> {
    let mut tools = Vec::new();
    for c in commands {
        let active = c.active();
        let paths_after = |i: usize| c.copies[i + 1..].iter().map(|s| s.path.clone()).collect();
        if platform::is_runnable(&active.path) && (include_system || !active.os) {
            tools.push(Tool { name: c.name.clone(), path: active.path.clone(), shadowed: paths_after(0), hidden_by: None });
        }
        if let Some(i) = c.hidden_by_os()
            && platform::is_runnable(&c.copies[i].path)
        {
            tools.push(Tool {
                name: c.name.clone(),
                path: c.copies[i].path.clone(),
                shadowed: paths_after(i),
                hidden_by: Some(active.path.clone()),
            });
        }
    }
    tools
}

fn entry_issues(entries: &[pathenv::PathEntry], sev: &Severities) -> Vec<EntryIssue> {
    pathenv::check_entries(entries)
        .into_iter()
        .filter_map(|(entry, problem)| EntryIssue::new(entry, problem, sev))
        .collect()
}

pub fn check(args: &CheckArgs) -> ExitCode {
    let config = match args.config.load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("binok: {e}");
            return ExitCode::from(2);
        }
    };
    let system = args.system || config.system.unwrap_or(false);
    let skip = if args.skip.is_empty() { config.skip.clone().unwrap_or_default() } else { args.skip.clone() };

    platform::init();
    // Ctrl+C: stop every running tool and give the cursor back before exiting.
    let _ = ctrlc::set_handler(|| {
        platform::kill_children();
        platform::cursor_visible(true);
        std::process::exit(130);
    });
    let jobs = args.jobs.or(config.jobs).unwrap_or_else(|| pool::default_jobs(JOBS_PER_CORE, MAX_JOBS)).max(1);

    let entries = pathenv::process_path();
    if entries.is_empty() {
        eprintln!("binok: PATH is empty");
        return ExitCode::from(2);
    }

    let severities = severities(args, &config);
    let mut path_problems = entry_issues(&entries, &severities);

    // Entries saved in the registry that this process never received, e.g. a
    // terminal started before PATH was changed.
    let session: HashSet<String> = entries.iter().map(|e| path_key(&e.dir)).collect();
    let mut reported = HashSet::new();
    for e in platform::persisted_path() {
        let key = path_key(&e.dir);
        if e.dir.is_dir() && !session.contains(&key) && reported.insert(key) {
            path_problems.extend(EntryIssue::new(e, EntryProblem::NotInSession, &severities));
        }
    }
    let mut app_paths = Vec::new();
    if check_registry(args, &config) {
        // Report persisted problems the process PATH doesn't already show.
        let known: HashSet<(String, String)> = path_problems
            .iter()
            .map(|p| (path_key(&p.entry.dir), p.problem.to_string()))
            .collect();
        path_problems.extend(
            entry_issues(&platform::persisted_path(), &severities)
                .into_iter()
                .filter(|p| !known.contains(&(path_key(&p.entry.dir), p.problem.to_string()))),
        );
        app_paths =
            platform::broken_app_paths().into_iter().filter_map(|a| AppPathIssue::new(a, &severities)).collect();
    }

    // A saved PATH cut off by a length limit lost every entry after the cut,
    // and nothing else shows that. Its stub is reported as the cut, not as a
    // plain missing folder.
    let cut = platform::truncated_saved_paths();
    let cut_keys: HashSet<String> = cut.iter().map(|e| path_key(&e.dir)).collect();
    path_problems.retain(|p| !(p.problem == EntryProblem::Missing && cut_keys.contains(&path_key(&p.entry.dir))));
    path_problems
        .extend(cut.into_iter().filter_map(|entry| EntryIssue::new(entry, EntryProblem::Truncated, &severities)));

    let dirs: Vec<PathBuf> = entries.iter().map(|e| e.dir.clone()).collect();
    let index = PathIndex::build(&dirs);
    let tools = tools_to_check(&discover::inventory(&entries, &index, system), system);
    let extra = match extra_tools(config.extra.as_deref().unwrap_or_default(), &tools) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("binok: {e}");
            return ExitCode::from(2);
        }
    };
    eprintln!("binok: checking {} tools...", tools.len() + extra.len());

    let ctx = Ctx {
        index: &index,
        dirs: &dirs,
        skip: skip.iter().map(|s| s.to_lowercase()).collect(),
        timeout: Duration::from_secs(args.timeout.or(config.timeout).unwrap_or(DEFAULT_TIMEOUT)),
        severities,
    };
    let mut prepared = pool::run_in_order(tools, jobs, |t| prepare(t, &ctx));
    prepared.extend(pool::run_in_order(extra, jobs, |t| prepare_extra(t, &ctx)));
    let mut queue: Vec<(usize, Prepared)> = prepared.into_iter().enumerate().collect();
    queue.sort_by_key(|(_, p)| cost_rank(p)); // stable: discovery order within a rank

    let progress = progress::Progress::new(queue.len());
    let mut done = pool::run_in_order(queue, jobs, |(i, p)| {
        let res = run(p, &ctx);
        progress.tick(&res.name);
        (i, res)
    });
    progress.finish();
    done.sort_by_key(|(i, _)| *i);
    let mut tools: Vec<ToolResult> = done.into_iter().map(|(_, r)| r).collect();

    // Slow starters can time out only because hundreds of programs share the
    // machine. Give each one more try once the load is gone.
    let timed_out: Vec<usize> = (0..tools.len())
        .filter(|&i| tools[i].attempts.iter().any(|a| a.timed_out))
        .collect();
    if !timed_out.is_empty() {
        // Alone, a slow starter finishes in seconds; a real hang shouldn't
        // cost the full timeout a second time.
        let retry = Ctx { timeout: ctx.timeout.min(RETRY_TIMEOUT), ..ctx };
        eprintln!(
            "binok: re-checking {} timed-out tools (up to {}s)...",
            timed_out.len(),
            retry.timeout.as_secs()
        );
        let progress = progress::Progress::new(timed_out.len());
        let rechecked = pool::run_in_order(timed_out.clone(), jobs, |i| {
            let t = &tools[i];
            let tool = Tool {
                name: t.name.clone(),
                path: t.path.clone(),
                shadowed: t.shadowed.clone(),
                hidden_by: t.hidden_by.clone(),
            };
            let res = run(prepare(tool, &retry), &retry);
            progress.tick(&res.name);
            res
        });
        progress.finish();
        for (i, res) in timed_out.into_iter().zip(rechecked) {
            tools[i] = res;
        }
    }

    platform::cleanup();
    let report =
        Report { schema: SCHEMA_VERSION, command: "check", config: config.files, path_problems, app_paths, tools };
    if args.json {
        match serde_json::to_string_pretty(&report) {
            Ok(j) => print_stdout(&format!("{j}\n")),
            Err(e) => {
                eprintln!("binok: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        print_stdout(&report.to_text(args.verbose));
    }
    ExitCode::from(report.exit_code())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::discover::FileCopy;

    /// An empty file the platform counts as a program named `tool`.
    fn program(dir: &Path) -> PathBuf {
        let path = dir.join(if cfg!(windows) { "tool.exe" } else { "tool" });
        fs::write(&path, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    /// `tool` where the copy in `os_dir` is marked as the OS's and runs first.
    fn hidden_by_os(os_dir: &Path, user_dir: &Path) -> PathCommand {
        PathCommand {
            name: "tool".into(),
            copies: vec![
                FileCopy { path: program(os_dir), os: true },
                FileCopy { path: program(user_dir), os: false },
            ],
        }
    }

    #[test]
    fn os_copy_hiding_a_user_copy_checks_the_user_copy() {
        let (os_dir, user_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let command = hidden_by_os(os_dir.path(), user_dir.path());

        let tools = tools_to_check(std::slice::from_ref(&command), false);
        assert_eq!(tools.len(), 1);
        assert!(tools[0].path.starts_with(user_dir.path()));
        assert_eq!(tools[0].hidden_by.as_deref(), Some(command.copies[0].path.as_path()));

        // With --system the OS copy is checked as well, ahead of the hidden one.
        let tools = tools_to_check(std::slice::from_ref(&command), true);
        assert_eq!(tools.len(), 2);
        assert!(tools[0].hidden_by.is_none() && tools[0].path.starts_with(os_dir.path()));
        assert_eq!(tools[0].shadowed, [command.copies[1].path.clone()]);
    }

    #[test]
    fn hidden_copy_warns_without_running_or_failing() {
        let (os_dir, user_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let command = hidden_by_os(os_dir.path(), user_dir.path());
        let dirs = vec![os_dir.path().to_path_buf(), user_dir.path().to_path_buf()];
        let index = PathIndex::build(&dirs);
        let ctx = Ctx {
            index: &index,
            dirs: &dirs,
            skip: HashSet::new(),
            timeout: Duration::from_secs(1),
            severities: Severities::default(),
        };

        let tool = tools_to_check(&[command], false).remove(0);
        let res = run(prepare(tool, &ctx), &ctx);
        assert_eq!(res.verdict, Verdict::Warn);
        assert!(res.attempts.is_empty() && res.ms == 0);
        assert!(res.note.as_deref().unwrap().starts_with("hidden behind "));

        let report =
            Report {
                schema: SCHEMA_VERSION,
                command: "check",
                config: vec![],
                path_problems: vec![],
                app_paths: vec![],
                tools: vec![res],
            };
        assert_eq!(report.exit_code(), 0);
        let text = report.to_text(false);
        assert!(text.contains("[WARN]   tool  "));
        assert!(text.contains("         BOK107 hidden behind "));
    }

    fn severities(pairs: &[(Code, Severity)]) -> Severities {
        Severities(pairs.iter().copied().collect())
    }

    #[test]
    fn severity_override_changes_the_hidden_copy_verdict() {
        let (os_dir, user_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let command = hidden_by_os(os_dir.path(), user_dir.path());
        let dirs = vec![os_dir.path().to_path_buf(), user_dir.path().to_path_buf()];
        let index = PathIndex::build(&dirs);
        let check_with = |sev: Severity| {
            let ctx = Ctx {
                index: &index,
                dirs: &dirs,
                skip: HashSet::new(),
                timeout: Duration::from_secs(1),
                severities: severities(&[(Code::Bok107, sev)]),
            };
            run(prepare(tools_to_check(std::slice::from_ref(&command), false).remove(0), &ctx), &ctx)
        };

        let res = check_with(Severity::Error);
        assert_eq!((res.verdict, res.code), (Verdict::Broken, Some(Code::Bok107)));
        let res = check_with(Severity::Off);
        assert_eq!((res.verdict, res.code), (Verdict::Skip, None));
        assert!(res.note.unwrap().starts_with("ignored BOK107: hidden behind "));
    }

    #[test]
    fn finding_set_to_warn_warns_instead_of_failing() {
        let dirs: Vec<PathBuf> = vec![];
        let index = PathIndex::build(&dirs);
        let sev = severities(&[(Code::Bok101, Severity::Warn)]);
        let ctx = Ctx { index: &index, dirs: &dirs, skip: HashSet::new(), timeout: Duration::from_secs(1), severities: sev };
        let reason = || crate::inspect::Reason::MissingTarget { path: PathBuf::from("gone") };
        let res = ToolResult {
            name: "tool".into(),
            path: PathBuf::from("tool"),
            origin: Origin::Path,
            kind: Kind::Missing,
            verdict: Verdict::Pass,
            reasons: Finding::new(reason(), &ctx.severities).into_iter().collect(),
            note: None,
            code: None,
            shadowed: vec![],
            hidden_by: None,
            attempts: vec![],
            ms: 0,
        };
        let res = run(Prepared { res, skip_why: Some("not run: target is missing") }, &ctx);
        assert_eq!(res.verdict, Verdict::Warn);

        let json = serde_json::to_value(&res.reasons[0]).unwrap();
        assert_eq!(json["code"], "BOK101");
        assert_eq!(json["severity"], "warn");
        assert_eq!(json["type"], "missing-target");
        assert_eq!(json["path"], "gone");

        assert!(Finding::new(reason(), &severities(&[(Code::Bok101, Severity::Off)])).is_none());
    }

    #[test]
    fn extra_places_are_checked_but_never_run() {
        let tmp = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let (file, expected) = {
            let file = tmp.path().join("tool.cmd");
            fs::write(&file, "@\"C:\\definitely\\missing\\real.exe\" %*\r\n").unwrap();
            (file, Code::Bok101)
        };
        #[cfg(unix)]
        let (file, expected) = {
            use std::os::unix::fs::PermissionsExt;
            let file = tmp.path().join("tool");
            fs::write(&file, "#!/definitely/missing/python\n").unwrap();
            fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
            (file, Code::Bok103)
        };
        let places = [tmp.path().display().to_string()];
        let mut tools = extra_tools(&places, &[]).unwrap();
        assert_eq!(tools.len(), 1);

        let dirs: Vec<PathBuf> = vec![];
        let index = PathIndex::build(&dirs);
        let ctx = Ctx {
            index: &index,
            dirs: &dirs,
            skip: HashSet::new(),
            timeout: Duration::from_secs(1),
            severities: Severities::default(),
        };
        let res = run(prepare_extra(tools.remove(0), &ctx), &ctx);
        assert_eq!((res.origin, res.verdict), (Origin::Extra, Verdict::Broken));
        assert!(res.attempts.is_empty());
        assert_eq!(res.reasons[0].code, expected);

        let report = Report {
            schema: SCHEMA_VERSION,
            command: "check",
            config: vec![],
            path_problems: vec![],
            app_paths: vec![],
            tools: vec![res],
        };
        assert!(report.to_text(false).contains("Extra\n[BROKEN] tool  "));

        // A program PATH already covers is not checked twice.
        let on_path = Tool { name: "tool".into(), path: file, shadowed: vec![], hidden_by: None };
        assert!(extra_tools(&places, &[on_path]).unwrap().is_empty());
    }

    #[test]
    fn entry_codes_follow_their_severity() {
        let entry = pathenv::PathEntry {
            raw: "dup".into(),
            dir: PathBuf::from("dup"),
            origin: pathenv::PathOrigin::Process,
        };
        let report = |sev: &Severities| Report {
            schema: SCHEMA_VERSION,
            command: "check",
            config: vec![],
            path_problems: EntryIssue::new(entry.clone(), EntryProblem::Duplicate, sev).into_iter().collect(),
            app_paths: vec![],
            tools: vec![],
        };

        let default = report(&Severities::default());
        assert_eq!(default.exit_code(), 0);
        assert!(default.to_text(false).contains("[WARN]   BOK007 (process) \"dup\" — duplicate entry"));
        let strict = report(&severities(&[(Code::Bok007, Severity::Error)]));
        assert_eq!(strict.exit_code(), 1);
        assert!(strict.to_text(false).contains("[BROKEN] BOK007 "));
        assert!(report(&severities(&[(Code::Bok007, Severity::Off)])).path_problems.is_empty());
    }
}
