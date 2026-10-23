//! `clisok check`: PATH entries, then every tool on PATH — static checks
//! first, then a test run (`--version`, then `--help`) for the ones that are safe to run.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;

use crate::codes::{Code, Severities, Severity};
use crate::discover::{self, PathCommand, Tool};
use crate::exec::{self, Outcome, ToolErrorKind};
use crate::fix::{self, Applied};
use crate::inspect::{self, Kind, Reason, python};
use crate::output::{self, OutputFormat};
use crate::pathenv::{self, EntryProblem, PathEntry, PathIndex};
use crate::platform::{self, path_key};
use crate::report::{
    AppPathIssue, EntryIssue, Finding, Origin, Report, SCHEMA_VERSION, ToolResult, Verdict,
    print_stdout, statistics_text,
};
use crate::{pool, progress};

/// Installers and uninstallers can act on any flag, so they are never run.
static INSTALLER_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(uninst|^unins\d|^setup|^install)").unwrap());

#[derive(clap::Args)]
#[command(group(clap::ArgGroup::new("fix_mode").multiple(true).args(["fix", "diff"])))]
pub struct CheckArgs {
    /// Also check the saved (registry) PATH and App Paths registrations
    #[cfg(windows)]
    #[arg(long)]
    registry: bool,
    #[cfg_attr(windows, doc = "Also check Windows' own folder (%SystemRoot%);")]
    #[cfg_attr(
        unix,
        doc = "Also check the distro's own folders (/usr/bin, /bin, /usr/sbin, /sbin);"
    )]
    /// static checks only, nothing there is run
    #[arg(long)]
    system: bool,
    /// How to print the report [default: full]
    #[arg(long, value_enum, value_name = "FORMAT")]
    output_format: Option<OutputFormat>,
    /// Print how many findings each code has instead of the findings
    #[arg(long)]
    statistics: bool,
    /// Also list passing and skipped tools, and shadowed copies
    #[arg(short, long)]
    verbose: bool,
    /// Don't run this tool (repeatable)
    #[arg(long, value_name = "NAME")]
    skip: Vec<String>,
    /// Allow probes to run as root or elevated
    #[arg(long)]
    allow_root: bool,
    /// Apply the safe fixes (the `fix:` lines). On Windows that removes empty
    /// and repeated entries from the saved user PATH; implies --registry
    #[arg(long)]
    fix: bool,
    /// Also apply the unsafe fixes (`fix (unsafe):`): clean quotes and spaces
    /// off user PATH entries, delete user App Paths keys
    #[arg(long, requires = "fix_mode")]
    unsafe_fixes: bool,
    /// Print what --fix would change as a diff, then exit without changing
    /// or running anything; exit 1 if there is something to fix.
    /// On Windows, implies --registry
    #[arg(long)]
    diff: bool,
    /// List every fix --fix applied, not just how many
    #[arg(long, requires = "fix")]
    show_fixes: bool,
    /// Exit 1 when --fix changed something, even if nothing is left
    #[arg(long, requires = "fix")]
    exit_non_zero_on_fix: bool,
    /// Drop findings with this code, e.g. CLI007 (repeatable)
    #[arg(long, value_name = "CODE")]
    ignore: Vec<Code>,
    /// Number of tools checked in parallel [default: one per CPU core]
    #[arg(short, long)]
    jobs: Option<usize>,
    /// Seconds before a test run is killed [default: 30]
    #[arg(long, value_name = "SECS", value_parser = clap::value_parser!(u64).range(1..))]
    timeout: Option<u64>,
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
        reasons: insp
            .reasons
            .into_iter()
            .filter_map(|r| Finding::new(r, &ctx.severities))
            .collect(),
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
    } else if res.reasons.iter().any(|f| f.severity == Severity::Error) {
        // The verdict is already broken; a run would only repeat why.
        Some("not run: static checks already failed")
    } else if matches!(res.kind, Kind::AppAlias { .. }) {
        Some("not run: app execution alias (starts a packaged app)")
    } else if INSTALLER_NAME.is_match(&res.name) {
        Some("not run: looks like an installer or uninstaller")
    } else if insp.gui {
        Some("not run: GUI program")
    } else if matches!(res.kind, Kind::Partial { .. }) {
        Some(
            "not run: static inspection was incomplete (oversized, unreadable or unsupported input)",
        )
    } else if matches!(res.kind, Kind::Script) {
        Some("not run: script, not a launcher")
    } else if matches!(res.kind, Kind::Missing) {
        Some("not run: target is missing")
    } else {
        platform::probe_unavailable()
    };
    Prepared { res, skip_why }
}

