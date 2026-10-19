//! Docs pages generated from the source, so they can't drift: the codes page
//! from `codes::CODES`, the config key table from `config::KEYS`. A test
//! fails when a committed page is out of date; `UPDATE_DOCS=1 cargo test`
//! rewrites it.

use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use crate::codes::CODES;
use crate::config;
use crate::fix::{self, Applicability};

const NOTICE: &str =
    "<!-- Generated from the source by `UPDATE_DOCS=1 cargo test`. Don't edit by hand. -->";

fn group(number: u16) -> &'static str {
    match number / 100 {
        0 => "CLI0xx — PATH entries",
        1 => "CLI1xx — tool checks",
        2 => "CLI2xx — the test run",
        _ => "CLI3xx — Windows registry",
    }
}

fn codes_page() -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# Error codes\n\n{NOTICE}\n");
    s.push_str(
        "Every finding has a code. A code keeps its meaning forever and is never reused, \
         so scripts, ignore lists and config files can rely on it. The first digit names the group.\n\n\
         Change what a code does with `--ignore CODE` or a [`[severity]`](configuration.md) entry. \
         The Fix column says what [`--fix`](fixes.md) can do about it. \
         `clisok rule CODE` prints a code's entry in the terminal.\n\n\
         | Code | Default | Fix | Meaning |\n|---|---|---|---|\n",
    );
    for c in CODES {
        let (code, id, fix) = (c.code, c.code.to_string().to_lowercase(), fix_short(c.code));
        let _ = writeln!(
            s,
            "| [{code}](#{id}) | {} | {fix} | {} |",
            c.default_name(),
            c.title
        );
    }
    let mut last = "";
    for c in CODES {
        let g = group(c.number);
        if g != last {
            let _ = writeln!(s, "\n## {g}");
            last = g;
        }
        let id = c.code.to_string().to_lowercase();
        let _ = writeln!(
            s,
            "\n### {} — {} {{#{id}}}\n\n**Default:** {}\n\n{}",
            c.code,
            c.title,
            c.default_long(),
            c.help
        );
        let _ = writeln!(s, "\n**Fix:** {}", fix::safety(c.code).1);
    }
    s
}

fn fix_short(code: crate::codes::Code) -> &'static str {
    match fix::safety(code).0 {
        Some(Applicability::Safe) => "safe",
        Some(Applicability::Unsafe) => "unsafe",
        Some(Applicability::DisplayOnly) => "advice",
        None => "—",
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
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("src")
        .join(rel);
    if std::env::var_os("UPDATE_DOCS").is_some() {
        fs::write(&path, &want).unwrap();
        return;
    }
    let have = fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert!(
        have == want,
        "{} is out of date; run `UPDATE_DOCS=1 cargo test`",
        path.display()
    );
}

#[test]
fn docs_are_current() {
    check_page("codes.md", codes_page());
    check_page("config-keys.md", config_keys_page());
}
