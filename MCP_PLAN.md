# Plan: expose clisok as an MCP server

## Goal and shape

Add a stdio MCP server as `clisok mcp`, built from the existing Rust code behind an
optional `mcp` Cargo feature. No separate wrapper process and no duplicated scan
logic.

```
client  <-- JSON-RPC/stdio -->  clisok mcp  -->  run_check()/run_fix()/run_list()/rules()
```

The server lives in the binary crate as `src/mcp.rs`, next to `check`, `list` and
`rule`. `src/lib.rs` stays what it is today: pure parsing boundaries
(`pub mod parsing`, used by `benches/parsing.rs`). No library split.

Default `cargo build` stays dependency-free and byte-for-byte the same CLI.
`cargo build --features mcp` adds the server.

## Why this is cheap

- `output::check_json()` (`src/output.rs:193`) and `output::statistics_json()`
  (`src/output.rs:254`) already return the JSON a tool should emit.
- `Report`, `ListReport`, `Rule` are serde-serializable (`src/report.rs:227`,
  `src/report.rs:487`, `src/rule.rs:61`).
- `Report::exit_code()` preserves CLI semantics.
- Probes cannot read or write the transport: child stdin is `NUL` / `/dev/null`
  and stdout is captured (`src/platform/windows/spawn.rs:383-396`,
  `src/platform/unix/spawn.rs:187`). The only stdout writers are the CLI
  adapters via `print_stdout`.
- The progress renderer already disables itself when stderr is not a terminal
  (`progress.rs`, `Terminal::new`), which is the normal case for an MCP server.
- The real work is removing print/exit/global-state from the core path.

## Phase 1 - Re-entrant core (no CLI behavior change)

Option structs, so the MCP layer never builds clap types:

- `CheckOptions`, `FixOptions`, `ListOptions` with `pub(crate)` fields.
  `registry` stays `#[cfg(windows)]`, as in `CheckArgs`. Probe vs. static is the
  existing `static_only` (`--static`, `src/check.rs:62`).
- `CheckArgs::to_options()`, `ListArgs::to_options()`.

Pure functions extracted from `check::check` (`src/check.rs:572`):

- `run_check(&CheckOptions) -> Result<Report, String>`: privilege policy,
  `platform::init()`, PATH findings, discovery, prepare/run, timeout retry,
  `platform::cleanup()`. No `print_stdout`, no `ExitCode`, no `ctrlc`.
- `run_fix(&FixOptions) -> Result<FixOutcome, String>`. Needs no tool scan:
  `--diff` already returns before discovery, and after `--fix` only the
  saved-settings findings are recomputed (`src/check.rs:621-635`).
  - dry run: the diff text (`print_diff` logic, returned instead of printed).
  - apply: `applied: Vec<Applied>`, `errors: Vec<String>` (today `eprintln!`ed
    at `src/check.rs:537,550`), and the remaining PATH / App Paths findings.
- `run_list(&ListOptions) -> Result<ListReport, String>`.
- `rules(Option<Code>) -> Vec<Rule>`; `Rule` becomes `pub(crate)`.
- `check()` / `list()` / `rule()` stay as thin CLI adapters: install `ctrlc`
  (CLI only), format, print, map exit codes. Error texts and exit codes
  unchanged.

Global state that breaks a second run in one process:

- **Work directory** (`src/platform/mod.rs:28-55`): `WORK_DIR` is a `OnceLock`
  but `cleanup()` deletes the directory, so run #2 probes from a dead cwd.
  Change to `Mutex<Option<TempDir>>`: `work_dir()` returns `PathBuf` and creates
  a fresh uniquely named dir when empty; `cleanup()` takes and drops it. Each run
  gets its own dir; no shutdown hook needed. Callers:
  `src/platform/windows/spawn.rs:364`, `src/platform/unix/spawn.rs:186`, test at
  `src/platform/unix/spawn.rs:376`.
- **Python probe cache** (`src/inspect/python.rs:69`): `PROBE_CACHE` lives for
  the process, so a long-lived server reports stale module results after a
  `pip install`. Clear it at the start of each `run_check`.
- **Ctrl+C** (`src/check.rs:585-590`): move `ctrlc::set_handler` and
  `process::exit(130)` into the CLI adapter.
- **Interrupt flags**: reset `progress::INTERRUPTED` at the start of each run.
  Unix `STOPPING` (`src/platform/unix/spawn.rs:33`) is only set by
  `kill_children()`, which v1 never calls in the server; leave it.

Tests: two consecutive `run_check` calls (static, then probe) in one process both
succeed; the work dir exists during a run and is gone after it.

## Phase 2 - MCP server

Dependencies, behind the feature:

```toml
[features]
default = []
mcp = ["dep:rmcp", "dep:tokio"]

[dependencies]
rmcp  = { version = "=3.8.0", default-features = false, features = ["server", "macros", "transport-io"], optional = true }
tokio = { version = "1", features = ["rt", "sync"], optional = true }
```

- `rmcp 3.8.0` declares rust-version 1.88 (checked with `cargo info rmcp`), the
  repo MSRV. Pin it exactly, like `criterion`, so a later 3.x cannot raise it.
