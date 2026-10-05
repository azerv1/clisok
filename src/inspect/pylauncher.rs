//! Python entry-point launchers written by pip (distlib) and uv.
//!
//! Both embed a zip whose `__main__.py` does `from pkg.mod import main`.
//! pip appends `#!<python>\n` + the zip to a launcher stub; uv stores the zip
//! in a PE resource and puts the same shebang inside `__main__.py`.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use crate::exec;

const LOCAL_HEADER: &[u8] = b"PK\x03\x04";
const MAIN_PY: &[u8] = b"__main__.py";

#[derive(Debug, PartialEq)]
pub struct PyTrailer {
    pub python: PathBuf,
    pub module: String,
}

pub fn parse(data: &[u8], exe_dir: &Path) -> Option<PyTrailer> {
    let (at, main_py) = find_main_py(data)?;
    let shebang = shebang_before(data, at).or_else(|| {
        let first = main_py.lines().next()?;
        first.starts_with("#!").then(|| first.to_string())
    })?;
    Some(PyTrailer {
        python: interpreter(&shebang, exe_dir)?,
        module: entry_module(&main_py)?,
    })
}

fn u16_at(d: &[u8], i: usize) -> Option<usize> {
    Some(u16::from_le_bytes(d.get(i..i + 2)?.try_into().ok()?) as usize)
}

fn u32_at(d: &[u8], i: usize) -> Option<usize> {
    Some(u32::from_le_bytes(d.get(i..i + 4)?.try_into().ok()?) as usize)
}

/// Locate the zip local header for `__main__.py` and return its offset and contents.
fn find_main_py(data: &[u8]) -> Option<(usize, String)> {
    let mut from = 0;
    while let Some(off) = data[from..].windows(4).position(|w| w == LOCAL_HEADER) {
        let at = from + off;
        from = at + 4;
        let name_len = u16_at(data, at + 26)?;
        if data.get(at + 30..at + 30 + name_len) != Some(MAIN_PY) {
            continue;
        }
        let method = u16_at(data, at + 8)?;
        let size = u32_at(data, at + 18)?;
        let start = at + 30 + name_len + u16_at(data, at + 28)?;
        let raw = data.get(start..start + size)?;
        let bytes = match method {
            0 => raw.to_vec(),
            8 => {
                let mut out = Vec::new();
                flate2::read::DeflateDecoder::new(raw).read_to_end(&mut out).ok()?;
                out
            }
            _ => return None,
        };
        return Some((at, String::from_utf8_lossy(&bytes).into_owned()));
    }
    None
}

/// pip's `#!<python>\n` line sitting directly in front of the zip.
fn shebang_before(data: &[u8], zip_at: usize) -> Option<String> {
    let head = &data[zip_at.saturating_sub(1024)..zip_at];
    let body = head.strip_suffix(b"\n")?;
    let body = body.strip_suffix(b"\r").unwrap_or(body);
    let start = body.windows(2).rposition(|w| w == b"#!")?;
    let line = &body[start..];
    (!line.contains(&b'\n')).then(|| String::from_utf8_lossy(line).into_owned())
}

fn interpreter(shebang: &str, exe_dir: &Path) -> Option<PathBuf> {
    let s = shebang.strip_prefix("#!")?.trim();
    let exe = match s.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => s,
    };
    if exe.starts_with('/') {
        return None; // `/bin/sh` style launchers: nothing Windows-specific to check
    }
    let p = PathBuf::from(exe);
    Some(if p.is_absolute() { p } else { exe_dir.join(p) })
}

/// Top-level package from the first `from X.y import z` line.
fn entry_module(main_py: &str) -> Option<String> {
    main_py.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("from ")?;
        let module = rest.split_whitespace().next()?.split('.').next()?;
        (module != "__future__" && !module.is_empty()).then(|| module.to_string())
    })
}

fn site_packages(python: &Path) -> Option<PathBuf> {
    let mut prefix = python.parent()?;
    if prefix.file_name().is_some_and(|n| n.eq_ignore_ascii_case("scripts")) {
        prefix = prefix.parent()?; // venv layout: <venv>\Scripts\python.exe
    }
    Some(prefix.join(r"Lib\site-packages"))
}

fn found_statically(sp: &Path, module: &str) -> bool {
    if sp.join(module).is_dir() || sp.join(format!("{module}.py")).is_file() {
        return true;
    }
    let prefix = format!("{}.", module.to_lowercase());
    fs::read_dir(sp).is_ok_and(|rd| {
        rd.flatten().any(|e| {
            let n = e.file_name().to_string_lossy().to_lowercase();
            n.starts_with(&prefix) && n.ends_with(".pyd")
        })
    })
}

