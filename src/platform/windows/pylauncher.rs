//! Python entry-point launchers written by pip (distlib) and uv.
//!
//! Both embed a zip whose `__main__.py` does `from pkg.mod import main`.
//! pip appends `#!<python>\n` + the zip to a launcher stub; uv stores the zip
//! in a PE resource and puts the same shebang inside `__main__.py`.

use std::path::{Path, PathBuf};

use object::pe::{ImageNtHeaders32, ImageNtHeaders64, RT_RCDATA};
use object::read::pe::{ImageNtHeaders, PeFile};
use object::{FileKind, LittleEndian as LE};

use crate::inspect::{MAX_SCRIPT_BYTES, python::entry_module, read_bounded};

const LOCAL_HEADER: &[u8] = b"PK\x03\x04";
const MAIN_PY: &[u8] = b"__main__.py";

#[derive(Debug, PartialEq)]
pub struct PyTrailer {
    pub python: PathBuf,
    pub module: String,
}

pub fn parse(data: &[u8], exe_dir: &Path) -> Result<Option<PyTrailer>, &'static str> {
    let Some((at, method, raw)) = find_main_py(data) else {
        return Ok(None);
    };
    let bytes = match method {
        0 => read_bounded(raw, MAX_SCRIPT_BYTES),
        8 => read_bounded(flate2::read::DeflateDecoder::new(raw), MAX_SCRIPT_BYTES),
        _ => return Err("unsupported launcher ZIP compression; static inspection skipped"),
    }
    .map_err(|_| "launcher text is invalid or exceeds 1 MiB; static inspection skipped")?;
    let main_py = String::from_utf8_lossy(&bytes);
    let module =
        entry_module(&main_py).ok_or("unrecognised Python entry point; static inspection only")?;
    let parse_entry = || {
        let shebang = shebang_before(data, at).or_else(|| {
            let first = main_py.lines().next()?;
            first.starts_with("#!").then(|| first.to_string())
        })?;
        Some(PyTrailer {
            python: interpreter(&shebang, exe_dir)?,
            module,
        })
    };
    Ok(parse_entry())
}

fn u16_at(d: &[u8], i: usize) -> Option<usize> {
    Some(u16::from_le_bytes(d.get(i..i + 2)?.try_into().ok()?) as usize)
}

fn u32_at(d: &[u8], i: usize) -> Option<usize> {
    Some(u32::from_le_bytes(d.get(i..i + 4)?.try_into().ok()?) as usize)
}

/// Locate the zip local header for `__main__.py` and return its offset and contents.
fn find_main_py(data: &[u8]) -> Option<(usize, usize, &[u8])> {
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
        return Some((at, method, raw));
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
        None => s.split_whitespace().next()?,
    };
    if exe.starts_with('/') {
        return None; // `/bin/sh` style launchers: nothing Windows-specific to check
    }
    let p = PathBuf::from(exe);
    Some(if p.is_absolute() { p } else { exe_dir.join(p) })
}

/// uv's Python trampolines (`python3.x.exe` from `uv python install`) carry
/// no zip: the interpreter path is an RCDATA resource named `UV_PYTHON_PATH`.
pub fn uv_python_target(data: &[u8], exe_dir: &Path) -> Option<PathBuf> {
    let raw = match FileKind::parse(data).ok()? {
        FileKind::Pe32 => uv_python_path::<ImageNtHeaders32>(data),
        FileKind::Pe64 => uv_python_path::<ImageNtHeaders64>(data),
        _ => None,
    }?;
    let p = PathBuf::from(raw);
    Some(if p.is_absolute() { p } else { exe_dir.join(p) })
}

