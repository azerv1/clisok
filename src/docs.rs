//! Docs pages generated from the source, so they can't drift: the codes page
//! from `codes::CODES`, the config key table from `config::KEYS`. A test
//! fails when a committed page is out of date; `UPDATE_DOCS=1 cargo test`
//! rewrites it.

use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use crate::codes::{CODES, Severity};
use crate::config;

const NOTICE: &str = "<!-- Generated from the source by `UPDATE_DOCS=1 cargo test`. Don't edit by hand. -->";

fn group(number: u16) -> &'static str {
    match number / 100 {
        0 => "BOK0xx — PATH entries",
        1 => "BOK1xx — tool checks",
        2 => "BOK2xx — the --help run",
        _ => "BOK3xx — Windows registry",
    }
}

fn codes_page() -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# Error codes\n\n{NOTICE}\n");
    s.push_str(
        "Every finding has a code. A code keeps its meaning forever and is never reused, \
         so scripts, ignore lists and config files can rely on it. The first digit names the group.\n\n\
         Change what a code does with `--ignore CODE` or a [`[severity]`](configuration.md) entry.\n\n\
         | Code | Default | Meaning |\n|---|---|---|\n",
    );
    for c in CODES {
        let _ = writeln!(s, "| [{}](#{}) | {} | {} |", c.code, c.code.to_string().to_lowercase(), default(c), c.title);
    }
    let mut last = "";
    for c in CODES {
        let g = group(c.number);
        if g != last {
            let _ = writeln!(s, "\n## {g}");
            last = g;
        }
        let id = c.code.to_string().to_lowercase();
        let _ = writeln!(s, "\n### {} — {} {{#{id}}}\n\n**Default:** {}\n\n{}", c.code, c.title, default_long(c), c.help);
    }
    s
}

fn default(c: &crate::codes::CodeInfo) -> &'static str {
    match (c.tool_error, c.default) {
        (true, _) => "tool error",
        (_, Severity::Error) => "error",
        (_, Severity::Warn) => "warning",
        (_, Severity::Off) => "off",
    }
}

fn default_long(c: &crate::codes::CodeInfo) -> &'static str {
    match (c.tool_error, c.default) {
        (true, _) => "tool error, shown as `[ERROR]`; doesn't fail the run",
        (_, Severity::Error) => "error; fails the run",
        (_, Severity::Warn) => "warning; doesn't fail the run",
        (_, Severity::Off) => "off",
    }
}

fn config_keys_page() -> String {
    let mut s = format!("{NOTICE}\n\n| Key | Value | Meaning |\n|---|---|---|\n");
    for (key, value, meaning) in config::KEYS {
        let _ = writeln!(s, "| `{key}` | {value} | {meaning} |");
    }
    s
}

fn check_page(rel: &str, want: String) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs").join("src").join(rel);
    if std::env::var_os("UPDATE_DOCS").is_some() {
        fs::write(&path, &want).unwrap();
        return;
    }
    let have = fs::read_to_string(&path).unwrap_or_default().replace("\r\n", "\n");
    assert!(have == want, "{} is out of date; run `UPDATE_DOCS=1 cargo test`", path.display());
}

#[test]
fn docs_are_current() {
    check_page("codes.md", codes_page());
    check_page("config-keys.md", config_keys_page());
}