/// A finding reported as the tool's note (hidden copy, tool error): its
/// verdict, unless the code's severity is overridden.
fn note_finding(
    res: &mut ToolResult,
    code: Code,
    note: String,
    default: Verdict,
    sev: &Severities,
) -> Verdict {
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

/// Preserve the skip reason while explaining an inconclusive module lookup.
fn skipped_note(kind: &Kind, why: &str) -> String {
    if let Kind::PyLauncher { python, module } = kind
        && python::module_found(python, module).is_none()
    {
        format!("{why}; module not found on disk; not probed (not run)")
    } else {
        why.to_string()
    }
}

/// Do the test run (unless skipped) and settle the verdict.
fn run(p: Prepared, ctx: &Ctx) -> ToolResult {
    let mut res = p.res;
    // Typing the name never reaches this copy, so by default it can't fail
    // the run; its static findings are still listed.
    if let Some(os_copy) = &res.hidden_by {
        let note = skipped_note(
            &res.kind,
            &format!(
                "hidden behind {}, which runs instead",
                platform::display_case(os_copy).display()
            ),
        );
        res.verdict = note_finding(&mut res, Code::Cli107, note, Verdict::Warn, &ctx.severities);
        return res;
    }
    let mut keep_attempts = false;
    let outcome = match p.skip_why {
        Some(why) => Outcome::Skip(skipped_note(&res.kind, why)),
        None => {
            let mut module_error = None;
            if let Kind::PyLauncher { python, module } = &res.kind
                && python::module_found(python, module).is_none()
            {
                let (found, attempt) = python::probe_module(python, module, ctx.timeout);
                if let Some(attempt) = attempt {
                    if let Some(signal) = attempt.signal.filter(|_| !attempt.timed_out) {
                        module_error = Some(Outcome::Broken(Reason::Crashed { signal }));
                    } else if attempt.timed_out {
                        module_error = Some(Outcome::ToolError(
                            ToolErrorKind::TimedOut,
                            format!("module probe timed out after {}s", ctx.timeout.as_secs()),
                        ));
                    }
                    res.ms += attempt.ms;
                    res.attempts.push(exec::trim_output(attempt));
                    keep_attempts = true;
                }
                if found == Some(false) {
                    module_error = Some(Outcome::Broken(Reason::MissingModule {
                        module: module.clone(),
                        python: python.clone(),
                    }));
                }
            }
            module_error.unwrap_or_else(|| {
                let (outcome, attempts) = exec::run_probe(&res.path, ctx.timeout);
                res.ms += attempts.iter().map(|a| a.ms).sum::<u64>();
                res.attempts
                    .extend(attempts.into_iter().map(exec::trim_output));
                outcome
            })
        }
    };

    let run_verdict = match outcome {
        Outcome::Broken(r) => {
            res.reasons.extend(Finding::new(r, &ctx.severities));
            Verdict::Pass
        }
        Outcome::Skip(note) => {
            // A tool with findings is explained by them, not by why it wasn't run.
            if res.reasons.is_empty() {
                res.note = Some(note);
            }
            Verdict::Skip
        }
        Outcome::Started(note) => {
            res.note = Some(note);
            Verdict::Pass
        }
        Outcome::ToolError(kind, msg) => {
            let code = match kind {
                ToolErrorKind::TimedOut => Code::Cli203,
                ToolErrorKind::Exit => Code::Cli204,
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
        if res.attempts.iter().any(|a| a.output_truncated) {
            res.note =
                Some("test output was truncated; retained output is included in the report".into());
        } else if !keep_attempts {
            res.attempts.clear();
        }
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
fn check_registry(args: &CheckArgs) -> bool {
    args.registry || args.fix || args.diff
}
#[cfg(unix)]
fn check_registry(_: &CheckArgs) -> bool {
    false
}

/// Suppress findings selected by `--ignore`.
fn severities(args: &CheckArgs) -> Severities {
    Severities(args.ignore.iter().map(|&c| (c, Severity::Off)).collect())
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
            tools.push(Tool {
                name: c.name.clone(),
                path: active.path.clone(),
                shadowed: paths_after(0),
                hidden_by: None,
            });
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
    pathenv::indexed_problems(entries)
        .into_iter()
        .filter_map(|(index, problem)| {
            let mut issue = EntryIssue::new(entries[index].clone(), problem, sev)?;
            issue.index = Some(index);
            Some(issue)
        })
        .collect()
}

/// Problems with PATH entries, this process's and (with `registry`) the saved
/// ones, and broken App Paths registrations.
fn path_findings(
    entries: &[PathEntry],
    persisted: &[PathEntry],
    cut: &[PathEntry],
    registrations: &[platform::AppPath],
    registry: bool,
    severities: &Severities,
) -> (Vec<EntryIssue>, Vec<AppPathIssue>) {
    let mut path_problems = entry_issues(entries, severities);

    // Entries saved in the registry that this process never received, e.g. a
    // terminal started before PATH was changed.
    let session: HashSet<String> = entries.iter().map(|e| path_key(&e.dir)).collect();
    let mut reported = HashSet::new();
    for e in persisted {
        let key = path_key(&e.dir);
        if e.dir.is_dir() && !session.contains(&key) && reported.insert(key) {
            path_problems.extend(EntryIssue::new(
                e.clone(),
                EntryProblem::NotInSession,
                severities,
            ));
        }
    }
    let mut app_paths = Vec::new();
    if registry {
        // Saved findings carry actionable fixes. Suppress matching process
        // findings instead of losing the saved finding in a fresh terminal.
        let saved = entry_issues(persisted, severities);
        let known: HashSet<(String, String)> = saved
            .iter()
            .map(|p| (path_key(&p.entry.dir), p.problem.to_string()))
            .collect();
        path_problems.retain(|p| {
            p.entry.origin != pathenv::PathOrigin::Process
                || !known.contains(&(path_key(&p.entry.dir), p.problem.to_string()))
        });
        path_problems.extend(saved);
        app_paths = registrations
            .iter()
            .cloned()
            .filter_map(|a| AppPathIssue::new(a, severities))
            .collect();
    }

    // A saved PATH cut off by a length limit lost every entry after the cut,
    // and nothing else shows that. Its stub is reported as the cut, not as a
    // plain missing folder.
    let cut_keys: HashSet<String> = cut.iter().map(|e| path_key(&e.dir)).collect();
    path_problems.retain(|p| {
        !(p.problem == EntryProblem::Missing && cut_keys.contains(&path_key(&p.entry.dir)))
    });
    path_problems.extend(
        cut.iter()
            .cloned()
            .filter_map(|entry| EntryIssue::new(entry, EntryProblem::Truncated, severities)),
    );
    (path_problems, app_paths)
}

/// Pretty JSON and a final newline.
fn to_json(value: &impl serde::Serialize) -> Result<String, String> {
    serde_json::to_string_pretty(value)
        .map(|j| format!("{j}\n"))
        .map_err(|e| e.to_string())
}

/// The PATH problems among the reported ones whose fix `--fix` may apply.
fn fixable_problems(
    path_problems: &[EntryIssue],
    unsafe_fixes: bool,
) -> Vec<(usize, EntryProblem)> {
    path_problems
        .iter()
        .filter(|i| i.entry.origin == pathenv::PathOrigin::User)
        .filter(|i| i.fix.as_ref().is_some_and(|f| f.applies(unsafe_fixes)))
        .filter_map(|i| Some((i.index?, i.problem)))
        .collect()
}

/// `--diff`: print what `apply_fixes` would change. Exit 1 when there is
/// something, like Ruff.
fn print_diff(
    saved: &platform::SavedPaths,
    path_problems: &[EntryIssue],
    app_paths: &[AppPathIssue],
    unsafe_fixes: bool,
) -> ExitCode {
    let fixable = fixable_problems(path_problems, unsafe_fixes);
    let mut out = String::new();
    if !fixable.is_empty() {
        match platform::preview_user_path(saved, &fixable) {
            Ok(Some((raw, edits))) => out.push_str(&fix::path_diff(&raw, &edits)),
            Ok(None) => {}
            Err(e) => {
                eprintln!("clisok: {e}");
                return ExitCode::from(2);
            }
        }
    }
    let keys: Vec<&str> = app_paths
        .iter()
        .filter(|a| a.fix.applies(unsafe_fixes))
        .map(|a| a.app_path.key.as_str())
        .collect();
    if !keys.is_empty() {
        out.push_str(&fix::app_paths_diff(&keys));
    }
    if out.is_empty() {
        eprintln!("clisok: nothing to fix");
        return ExitCode::SUCCESS;
    }
    print_stdout(&out);
    ExitCode::from(1)
}

/// Apply the reported fixes that `--fix` may apply. A code turned off is not
/// reported, so it is never fixed. A fix that fails is reported on stderr and
/// its finding stays in the report.
fn apply_fixes(
    saved: &platform::SavedPaths,
    path_problems: &[EntryIssue],
    app_paths: &[AppPathIssue],
    unsafe_fixes: bool,
) -> Vec<Applied> {
    let fixable = fixable_problems(path_problems, unsafe_fixes);
    let mut applied = Vec::new();
    let path_fix = if fixable.is_empty() {
        Ok(None)
    } else {
        platform::fix_user_path(saved, &fixable)
    };
    match path_fix {
        Ok(Some(repair)) => applied.extend(fix::applied_path(repair)),
        Ok(None) => {}
        Err(e) => eprintln!("clisok: {e}"),
    }
    for a in app_paths.iter().filter(|a| a.fix.applies(unsafe_fixes)) {
        match platform::delete_app_path(&a.app_path.key) {
            Ok(backup) => {
                let target = a.app_path.key.clone();
                applied.push(Applied {
                    code: a.code,
                    target,
                    message: "deleted".into(),
                    backup,
                });
            }
            Err(e) => eprintln!("clisok: {e}"),
        }
    }
    applied
}

fn probe_permission(
    args: &CheckArgs,
    privileged: impl FnOnce() -> std::io::Result<bool>,
) -> Result<(), String> {
    if args.diff || args.allow_root {
        return Ok(());
    }
    match privileged() {
        Ok(false) => Ok(()),
        Ok(true) => {
            Err("refusing to run probes as root or elevated; use --allow-root to override".into())
        }
        Err(e) => Err(format!("cannot determine probe privileges: {e}")),
    }
}

pub fn check(args: &CheckArgs) -> ExitCode {
    if let Err(error) = probe_permission(args, platform::is_privileged) {
        eprintln!("clisok: {error}");
        return ExitCode::from(2);
    }
    let format = args.output_format.unwrap_or(OutputFormat::Full);

    platform::init();
    // Ctrl+C: stop every running tool and give the cursor back before exiting.
    let _ = ctrlc::set_handler(|| {
        platform::kill_children();
        platform::cleanup();
        platform::cursor_visible(true);
        std::process::exit(130);
    });
    let jobs = args
        .jobs
        .unwrap_or_else(|| pool::default_jobs(JOBS_PER_CORE, MAX_JOBS))
        .max(1);

    let entries = pathenv::process_path();
    if entries.is_empty() {
        eprintln!("clisok: PATH is empty");
        return ExitCode::from(2);
    }

    let severities = severities(args);
    let registry = check_registry(args);
    let findings = || {
        let saved = platform::saved_paths();
        let registrations = if registry {
            platform::broken_app_paths()
        } else {
            Vec::new()
        };
        let (issues, apps) = path_findings(
            &entries,
            &saved.entries,
            &saved.cut,
            &registrations,
            registry,
            &severities,
        );
        (saved, issues, apps)
    };
    let (saved, mut path_problems, mut app_paths) = findings();
    let unsafe_fixes = args.unsafe_fixes;
    if args.diff {
        return print_diff(&saved, &path_problems, &app_paths, unsafe_fixes);
    }
    let fixed = if args.fix {
        apply_fixes(&saved, &path_problems, &app_paths, unsafe_fixes)
    } else {
        Vec::new()
    };
    if !fixed.is_empty() {
        // Fixes change saved settings only, not this process's PATH, so only
        // the saved-settings findings can have changed.
        (_, path_problems, app_paths) = findings();
    }

    let dirs: Vec<PathBuf> = entries.iter().map(|e| e.dir.clone()).collect();
    let index = PathIndex::build(&dirs);
    let tools = tools_to_check(
        &discover::inventory(&entries, &index, args.system),
        args.system,
    );
    eprintln!("clisok: checking {} tools...", tools.len());

    let ctx = Ctx {
        index: &index,
        dirs: &dirs,
        skip: args.skip.iter().map(|s| s.to_lowercase()).collect(),
        timeout: Duration::from_secs(args.timeout.unwrap_or(DEFAULT_TIMEOUT)),
        severities,
    };
    let prepared = pool::run_in_order(tools, jobs, |t| prepare(t, &ctx));
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
        let retry = Ctx {
            timeout: ctx.timeout.min(RETRY_TIMEOUT),
            ..ctx
        };
        eprintln!(
            "clisok: re-checking {} timed-out tools (up to {}s)...",
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
    let report = Report {
        schema: SCHEMA_VERSION,
        command: "check",
        fixed,
        path_problems,
        app_paths,
        tools,
    };
    let out = match (format, args.statistics) {
        (OutputFormat::Json, true) => to_json(&report.statistics()),
        (_, true) => Ok(statistics_text(&report.statistics())),
        (OutputFormat::Full, false) => Ok(report.to_text(args.verbose, args.show_fixes)),
        (OutputFormat::Concise, false) => Ok(output::concise(&report)),
        (OutputFormat::Json, false) => to_json(&report),
        (OutputFormat::Github, false) => Ok(output::github(&report)),
        (OutputFormat::Sarif, false) => to_json(&output::sarif(&report)),
    };
    match out {
        Ok(text) => print_stdout(&text),
        Err(e) => {
            eprintln!("clisok: {e}");
            return ExitCode::from(2);
        }
    }
    let fixed_any = args.exit_non_zero_on_fix && !report.fixed.is_empty();
    ExitCode::from(report.exit_code().max(u8::from(fixed_any)))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::discover::FileCopy;

    #[test]
    fn privilege_policy_requires_an_explicit_override_and_exempts_diff() {
        use clap::Parser;
        let args = |flags: &[&str]| {
            let cli = crate::Cli::try_parse_from([&["clisok", "check"], flags].concat()).unwrap();
            let Some(crate::Command::Check(args)) = cli.command else {
                panic!("expected check")
            };
            args
        };
        assert!(
            probe_permission(&args(&[]), || Ok(true))
                .unwrap_err()
                .contains("--allow-root")
        );
        assert!(probe_permission(&args(&[]), || Ok(false)).is_ok());
        assert!(
            probe_permission(&args(&[]), || Err(std::io::Error::other("query failed"))).is_err()
        );
        for flags in [&["--allow-root"][..], &["--diff"][..]] {
            assert!(
                probe_permission(&args(flags), || panic!(
                    "override/diff must not query privileges"
                ))
                .is_ok()
            );
        }
    }

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
                FileCopy {
                    path: program(os_dir),
                    os: true,
                },
                FileCopy {
                    path: program(user_dir),
                    os: false,
                },
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
        assert_eq!(
            tools[0].hidden_by.as_deref(),
            Some(command.copies[0].path.as_path())
        );

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

        let report = Report {
            schema: SCHEMA_VERSION,
            command: "check",
            fixed: vec![],
            path_problems: vec![],
            app_paths: vec![],
            tools: vec![res],
        };
        assert_eq!(report.exit_code(), 0);
        let text = report.to_text(false, false);
        assert!(text.contains("[WARN]   tool  "));
        assert!(text.contains("         CLI107 hidden behind "));
    }

    fn severities(pairs: &[(Code, Severity)]) -> Severities {
        Severities(pairs.iter().copied().collect())
    }

    #[test]
    fn path_findings_use_only_the_supplied_saved_entries() {
        use pathenv::PathOrigin;

        let tmp = tempfile::tempdir().unwrap();
        let process = PathEntry {
            raw: tmp.path().display().to_string(),
            dir: tmp.path().to_path_buf(),
            origin: PathOrigin::Process,
        };
        let saved_dir = tmp.path().join("saved");
        fs::create_dir(&saved_dir).unwrap();
        let saved = PathEntry {
            raw: saved_dir.display().to_string(),
            dir: saved_dir,
            origin: PathOrigin::User,
        };
        let cut_dir = tmp.path().join("cut");
        let cut = PathEntry {
            raw: cut_dir.display().to_string(),
            dir: cut_dir,
            origin: PathOrigin::User,
        };
        let registrations = [platform::AppPath {
            key: "HKCU\\test\\tool.exe".into(),
            target: tmp.path().join("gone.exe"),
        }];
        let persisted = [saved, cut.clone()];
        let sev = Severities::default();
        let (issues, apps) = path_findings(
            &[process],
            &persisted,
            std::slice::from_ref(&cut),
            &registrations,
            true,
            &sev,
        );
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].problem, EntryProblem::NotInSession);
        assert_eq!(issues[0].entry.origin, PathOrigin::User);
        assert_eq!(issues[1].problem, EntryProblem::Truncated);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].app_path.key, registrations[0].key);

        let (issues, apps) = path_findings(&[], &[], &[], &[], true, &sev);
        assert!(issues.is_empty() && apps.is_empty());
    }

    #[test]
    #[cfg(windows)]
    fn matching_process_finding_keeps_the_fixable_saved_duplicate() {
        let tmp = tempfile::tempdir().unwrap();
        let raw = tmp.path().display().to_string();
        let process = pathenv::parse_path(&format!("{raw};{raw}"), pathenv::PathOrigin::Process);
        let saved = pathenv::parse_path(&format!("{raw};{raw}"), pathenv::PathOrigin::User);
        let (issues, _) = path_findings(&process, &saved, &[], &[], true, &Severities::default());
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].code, Code::Cli007);
        assert_eq!(issues[0].entry.origin, pathenv::PathOrigin::User);
        assert_eq!(
            fixable_problems(&issues, false),
            [(1, EntryProblem::Duplicate)]
        );
    }

    #[test]
    #[cfg(windows)]
    fn fix_and_diff_enable_saved_settings_checks() {
        use clap::Parser;
        for (flags, expected) in [
            (vec![], false),
            (vec!["--registry"], true),
            (vec!["--fix"], true),
            (vec!["--diff"], true),
            (vec!["--fix", "--unsafe-fixes"], true),
            (vec!["--diff", "--unsafe-fixes"], true),
        ] {
            let cli = crate::Cli::try_parse_from([vec!["clisok", "check"], flags.clone()].concat())
                .unwrap();
            let Some(crate::Command::Check(args)) = cli.command else {
                panic!("expected check")
            };
            assert_eq!(check_registry(&args), expected, "{flags:?}");
        }
    }

    #[test]
    fn saved_path_and_app_path_errors_require_registry_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("missing");
        let entry = PathEntry {
            raw: dir.display().to_string(),
            dir: dir.clone(),
            origin: pathenv::PathOrigin::User,
        };
        let registrations = [platform::AppPath {
            key: "HKCU\\test\\tool.exe".into(),
            target: dir,
        }];
        let (issues, apps) = path_findings(
            &[],
            std::slice::from_ref(&entry),
            &[],
            &registrations,
            false,
            &Severities::default(),
        );
        assert!(issues.is_empty() && apps.is_empty());
        let (issues, apps) = path_findings(
            &[],
            &[entry],
            &[],
            &registrations,
            true,
            &Severities::default(),
        );
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].problem, EntryProblem::Missing);
        assert_eq!(apps.len(), 1);
    }

    #[test]
    fn system_and_other_skip_reasons_do_not_probe_python() {
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("unexpected-python.txt");
        #[cfg(windows)]
        let python = root.path().join("python.cmd");
        #[cfg(unix)]
        let python = root.path().join("python3");
        #[cfg(windows)]
        let text = format!(
            "@echo off\r\necho ran > \"{}\"\r\nexit /b 0\r\n",
            marker.display()
        );
        #[cfg(unix)]
        let text = format!("#!/bin/sh\nprintf ran > '{}'\n", marker.display());
        fs::write(&python, text).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&python, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let dirs = Vec::new();
        let index = PathIndex::build(&dirs);
        let ctx = Ctx {
            index: &index,
            dirs: &dirs,
            skip: HashSet::new(),
            timeout: Duration::from_secs(1),
            severities: Severities::default(),
        };
        for why in [
            "not run: system program (static checks only)",
            "not run: --skip",
        ] {
            let res = ToolResult {
                name: "fixture".into(),
                path: root.path().join("launcher"),
                origin: Origin::Path,
                kind: Kind::PyLauncher {
                    python: python.clone(),
                    module: "fixture".into(),
                },
                verdict: Verdict::Pass,
                reasons: vec![],
                note: None,
                code: None,
                shadowed: vec![],
                hidden_by: None,
                attempts: vec![],
                ms: 0,
            };
            let result = run(
                Prepared {
                    res,
                    skip_why: Some(why),
                },
                &ctx,
            );
            assert_eq!(result.verdict, Verdict::Skip);
            assert!(result.attempts.is_empty());
            assert!(result.note.unwrap().contains("not probed (not run)"));
            assert!(!marker.exists(), "{why} started Python");
        }
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
                severities: severities(&[(Code::Cli107, sev)]),
            };
            run(
                prepare(
                    tools_to_check(std::slice::from_ref(&command), false).remove(0),
                    &ctx,
                ),
                &ctx,
            )
        };

        let res = check_with(Severity::Error);
        assert_eq!(
            (res.verdict, res.code),
            (Verdict::Broken, Some(Code::Cli107))
        );
        let res = check_with(Severity::Off);
        assert_eq!((res.verdict, res.code), (Verdict::Skip, None));
        assert!(
            res.note
                .unwrap()
                .starts_with("ignored CLI107: hidden behind ")
        );
    }

    #[test]
    fn finding_set_to_warn_warns_instead_of_failing() {
        let dirs: Vec<PathBuf> = vec![];
        let index = PathIndex::build(&dirs);
        let sev = severities(&[(Code::Cli101, Severity::Warn)]);
        let ctx = Ctx {
            index: &index,
            dirs: &dirs,
            skip: HashSet::new(),
            timeout: Duration::from_secs(1),
            severities: sev,
        };
        let reason = || crate::inspect::Reason::MissingTarget {
            path: PathBuf::from("gone"),
        };
        let res = ToolResult {
            name: "tool".into(),
            path: PathBuf::from("tool"),
            origin: Origin::Path,
            kind: Kind::Missing,
            verdict: Verdict::Pass,
            reasons: Finding::new(reason(), &ctx.severities)
                .into_iter()
                .collect(),
            note: None,
            code: None,
            shadowed: vec![],
            hidden_by: None,
            attempts: vec![],
            ms: 0,
        };
        let res = run(
            Prepared {
                res,
                skip_why: Some("not run: target is missing"),
            },
            &ctx,
        );
        assert_eq!(res.verdict, Verdict::Warn);

        let json = serde_json::to_value(&res.reasons[0]).unwrap();
        assert_eq!(json["code"], "CLI101");
        assert_eq!(json["severity"], "warn");
        assert_eq!(json["type"], "missing-target");
        assert_eq!(json["path"], "gone");

        assert!(Finding::new(reason(), &severities(&[(Code::Cli101, Severity::Off)])).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn tool_that_failed_static_checks_is_not_run() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("tool.cmd");
        fs::write(
            &path,
            format!("@\"{}\" %*\r\n", tmp.path().join("gone.exe").display()),
        )
        .unwrap();
        let dirs = vec![tmp.path().to_path_buf()];
        let index = PathIndex::build(&dirs);
        let ctx = Ctx {
            index: &index,
            dirs: &dirs,
            skip: HashSet::new(),
            timeout: Duration::from_secs(1),
            severities: Severities::default(),
        };
        let tool = Tool {
            name: "tool".into(),
            path,
            shadowed: vec![],
            hidden_by: None,
        };
        let p = prepare(tool, &ctx);
        assert_eq!(p.skip_why, Some("not run: static checks already failed"));
        let res = run(p, &ctx);
        assert_eq!(res.verdict, Verdict::Broken);
        assert!(res.attempts.is_empty() && res.note.is_none());
        assert_eq!(res.reasons.len(), 1);
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
            fixed: vec![],
            path_problems: EntryIssue::new(entry.clone(), EntryProblem::Duplicate, sev)
                .into_iter()
                .collect(),
            app_paths: vec![],
            tools: vec![],
        };

        let default = report(&Severities::default());
        assert_eq!(default.exit_code(), 0);
        assert!(default.to_text(false, false).contains(
            "[WARN]   CLI007 (process) \"dup\" — duplicate entry\n         help: remove this later copy (where this terminal's PATH is set)\n"
        ));
        let json = serde_json::to_value(&default).unwrap();
        assert_eq!(
            json["path_problems"][0]["fix"]["applicability"],
            "display-only"
        );
        let strict = report(&severities(&[(Code::Cli007, Severity::Error)]));
        assert_eq!(strict.exit_code(), 1);
        assert!(strict.to_text(false, false).contains("[BROKEN] CLI007 "));
        assert!(
            report(&severities(&[(Code::Cli007, Severity::Off)]))
                .path_problems
                .is_empty()
        );
    }

    #[cfg(windows)]
    #[test]
    fn safe_fixes_are_marked_counted_and_reported_once_applied() {
        let user = |raw: &str| pathenv::PathEntry {
            raw: raw.into(),
            dir: PathBuf::from(raw.trim()),
            origin: pathenv::PathOrigin::User,
        };
        let sev = Severities::default();
        let issues = [
            (user("dup"), EntryProblem::Duplicate),
            (user(" sp"), EntryProblem::Whitespace),
        ];
        let mut report = Report {
            schema: SCHEMA_VERSION,
            command: "check",
            fixed: vec![],
            path_problems: issues
                .into_iter()
                .filter_map(|(e, p)| EntryIssue::new(e, p, &sev))
                .collect(),
            app_paths: vec![],
            tools: vec![],
        };
        let text = report.to_text(false, false);
        assert!(text.contains("[WARN]   CLI007 [*] (user) \"dup\" — duplicate entry\n         fix: remove it from the user PATH\n"));
        // Unsafe fixes get no [*].
        assert!(text.contains("[BROKEN] CLI006 (user) \" sp\" — has leading/trailing whitespace\n         fix (unsafe): trim the spaces in the user PATH\n"));
        assert!(text.ends_with("[*] 1 fixable with --fix (1 more with --unsafe-fixes).\n"));

        report.path_problems.clear();
        report.fixed.push(Applied {
            code: Code::Cli007,
            target: "(user) \"dup\"".into(),
            message: "removed".into(),
            backup: PathBuf::from(r"C:\b\user-Path-1.txt"),
        });
        // By default only the count and where the backups went.
        let text = report.to_text(false, false);
        assert!(!text.contains("[FIXED]"));
        assert!(text.ends_with("Fixed 1 problem; backups in C:\\b.\n"));
        // --show-fixes lists each one.
        let text = report.to_text(false, true);
        assert!(text.starts_with(
            "Fixed\n[FIXED]  CLI007 (user) \"dup\" — removed\n         backup of the old value: C:\\b\\user-Path-1.txt\n"
        ));
        assert!(text.ends_with("Fixed 1 problem.\n"));
    }

    #[cfg(windows)]
    #[test]
    fn statistics_count_codes_and_mark_how_many_fix_fixes() {
        use pathenv::PathOrigin::{Process, User};
        let entry = |raw: &str, origin| pathenv::PathEntry {
            raw: raw.into(),
            dir: PathBuf::from(raw),
            origin,
        };
        let sev = Severities::default();
        let issues = [
            (entry("a", User), EntryProblem::Duplicate),
            (entry("b", Process), EntryProblem::Duplicate),
            (entry("c", User), EntryProblem::Empty),
            (entry("d", User), EntryProblem::Empty),
            (entry("e", Process), EntryProblem::Missing),
        ];
        let report = Report {
            schema: SCHEMA_VERSION,
            command: "check",
            fixed: vec![],
            path_problems: issues
                .into_iter()
                .filter_map(|(e, p)| EntryIssue::new(e, p, &sev))
                .collect(),
            app_paths: vec![],
            tools: vec![],
        };
        // Ties keep code order.
        assert_eq!(
            statistics_text(&report.statistics()),
            "2  CLI001  [*] empty entry\n2  CLI007  [-] duplicate entry\n1  CLI002  [ ] directory does not exist\n"
        );
        let json = serde_json::to_value(report.statistics()).unwrap();
        assert_eq!(
            json[0],
            serde_json::json!({"code": "CLI001", "name": "empty entry", "count": 2, "fixable": true})
        );
        assert_eq!(json[1]["fixable"], false);
    }
}