fn uv_python_path<Pe: ImageNtHeaders>(data: &[u8]) -> Option<String> {
    let pe = PeFile::<Pe, &[u8]>::parse(data).ok()?;
    let sections = pe.section_table();
    let dir = pe
        .data_directories()
        .resource_directory(data, &sections)
        .ok()??;
    let rcdata = dir
        .root()
        .ok()?
        .entries
        .iter()
        .find(|e| e.name_or_id().id() == Some(RT_RCDATA))?;
    let named = rcdata.data(dir).ok()?.table()?.entries.iter().find(|e| {
        e.name_or_id()
            .name()
            .and_then(|n| n.to_string_lossy(dir).ok())
            .as_deref()
            == Some("UV_PYTHON_PATH")
    })?;
    let lang = named.data(dir).ok()?.table()?.entries.first()?;
    let entry = lang.data(dir).ok()?.data()?;
    let bytes = sections.pe_data_at(data, entry.offset_to_data.get(LE))?;
    let path = bytes.get(..entry.size.get(LE) as usize)?;
    Some(String::from_utf8_lossy(path).into_owned())
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
        let t = parse(&exe, Path::new(r"C:\Python312\Scripts"))
            .unwrap()
            .unwrap();
        assert_eq!(t.python, PathBuf::from(r"C:\Python312\python.exe"));
        assert_eq!(t.module, "somepkg");
    }

    #[test]
    fn parses_quoted_shebang() {
        let mut exe = b"MZ".to_vec();
        exe.extend(b"#!\"C:\\Program Files\\Python\\python.exe\"\r\n");
        exe.extend(stored_zip(MAIN_PY, MAIN.as_bytes()));
        let t = parse(&exe, Path::new(r"C:\x")).unwrap().unwrap();
        assert_eq!(
            t.python,
            PathBuf::from(r"C:\Program Files\Python\python.exe")
        );
    }

    #[test]
    fn interpreter_arguments_are_not_part_of_the_path() {
        assert_eq!(
            interpreter(r"#!C:\Python\python.exe -u", Path::new(r"C:\tool")),
            Some(PathBuf::from(r"C:\Python\python.exe"))
        );
        assert_eq!(
            interpreter(
                r#"#!"C:\Program Files\Python\python.exe" -u"#,
                Path::new(r"C:\tool")
            ),
            Some(PathBuf::from(r"C:\Program Files\Python\python.exe"))
        );
    }

    #[test]
    fn parses_uv_launcher() {
        let main = format!("#!C:\\uv\\tools\\sometool\\Scripts\\python.exe\n{MAIN}");
        let mut exe = b"MZ ... UV_PYTHON_PATH ... PA".to_vec();
        exe.extend(stored_zip(MAIN_PY, main.as_bytes()));
        exe.extend(b"PK\x01\x02 central dir");
        let t = parse(&exe, Path::new(r"C:\x")).unwrap().unwrap();
        assert_eq!(
            t.python,
            PathBuf::from(r"C:\uv\tools\sometool\Scripts\python.exe")
        );
        assert_eq!(t.module, "somepkg");
    }

    #[test]
    fn oversized_deflated_launcher_is_rejected_without_expanding_it_all() {
        use std::io::Write;
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
        encoder
            .write_all(&vec![b'x'; MAX_SCRIPT_BYTES * 4])
            .unwrap();
        let compressed = encoder.finish().unwrap();
        let mut zip = stored_zip(MAIN_PY, &compressed);
        zip[8..10].copy_from_slice(&8u16.to_le_bytes());
        assert!(parse(&zip, Path::new("C:\\\\")).is_err());
        let stored = stored_zip(MAIN_PY, &vec![b'x'; MAX_SCRIPT_BYTES + 1]);
        assert!(parse(&stored, Path::new("C:\\\\")).is_err());
    }

    #[test]
    fn ignores_other_zips_and_plain_exes() {
        let mut exe = b"MZ".to_vec();
        exe.extend(stored_zip(b"data.txt", b"hello"));
        assert_eq!(parse(&exe, Path::new("C:\\")), Ok(None));
        assert_eq!(parse(b"MZ plain program", Path::new("C:\\")), Ok(None));
    }

    #[test]
    fn embedded_python_actions_are_not_treated_as_native_executables() {
        let main = format!("{MAIN}\nopen('marker', 'w').close()\n");
        let mut exe = b"MZ stub\n#!C:\\Python\\python.exe\n".to_vec();
        exe.extend(stored_zip(MAIN_PY, main.as_bytes()));
        assert!(parse(&exe, Path::new(r"C:\x")).is_err());
    }

    #[test]
    fn reads_uv_python_trampoline_target() {
        let exe = include_bytes!("../../../tests/fixtures/uv-python-trampoline.exe");
        let p = uv_python_target(exe, Path::new(r"C:\x")).unwrap();
        assert!(p.starts_with(r"C:\pathsok-fixture")); // baked into the fixture under the old name
        assert!(p.ends_with("python.exe"));
        assert_eq!(parse(exe, Path::new(r"C:\x")), Ok(None));
    }

    #[test]
    fn other_programs_have_no_uv_python_target() {
        assert_eq!(
            uv_python_target(b"MZ plain program", Path::new("C:\\")),
            None
        );
        let root = std::env::var_os("SystemRoot")
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        let data = std::fs::read(root.join(r"System32\cmd.exe")).unwrap();
        assert_eq!(uv_python_target(&data, Path::new("C:\\")), None);
    }
}