- rmcp's `server` feature already pulls in schemars; derive `JsonSchema` through
  the `rmcp::schemars` re-export, not a direct dependency.
- Build a current-thread tokio runtime by hand in `mcp::serve()`.

CLI surface: `Command::Mcp(McpArgs)` behind `#[cfg(feature = "mcp")]`, with
server-side safety flags that the model cannot flip:

- `--allow-fix`: the `fix` tool may apply changes.
- `--allow-unsafe-fixes` (requires `--allow-fix`): `unsafe_fixes=true` is honored.
- `--allow-root`: probes may run as root / elevated.

`ClisokMcp` uses `#[tool_router]` + `#[tool]` + `Parameters<T>`, served with
`ServiceExt::serve(stdio())`. Every `run_*` call runs in `spawn_blocking` under
one `std::sync::Mutex`, because `platform` and `progress` state are global;
concurrent calls queue.

Tool surface:

| Tool | Params | Returns | Annotation |
|---|---|---|---|
| `check` | `mode: "static"\|"probe"` (default `static`), `system`, `registry` (Windows), `skip[]`, `ignore[]`, `timeout_secs` (default 10), `jobs`, `detail: "summary"\|"findings"\|"full"` (default `summary`) | `structuredContent` = detail payload + `exit_code`; one-line text summary | `readOnlyHint` |
| `list` | `system` | `ListReport` JSON | `readOnlyHint` |
| `rules` | `code?`; omitted = full catalog | rule object or array | `readOnlyHint` |
| `fix` | `apply` (default `false`), `unsafe_fixes` (default `false`) | diff, or `applied` + `errors` + remaining findings | `destructiveHint` |

`detail` maps onto existing functions, so there is no separate `verbose` param:
`summary` -> `statistics_json`, `findings` -> `check_json(_, false)`,
`full` -> `check_json(_, true)`.

Rules:

- Findings are a **successful** call, not `isError`. Runtime failures (PATH
  empty, privilege refusal, I/O) are `CallToolResult::error`; an unknown code or
  enum value is `McpError::invalid_params`.
- `fix` with `apply=true` without `--allow-fix`, or `unsafe_fixes=true` without
  `--allow-unsafe-fixes`, is a tool error naming the missing server flag.
- `probe` on an elevated server without `--allow-root` is refused, as in the CLI.
- Backups stay under `%LOCALAPPDATA%\clisok\backups\`.
- `get_info()` advertises name `clisok`, the crate version, and instructions:
  `probe` executes tools from PATH; the server checks the PATH it was launched
  with, which may differ from the user's shell (`registry` on Windows also covers
  the saved PATH).

## Phase 3 - Tests, CI, docs

- `tests/mcp.rs` (`#![cfg(feature = "mcp")]`): spawn `CARGO_BIN_EXE_clisok mcp`
  and speak JSON-RPC:
  - `initialize` -> `tools/list` -> `check {mode:"static"}` twice (work-dir
    regression) -> `list` -> `rules`.
  - `rules {code:"CLI999"}` -> invalid params.
  - `fix {apply:true}` without `--allow-fix` -> gate error.
  - Every stdout line parses as JSON-RPC (stdout purity).
- CI (`ci.yml`): add `cargo clippy --all-targets --features mcp -- -D warnings`
  and `cargo test --locked --features mcp` to the native matrix, including the
  existing 1.88.0 rows (MSRV check for rmcp/tokio).
- Release: build published binaries with `--features mcp`; check
  `tests/release/test_artifacts.py` and `reproducible.yml` still hold.
- README: "Use as an MCP server" with a client config snippet, the three server
  flags, and the PATH caveat.
- CHANGELOG entry; no `SCHEMA_VERSION` bump (payload unchanged).

## Estimates

| Phase | Work | Estimate |
|---|---|---|
| 1 | Options, `run_*` extraction, globals, regression tests | 1-1.5 days |
| 2 | rmcp server, 4 tools, flags, feature plumbing | 1-1.5 days |
| 3 | MCP integration tests, CI, release, docs | 0.5-1 day |
| | **Total** | **2.5-4 days** |

## Risks

- **Long scans**: a probe run can take minutes and clients time out. Mitigated by
  the `static` default and a 10 s MCP `timeout_secs` default (CLI stays 30 s).
- **No cancellation in v1**: an in-flight check cannot be aborted. Later: map
  `notifications/cancelled` to `platform::kill_children()`, then reset
  `progress::INTERRUPTED` and Unix `STOPPING`.
- **Killed mid-run**: a server killed during a probe can leave one work dir
  behind, same as the CLI under SIGKILL today.
- **PATH mismatch**: the server sees its launch PATH, not the user's shell.
  Documented in `get_info` and the README.
- **`fix` mutates the host**: dry-run by default; applying needs `--allow-fix`
  in the client config, unsafe fixes need `--allow-unsafe-fixes`.
- **Payload size**: default `detail:"summary"`; `full` can be large.

## Decisions

1. Route: native `clisok mcp` in the binary crate.
2. Default check mode: `static`.
3. `fix` tool: included, dry-run by default, apply gated by server flags.
4. Release binaries: built with `--features mcp`; plain `cargo build` stays lean.
5. Transport: stdio only.