type ProbeCache = HashMap<(PathBuf, String), Option<bool>>;

static PROBE_CACHE: LazyLock<Mutex<ProbeCache>> =
    LazyLock::new(Default::default);

/// Some(false) when the module is definitely missing; None when we can't tell.
pub fn module_found(python: &Path, module: &str) -> Option<bool> {
    if site_packages(python).is_some_and(|sp| found_statically(&sp, module)) {
        return Some(true);
    }
    let key = (python.to_path_buf(), module.to_string());
    if let Some(hit) = PROBE_CACHE.lock().unwrap().get(&key) {
        return *hit;
    }
    // Editable installs and user site-packages: let the interpreter decide.
    // find_spec on a top-level name locates the package without running it.
    let probe = "import importlib.util,sys;sys.exit(0 if importlib.util.find_spec(sys.argv[1]) else 3)";
    let result = exec::run_capture(python, &["-c", probe, module], Duration::from_secs(30))
        .ok()
        .and_then(|a| match a.exit {
            Some(0) => Some(true),
            Some(3) => Some(false),
            _ => None,
        });
    PROBE_CACHE.lock().unwrap().insert(key, result);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored_zip(name: &[u8], content: &[u8]) -> Vec<u8> {
        let mut z = LOCAL_HEADER.to_vec();
        z.extend([20, 0, 0, 0, 0, 0]); // version, flags, method=stored
        z.extend([0; 8]); // time, date, crc
        z.extend((content.len() as u32).to_le_bytes());
        z.extend((content.len() as u32).to_le_bytes());
        z.extend((name.len() as u16).to_le_bytes());
        z.extend(0u16.to_le_bytes());
        z.extend(name);
        z.extend(content);
        z
    }

    const MAIN: &str = "# -*- coding: utf-8 -*-\nimport re\nimport sys\nfrom somepkg.cli import main\nif __name__ == '__main__':\n    sys.exit(main())\n";

    #[test]
    fn parses_pip_launcher() {
        let mut exe = b"MZ stub\0\0".to_vec();
        exe.extend(b"#!C:\\Python312\\python.exe\n");
        exe.extend(stored_zip(MAIN_PY, MAIN.as_bytes()));
        let t = parse(&exe, Path::new(r"C:\Python312\Scripts")).unwrap();
        assert_eq!(t.python, PathBuf::from(r"C:\Python312\python.exe"));
        assert_eq!(t.module, "somepkg");
    }

    #[test]
    fn parses_quoted_shebang() {
        let mut exe = b"MZ".to_vec();
        exe.extend(b"#!\"C:\\Program Files\\Python\\python.exe\"\r\n");
        exe.extend(stored_zip(MAIN_PY, MAIN.as_bytes()));
        let t = parse(&exe, Path::new(r"C:\x")).unwrap();
        assert_eq!(t.python, PathBuf::from(r"C:\Program Files\Python\python.exe"));
    }

    #[test]
    fn parses_uv_launcher() {
        let main = format!("#!C:\\uv\\tools\\sometool\\Scripts\\python.exe\n{MAIN}");
        let mut exe = b"MZ ... UV_PYTHON_PATH ... PA".to_vec();
        exe.extend(stored_zip(MAIN_PY, main.as_bytes()));
        exe.extend(b"PK\x01\x02 central dir");
        let t = parse(&exe, Path::new(r"C:\x")).unwrap();
        assert_eq!(t.python, PathBuf::from(r"C:\uv\tools\sometool\Scripts\python.exe"));
        assert_eq!(t.module, "somepkg");
    }

    #[test]
    fn ignores_other_zips_and_plain_exes() {
        let mut exe = b"MZ".to_vec();
        exe.extend(stored_zip(b"data.txt", b"hello"));
        assert_eq!(parse(&exe, Path::new("C:\\")), None);
        assert_eq!(parse(b"MZ plain program", Path::new("C:\\")), None);
    }

    #[test]
    fn site_packages_layouts() {
        assert_eq!(
            site_packages(Path::new(r"C:\Python312\python.exe")),
            Some(PathBuf::from(r"C:\Python312\Lib\site-packages"))
        );
        assert_eq!(
            site_packages(Path::new(r"C:\uv\tools\x\Scripts\python.exe")),
            Some(PathBuf::from(r"C:\uv\tools\x\Lib\site-packages"))
        );
    }
}
