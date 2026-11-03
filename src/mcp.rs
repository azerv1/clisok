//! `clisok mcp`: the checks as Model Context Protocol tools over stdio.
//!
//! stdout carries JSON-RPC only; progress and notes go to stderr. Checks share
//! process-wide state (`platform`, `progress`), so they run one at a time.

use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt, schemars, tool, tool_handler, tool_router,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::check::{self, CheckOptions, FixOptions};
use crate::codes::Code;
use crate::report::Report;
use crate::{list, output, platform, rule};

/// Shorter than the CLI's 30 s: MCP clients time out whole calls.
const DEFAULT_TIMEOUT: u64 = 10;

const INSTRUCTIONS: &str = "clisok checks that the executables on PATH work. \
`check` with mode `static` only reads files; mode `probe` also runs each tool \
that is safe to run (`--version`, then `--help`), headless and with a timeout. \
The server checks the PATH it was started with, which can differ from the \
user's shell. `fix` previews changes to saved settings; applying them needs \
the server started with --allow-fix.";

#[derive(clap::Args)]
pub struct McpArgs {
    /// Let the `fix` tool change saved settings; without it, `fix` only previews
    #[arg(long)]
    allow_fix: bool,
    /// Let the `fix` tool apply unsafe fixes too
    #[arg(long, requires = "allow_fix")]
    allow_unsafe_fixes: bool,
    /// Allow probes to run as root or elevated
    #[arg(long)]
    allow_root: bool,
}

