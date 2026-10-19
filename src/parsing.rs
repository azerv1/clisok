//! Pure parsing shared by production, fuzz targets, Miri and benchmarks.

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

/// Top-level package from the first `from X.y import z` line.
pub fn entry_module(main_py: &str) -> Option<String> {
    main_py.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("from ")?;
        let module = rest.split_whitespace().next()?.split('.').next()?;
        (module != "__future__" && !module.is_empty()).then(|| module.to_string())
    })
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
    fn entry_point_ignores_future_imports_and_returns_only_a_top_level_name() {
        assert_eq!(
            entry_module("from __future__ import annotations\nfrom pkg.cli import main"),
            Some("pkg".into())
        );
        assert_eq!(entry_module("print('not an import')"), None);
    }
}
