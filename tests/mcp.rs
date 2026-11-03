//! `clisok mcp` over stdio, against a disposable PATH. Nothing is fixed.
#![cfg(feature = "mcp")]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    _root: tempfile::TempDir,
}

impl Session {
    /// A server whose PATH holds one native tool, `native-probe`.
    fn start(flags: &[&str]) -> Self {
        let root = tempfile::tempdir().unwrap();
        let commands = root.path().join("commands");
        fs::create_dir(&commands).unwrap();
        let native = if cfg!(windows) {
            "native-probe.exe"
        } else {
            "native-probe"
        };
        fs::copy(env!("CARGO_BIN_EXE_clisok"), commands.join(native)).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_clisok"))
            .arg("mcp")
            // CI may run as root; the refusal itself is covered by tests/cli.rs.
            .arg("--allow-root")
            .args(flags)
            .env("PATH", &commands)
            .current_dir(root.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut session = Self {
            child,
            stdin,
            stdout,
            next_id: 0,
            _root: root,
        };
        let init = session.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "clisok-tests", "version": "0" },
            }),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "clisok", "{init}");
        session.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        session
    }

    fn send(&mut self, message: &Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// Every stdout line must be a JSON-RPC message; the reply must match the id.
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let mut line = String::new();
        assert!(
            self.stdout.read_line(&mut line).unwrap() > 0,
            "server closed stdout"
        );
        let reply: Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("stdout is not JSON-RPC: {e}: {line}"));
        assert_eq!(reply["jsonrpc"], "2.0", "{reply}");
        assert_eq!(reply["id"], id, "{reply}");
        reply
    }

    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        self.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        )
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn tool<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == name)
        .unwrap_or_else(|| panic!("{name} missing: {report}"))
}

#[test]
fn tools_are_listed_with_their_annotations() {
    let mut s = Session::start(&[]);
    let reply = s.request("tools/list", json!({}));
    let tools = reply["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names.len(), 4, "{names:?}");
    for name in ["check", "list", "rules", "fix"] {
        assert!(names.contains(&name), "{names:?}");
    }
    for t in tools {
        let read_only = t["name"] != "fix";
        assert_eq!(t["annotations"]["readOnlyHint"] == true, read_only, "{t}");
        assert_eq!(
            t["annotations"]["destructiveHint"] == true,
            !read_only,
            "{t}"
        );
    }
}

#[test]
fn checks_can_run_repeatedly_in_one_server() {
    let mut s = Session::start(&[]);
    for mode in ["static", "probe", "probe"] {
        let reply = s.call("check", json!({ "mode": mode, "detail": "findings" }));
        let report = &reply["result"]["structuredContent"];
        assert_ne!(reply["result"]["isError"], true, "{reply}");
        assert_eq!(report["command"], "check", "{reply}");
        let entry = tool(report, "native-probe");
        let findings: Vec<&Value> = report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["tool"] == "native-probe")
            .collect();
        let expected = if mode == "static" { "skip" } else { "pass" };
        assert_eq!(entry["verdict"], expected, "{mode}: {entry} {findings:?}");
    }
}

#[test]
fn summary_is_the_default_detail() {
    let mut s = Session::start(&[]);
    let report = s.call("check", json!({}))["result"]["structuredContent"].clone();
    assert!(report["summary"]["tools"].is_u64(), "{report}");
    assert!(report["statistics"].is_array(), "{report}");
    assert!(report["exit_code"].is_u64(), "{report}");
    assert!(report.get("tools").is_none(), "{report}");
}

#[test]
fn list_and_rules_round_trip() {
    let mut s = Session::start(&[]);
    let list = s.call("list", json!({}))["result"]["structuredContent"].clone();
    assert_eq!(list["command"], "list", "{list}");
    assert!(
        list["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "native-probe"),
        "{list}"
    );

    let all = s.call("rules", json!({}))["result"]["structuredContent"].clone();
    assert!(all["rules"].as_array().unwrap().len() > 1, "{all}");
    let one = s.call("rules", json!({ "code": "cli002" }))["result"]["structuredContent"].clone();
    assert_eq!(one["rules"][0]["code"], "CLI002", "{one}");
}

#[test]
fn unknown_codes_are_invalid_params() {
    let mut s = Session::start(&[]);
    for (tool, arguments) in [
        ("rules", json!({ "code": "CLI999" })),
        ("check", json!({ "ignore": ["CLI999"] })),
    ] {
        let reply = s.call(tool, arguments);
        assert_eq!(reply["error"]["code"], -32602, "{reply}");
    }
}

#[test]
fn applying_fixes_needs_the_server_flags() {
    let mut s = Session::start(&[]);
    let reply = s.call("fix", json!({ "apply": true }));
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    assert!(reply.to_string().contains("--allow-fix"), "{reply}");

    let mut s = Session::start(&["--allow-fix"]);
    let reply = s.call("fix", json!({ "apply": true, "unsafe_fixes": true }));
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    assert!(
        reply.to_string().contains("--allow-unsafe-fixes"),
        "{reply}"
    );
}

#[test]
fn fix_previews_by_default() {
    let mut s = Session::start(&[]);
    let reply = s.call("fix", json!({}));
    let report = &reply["result"]["structuredContent"];
    assert_eq!(report["applied"], false, "{reply}");
    assert!(report["diff"].is_string(), "{reply}");
}