/// What the person who started the server allows; tool arguments can't change it.
#[derive(Clone, Copy)]
struct Gates {
    allow_fix: bool,
    allow_unsafe_fixes: bool,
    allow_root: bool,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum Mode {
    /// Inspect files only; nothing is run.
    #[default]
    Static,
    /// Also run each tool that is safe to run, headless and with a timeout.
    Probe,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum Detail {
    /// Totals, verdict counts and how many findings each code has.
    #[default]
    Summary,
    /// Every finding and every tool's verdict.
    Findings,
    /// Findings plus inspection details, run attempts and shadowed copies.
    Full,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CheckParams {
    /// `static` (default) reads files only; `probe` also runs tools.
    #[serde(default)]
    mode: Mode,
    /// Also check the OS's own folders (static checks only, never run).
    #[serde(default)]
    system: bool,
    /// Also check the saved (registry) PATH and App Paths registrations.
    #[cfg(windows)]
    #[serde(default)]
    registry: bool,
    /// Tool names not to run.
    #[serde(default)]
    skip: Vec<String>,
    /// Finding codes to drop, e.g. CLI007.
    #[serde(default)]
    ignore: Vec<String>,
    /// Seconds before a test run is killed (default 10).
    timeout_secs: Option<u64>,
    /// Tools checked in parallel (default: one per CPU core).
    jobs: Option<usize>,
    /// How much of the report to return (default `summary`).
    #[serde(default)]
    detail: Detail,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListParams {
    /// Also list commands found only in the OS's own folders.
    #[serde(default)]
    system: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RulesParams {
    /// One code, e.g. CLI002; omit for every rule.
    code: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct FixParams {
    /// Change saved settings (needs --allow-fix); otherwise return the diff only.
    #[serde(default)]
    apply: bool,
    /// Include unsafe fixes: cleaning quotes and spaces off user PATH entries,
    /// deleting user App Paths keys (applying needs --allow-unsafe-fixes).
    #[serde(default)]
    unsafe_fixes: bool,
    /// Finding codes to drop, so they are not fixed either.
    #[serde(default)]
    ignore: Vec<String>,
}

#[derive(Clone)]
struct Server {
    gates: Gates,
    /// Held for the whole of each check, list or fix.
    busy: Arc<Mutex<()>>,
    tool_router: ToolRouter<Self>,
}

fn codes(names: &[String]) -> Result<Vec<Code>, McpError> {
    names
        .iter()
        .map(|n| {
            n.parse()
                .map_err(|e: String| McpError::invalid_params(e, None))
        })
        .collect()
}

fn internal(e: impl std::fmt::Display) -> McpError {
    McpError::internal_error(e.to_string(), None)
}

fn tool_error(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message.into())])
}

fn check_payload(report: &Report, detail: Detail) -> Result<Value, McpError> {
    let full = output::check_json(report, detail == Detail::Full).map_err(internal)?;
    let mut value = match detail {
        Detail::Summary => json!({
            "schema": full["schema"],
            "command": full["command"],
            "summary": full["summary"],
            "statistics": report.statistics(),
        }),
        Detail::Findings | Detail::Full => full,
    };
    value["exit_code"] = json!(report.exit_code());
    Ok(value)
}

#[tool_router]
impl Server {
    fn new(gates: Gates) -> Self {
        Self {
            gates,
            busy: Arc::default(),
            tool_router: Self::tool_router(),
        }
    }

    /// Run `f` off the async runtime, one call at a time.
    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, McpError> {
        let busy = Arc::clone(&self.busy);
        tokio::task::spawn_blocking(move || {
            let _guard = busy.lock().unwrap_or_else(|e| e.into_inner());
            f()
        })
        .await
        .map_err(internal)
    }

    #[tool(
        description = "Check PATH entries and every tool on PATH: broken entries, missing \
            targets and interpreters, shadowed copies and, in probe mode, tools that fail \
            to start. Findings are a successful result; exit_code is 1 when any is an error.",
        annotations(read_only_hint = true)
    )]
    async fn check(
        &self,
        Parameters(p): Parameters<CheckParams>,
    ) -> Result<CallToolResult, McpError> {
        if p.timeout_secs == Some(0) {
            return Err(McpError::invalid_params(
                "timeout_secs must be at least 1",
                None,
            ));
        }
        let opts = CheckOptions {
            #[cfg(windows)]
            registry: p.registry,
            system: p.system,
            skip: p.skip,
            ignore: codes(&p.ignore)?,
            static_only: p.mode == Mode::Static,
            allow_root: self.gates.allow_root,
            jobs: p.jobs,
            timeout: Some(p.timeout_secs.unwrap_or(DEFAULT_TIMEOUT)),
        };
        let run = self
            .blocking(move || {
                check::probe_permission(&opts, platform::is_privileged)?;
                check::run_check(&opts)
            })
            .await?;
        match run {
            Ok(report) => Ok(CallToolResult::structured(check_payload(
                &report, p.detail,
            )?)),
            Err(e) => Ok(tool_error(e)),
        }
    }

    #[tool(
        description = "List every command on PATH and the copies it shadows. Nothing is run.",
        annotations(read_only_hint = true)
    )]
    async fn list(
        &self,
        Parameters(p): Parameters<ListParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.blocking(move || list::run_list(p.system)).await? {
            Ok(report) => Ok(CallToolResult::structured(
                serde_json::to_value(report).map_err(internal)?,
            )),
            Err(e) => Ok(tool_error(e)),
        }
    }

    #[tool(
        description = "Explain finding codes: title, default severity, fix safety and explanation.",
        annotations(read_only_hint = true)
    )]
    async fn rules(
        &self,
        Parameters(p): Parameters<RulesParams>,
    ) -> Result<CallToolResult, McpError> {
        let code = p
            .code
            .map(|c| c.parse::<Code>())
            .transpose()
            .map_err(|e| McpError::invalid_params(e, None))?;
        Ok(CallToolResult::structured(
            json!({ "rules": rule::rules(code) }),
        ))
    }

    #[tool(
        description = "Preview (default) or apply the fixes for saved PATH and App Paths \
            settings. Returns a diff, or what was changed plus the findings left. Applying \
            needs the server started with --allow-fix; unsafe fixes also need \
            --allow-unsafe-fixes. Changed values are backed up first.",
        annotations(destructive_hint = true)
    )]
    async fn fix(&self, Parameters(p): Parameters<FixParams>) -> Result<CallToolResult, McpError> {
        if p.apply && !self.gates.allow_fix {
            return Ok(tool_error(
                "applying fixes is turned off; start the server with --allow-fix",
            ));
        }
        if p.apply && p.unsafe_fixes && !self.gates.allow_unsafe_fixes {
            return Ok(tool_error(
                "applying unsafe fixes is turned off; start the server with --allow-unsafe-fixes",
            ));
        }
        let opts = FixOptions {
            ignore: codes(&p.ignore)?,
            unsafe_fixes: p.unsafe_fixes,
            apply: p.apply,
        };
        let outcome = match self.blocking(move || check::run_fix(&opts)).await? {
            Ok(outcome) => outcome,
            Err(e) => return Ok(tool_error(e)),
        };
        let mut value = output::check_json(&outcome.report, false).map_err(internal)?;
        value["applied"] = json!(p.apply);
        if !p.apply {
            value["diff"] = json!(outcome.diff);
        }
        if let Some(why) = outcome.nothing_to_fix {
            value["nothing_to_fix"] = json!(why);
        }
        if !outcome.errors.is_empty() {
            value["errors"] = json!(outcome.errors);
        }
        Ok(CallToolResult::structured(value))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("clisok", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}

pub fn mcp(args: &McpArgs) -> ExitCode {
    let gates = Gates {
        allow_fix: args.allow_fix,
        allow_unsafe_fixes: args.allow_unsafe_fixes,
        allow_root: args.allow_root,
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("clisok: {e}");
            return ExitCode::from(2);
        }
    };
    let served = runtime.block_on(async {
        let service = Server::new(gates)
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|e| e.to_string())?;
        service.waiting().await.map_err(|e| e.to_string())
    });
    // The client is gone: stop any tool still running and remove the work dir.
    platform::kill_children();
    runtime.shutdown_timeout(Duration::from_secs(1));
    platform::cleanup();
    match served {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("clisok: {e}");
            ExitCode::from(2)
        }
    }
}
