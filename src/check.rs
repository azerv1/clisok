//! `pathsok check`: PATH entries, then every tool on PATH — static checks
//! first, then `--help` for the ones that are safe to run.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;

use crate::discover::{self, PathCommand, Tool};
use crate::exec::{self, Outcome};
use crate::inspect::{self, Kind};
use crate::pathenv::{self, PathIndex};
use crate::platform::{self, path_key};
use crate::report::{EntryIssue, Report, ToolResult, Verdict};
use crate::{pool, progress};

/// Installers and uninstallers can act on `--help`, so they are never run.
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
    /// Don't run --help for this tool (repeatable)
    #[arg(long, value_name = "NAME")]
    skip: Vec<String>,
    /// Number of tools checked in parallel [default: one per CPU core]
    #[arg(short, long)]
    jobs: Option<usize>,
    /// Seconds before a --help run is killed
    #[arg(long, default_value_t = 30, value_name = "SECS")]
    timeout: u64,
}

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
}

/// A tool after its static checks, with the decision whether to run it.
struct Prepared {
    res: ToolResult,
    skip_why: Option<&'static str>,
}

/// Static checks only: cheap, and decides whether `--help` is safe to run.
fn prepare(tool: Tool, ctx: &Ctx) -> Prepared {
    let insp = inspect::inspect(&tool.path, ctx.index, ctx.dirs);
    let res = ToolResult {
        name: tool.name,
        path: tool.path,
        kind: insp.kind,
        verdict: Verdict::Pass,
        reasons: insp.reasons,
        note: None,
        shadowed: tool.shadowed,
        attempts: Vec::new(),
        ms: 0,
    };

    let skip_why = if let Some(why) = platform::never_run(&res.path) {
        Some(why)
    } else if ctx.skip.contains(&res.name.to_lowercase()) {
        Some("not run: --skip")
    } else if matches!(res.kind, Kind::AppAlias { .. }) {
        Some("not run: app execution alias (starts a packaged app)")
    } else if INSTALLER_NAME.is_match(&res.name) {
        Some("not run: looks like an installer or uninstaller")
    } else if insp.gui {
        Some("no --help: GUI program")
    } else if matches!(res.kind, Kind::Script) {
        Some("no --help: script, not a launcher")
    } else if matches!(res.kind, Kind::Missing) {
        Some("not run: target is missing")
    } else {
        None
    };
    Prepared { res, skip_why }
}

/// Run `--help` (unless skipped) and settle the verdict.
fn run(p: Prepared, ctx: &Ctx) -> ToolResult {
    let mut res = p.res;
    let outcome = match p.skip_why {
        Some(why) => Outcome::Skip(why.to_string()),
        None => {
            let (outcome, attempts) = exec::run_help(&res.path, ctx.timeout);
            res.ms = attempts.iter().map(|a| a.ms).sum();
            res.attempts = attempts.into_iter().map(exec::trim_output).collect();
            outcome
        }
    };

    res.verdict = match outcome {
        Outcome::Broken(r) => {
            res.reasons.push(r);
            Verdict::Broken
        }
        Outcome::Skip(note) => {
            res.note = Some(note);
            if res.reasons.is_empty() { Verdict::Skip } else { Verdict::Broken }
        }
        // A static finding outranks a passing run: the path is still dead.
        _ if !res.reasons.is_empty() => Verdict::Broken,
        Outcome::ToolError(msg) => {
            res.note = Some(msg);
            Verdict::ToolError
        }
        Outcome::Pass => Verdict::Pass,
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
fn check_registry(args: &CheckArgs) -> bool {
    args.registry
}
#[cfg(unix)]
fn check_registry(_: &CheckArgs) -> bool {
    false
}

/// For each command, the copy that runs. OS copies only with --system, and
/// even then they are never run (`platform::never_run`).
fn tools_to_check(commands: &[PathCommand], include_system: bool) -> Vec<Tool> {
    commands
        .iter()
        .filter(|c| platform::is_runnable(&c.active().path) && (include_system || !c.active().os))
        .map(|c| Tool {
            name: c.name.clone(),
            path: c.active().path.clone(),
            shadowed: c.shadowed().iter().map(|s| s.path.clone()).collect(),
        })
        .collect()
}

fn entry_issues(entries: &[pathenv::PathEntry]) -> Vec<EntryIssue> {
    pathenv::check_entries(entries)
        .into_iter()
        .map(|(entry, problem)| EntryIssue { entry, problem })
        .collect()
}

pub fn check(args: &CheckArgs) -> ExitCode {
    platform::init();
    // Ctrl+C: stop every running tool and give the cursor back before exiting.
    let _ = ctrlc::set_handler(|| {
        platform::kill_children();
        platform::cursor_visible(true);
        std::process::exit(130);
    });
    let jobs = args.jobs.unwrap_or_else(|| pool::default_jobs(JOBS_PER_CORE, MAX_JOBS)).max(1);

    let entries = pathenv::process_path();
    if entries.is_empty() {
        eprintln!("pathsok: PATH is empty");
        return ExitCode::from(2);
    }

    let mut path_problems = entry_issues(&entries);

    // Entries saved in the registry that this process never received, e.g. a
    // terminal started before PATH was changed.
    let session: HashSet<String> = entries.iter().map(|e| path_key(&e.dir)).collect();
    let mut reported = HashSet::new();
    for e in platform::persisted_path() {
        let key = path_key(&e.dir);
        if e.dir.is_dir() && !session.contains(&key) && reported.insert(key) {
            path_problems.push(EntryIssue { entry: e, problem: pathenv::EntryProblem::NotInSession });
        }
    }
    let mut app_paths = Vec::new();
    if check_registry(args) {
        // Report persisted problems the process PATH doesn't already show.
        let known: HashSet<(String, String)> = path_problems
            .iter()
            .map(|p| (path_key(&p.entry.dir), p.problem.to_string()))
            .collect();
        path_problems.extend(
            entry_issues(&platform::persisted_path())
                .into_iter()
                .filter(|p| !known.contains(&(path_key(&p.entry.dir), p.problem.to_string()))),
        );
        app_paths = platform::broken_app_paths();
    }

    let dirs: Vec<PathBuf> = entries.iter().map(|e| e.dir.clone()).collect();
    let index = PathIndex::build(&dirs);
    let tools = tools_to_check(&discover::inventory(&entries, &index, args.system), args.system);
    eprintln!("pathsok: checking {} tools...", tools.len());

    let ctx = Ctx {
        index: &index,
        dirs: &dirs,
        skip: args.skip.iter().map(|s| s.to_lowercase()).collect(),
        timeout: Duration::from_secs(args.timeout),
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
        let retry = Ctx { timeout: ctx.timeout.min(RETRY_TIMEOUT), ..ctx };
        eprintln!(
            "pathsok: re-checking {} timed-out tools (up to {}s)...",
            timed_out.len(),
            retry.timeout.as_secs()
        );
        let progress = progress::Progress::new(timed_out.len());
        let rechecked = pool::run_in_order(timed_out.clone(), jobs, |i| {
            let t = &tools[i];
            let tool = Tool { name: t.name.clone(), path: t.path.clone(), shadowed: t.shadowed.clone() };
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
    let report = Report { path_problems, app_paths, tools };
    if args.json {
        match serde_json::to_string_pretty(&report) {
            Ok(j) => println!("{j}"),
            Err(e) => {
                eprintln!("pathsok: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        print!("{}", report.to_text(args.verbose));
    }
    ExitCode::from(report.exit_code())
}
