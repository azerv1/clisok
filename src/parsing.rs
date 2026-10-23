//! Pure parsing shared by production, fuzz targets, Miri and benchmarks.

use std::sync::LazyLock;

use regex::Regex;

/// Expand configuration variables using a caller-supplied lookup, without executing code.
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

static ENTRY_IMPORT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^from ([A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*) import ([A-Za-z_][A-Za-z0-9_]*)$")
        .unwrap()
});
static CODING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"coding[:=]\s*([-A-Za-z0-9_.]+)").unwrap());

/// Top-level package from a generated pip/distlib or uv console entry point.
/// Unknown Python bodies remain scripts; an import alone is not a launcher.
pub fn entry_module(main_py: &str) -> Option<String> {
    // Encodings such as unicode_escape can turn a comment's escaped newlines
    // into executable source before Python parses it.
    for line in main_py.lines().take(2) {
        if line.trim_start().starts_with('#')
            && let Some(coding) = CODING.captures(line)
            && !matches!(&coding[1], "utf-8" | "utf8" | "UTF-8" | "ascii")
        {
            return None;
        }
    }
    let lines: Vec<String> = main_py
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .map(|l| l.replace('"', "'"))
        .collect();
    let (mut sys, mut re, mut at) = (false, false, 0);
    while let Some(line) = lines.get(at) {
        match line.as_str() {
            "import sys" if !sys => sys = true,
            "import re" if !re => re = true,
            _ => break,
        }
        at += 1;
    }
    if !sys {
        return None;
    }
    let import = ENTRY_IMPORT.captures(lines.get(at)?)?;
    let module = import[1].split('.').next()?;
    if module == "__future__" {
        return None;
    }
    let call = format!("sys.exit({}())", &import[2]);
    at += 1;
    let body: Vec<&str> = if lines.get(at)?.as_str() == "if __name__ == '__main__':" {
        lines[at + 1..]
            .iter()
            .map(|line| line.strip_prefix("    "))
            .collect::<Option<_>>()?
    } else {
        lines[at..].iter().map(String::as_str).collect()
    };
    let (exit, setup) = body.split_last()?;
    if *exit != call {
        return None;
    }
    let safe_setup = match setup {
        [] => true,
        [rewrite] if re => [
            r"sys.argv[0] = re.sub(r'(-script\.pyw?|\.exe)?$', '', sys.argv[0])",
            r"sys.argv[0] = re.sub(r'(-script\.pyw|\.exe)?$', '', sys.argv[0])",
        ]
        .contains(rewrite),
        [
            "if sys.argv[0].endswith('.exe'):",
            "    sys.argv[0] = sys.argv[0][:-4]",
        ] => true,
        [
            "if sys.argv[0].endswith('-script.pyw'):",
            "    sys.argv[0] = sys.argv[0][:-11]",
            "elif sys.argv[0].endswith('.exe'):",
            "    sys.argv[0] = sys.argv[0][:-4]",
        ] => true,
        _ => false,
    };
    safe_setup.then(|| module.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expansion_preserves_literal_dollars_and_does_not_evaluate_commands() {
        assert_eq!(expand("$${HOME}", &|_| None).unwrap(), "${HOME}");
        assert!(expand("$(touch unexpected)", &|_| None).is_err());
        assert_eq!(expand("${MISSING:-value}", &|_| None).unwrap(), "value");
    }

    #[test]
    fn entry_point_requires_a_complete_body_and_returns_a_top_level_name() {
        assert_eq!(
            entry_module("import sys\nfrom pkg.cli import main\nsys.exit(main())\n"),
            Some("pkg".into())
        );
        assert_eq!(
            entry_module("from __future__ import annotations\nfrom pkg.cli import main"),
            None
        );
        assert_eq!(entry_module("print('not an import')"), None);
    }

    #[test]
    fn python_import_does_not_make_an_action_script_a_launcher() {
        let script =
            "#!/usr/bin/python3\nfrom pathlib import Path\nPath('marker').write_text('ran')\n";
        assert_eq!(entry_module(script), None);
        let disguised = "import sys\nfrom pkg.cli import main\nif __name__ == '__main__':\n    open('marker', 'w').close()\n    sys.exit(main())\n";
        assert_eq!(entry_module(disguised), None);
        let encoded = "# coding: unicode_escape\n# \\x0aopen('marker', 'w').close()\nimport sys\nfrom pkg import main\nsys.exit(main())\n";
        assert_eq!(entry_module(encoded), None);
    }

    #[test]
    fn recognizes_generated_pip_and_uv_entry_points() {
        let pip = "#!/usr/bin/python3\n# -*- coding: utf-8 -*-\nimport re\nimport sys\nfrom somepkg.cli import main\nif __name__ == '__main__':\n    sys.argv[0] = re.sub(r'(-script\\.pyw?|\\.exe)?$', '', sys.argv[0])\n    sys.exit(main())\n";
        let uv = "#!C:\\venv\\Scripts\\python.exe\nimport sys\nfrom mypy.__main__ import console_entry\nif __name__ == \"__main__\":\n    if sys.argv[0].endswith(\"-script.pyw\"):\n        sys.argv[0] = sys.argv[0][:-11]\n    elif sys.argv[0].endswith(\".exe\"):\n        sys.argv[0] = sys.argv[0][:-4]\n    sys.exit(console_entry())\n";
        assert_eq!(entry_module(pip), Some("somepkg".into()));
        assert_eq!(entry_module(uv), Some("mypy".into()));
        assert_eq!(
            entry_module(&uv.replace("[:-4]", "[open('marker', 'w'):]")),
            None
        );
        assert_eq!(
            entry_module(&pip.replace("main())", "main('action'))")),
            None
        );
    }
}
