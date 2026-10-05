//! ELF programs: resolve every needed shared library the way the dynamic
//! loader does (RPATH, LD_LIBRARY_PATH, RUNPATH, ld.so.conf, default folders).

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use object::elf::{DT_NEEDED, DT_RPATH, DT_RUNPATH, FileHeader32, FileHeader64};
use object::read::elf::FileHeader;
use object::{Endianness, FileKind, ReadCache, ReadRef};

use crate::inspect::Reason;

/// How deep to follow libraries that ship outside the system lib folders.
const MAX_DEPTH: usize = 3;
const SYSTEM_LIB_PREFIXES: &[&str] = &["/lib", "/usr/lib"];

/// Class and machine: a library only satisfies a program built for the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ident {
    is64: bool,
    machine: u16,
}

#[derive(Debug, Default)]
struct Deps {
    needed: Vec<String>,
    rpath: Vec<String>,
    runpath: Vec<String>,
}

fn ident(path: &Path) -> Option<Ident> {
    let mut head = [0u8; 20];
    File::open(path).ok()?.read_exact(&mut head).ok()?;
    if &head[0..4] != b"\x7fELF" {
        return None;
    }
    let machine = match head[5] {
        1 => u16::from_le_bytes([head[18], head[19]]),
        _ => u16::from_be_bytes([head[18], head[19]]),
    };
    Some(Ident { is64: head[4] == 2, machine })
}

fn read_deps(path: &Path) -> Option<Deps> {
    let cache = ReadCache::new(File::open(path).ok()?);
    match FileKind::parse(&cache).ok()? {
        FileKind::Elf32 => parse::<FileHeader32<Endianness>, _>(&cache),
        FileKind::Elf64 => parse::<FileHeader64<Endianness>, _>(&cache),
        _ => None,
    }
}

fn parse<'d, Elf: FileHeader<Endian = Endianness>, R: ReadRef<'d>>(data: R) -> Option<Deps> {
    let header = Elf::parse(data).ok()?;
    let endian = header.endian().ok()?;
    let sections = header.sections(endian, data).ok()?;
    let table = sections.dynamic_table(endian, data).ok()?;
    let mut deps = Deps::default();
    for d in table.iter() {
        let Ok(bytes) = table.string(d) else { continue };
        let value = String::from_utf8_lossy(bytes).into_owned();
        if d.tag == DT_NEEDED {
            deps.needed.push(value);
        } else if d.tag == DT_RPATH {
            deps.rpath.extend(value.split(':').filter(|s| !s.is_empty()).map(String::from));
        } else if d.tag == DT_RUNPATH {
            deps.runpath.extend(value.split(':').filter(|s| !s.is_empty()).map(String::from));
        }
    }
    Some(deps)
}

/// Folders listed in /etc/ld.so.conf and the files it includes.
fn ld_so_conf_dirs() -> &'static [PathBuf] {
    static DIRS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    DIRS.get_or_init(|| {
        let mut out = Vec::new();
        parse_conf(Path::new("/etc/ld.so.conf"), &mut out, 0);
        out
    })
}

fn parse_conf(path: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 8 {
        return;
    }
    let Ok(text) = fs::read_to_string(path) else { return };
    let base = path.parent().unwrap_or(Path::new("/"));
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if let Some(pattern) = line.strip_prefix("include") {
            for file in expand_glob(&base.join(pattern.trim())) {
                parse_conf(&file, out, depth + 1);
            }
        } else if !line.is_empty() {
            out.extend(line.split([':', ',', ' ', '\t']).filter(|s| s.starts_with('/')).map(PathBuf::from));
        }
    }
}

/// Just enough globbing for ld.so.conf: one `*` in the file name.
fn expand_glob(pattern: &Path) -> Vec<PathBuf> {
    let name = pattern.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let Some((prefix, suffix)) = name.split_once('*') else { return vec![pattern.to_path_buf()] };
    let dir = pattern.parent().unwrap_or(Path::new("/"));
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with(prefix) && n.ends_with(suffix)
        })
        .map(|e| e.path())
        .collect();
    files.sort();
    files
}

fn expand_origin(entry: &str, origin: &Path, is64: bool) -> PathBuf {
    let o = origin.to_string_lossy();
    let lib = if is64 { "lib64" } else { "lib" };
    PathBuf::from(entry.replace("${ORIGIN}", &o).replace("$ORIGIN", &o).replace("${LIB}", lib).replace("$LIB", lib))
}

/// Folders searched for `deps`' libraries, in the loader's order.
fn search_dirs(deps: &Deps, origin: &Path, id: Ident) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    // DT_RPATH only applies when there is no DT_RUNPATH.
    if deps.runpath.is_empty() {
        dirs.extend(deps.rpath.iter().map(|e| expand_origin(e, origin, id.is64)));
    }
    if let Some(llp) = std::env::var_os("LD_LIBRARY_PATH") {
        dirs.extend(std::env::split_paths(&llp).filter(|p| !p.as_os_str().is_empty()));
    }
    dirs.extend(deps.runpath.iter().map(|e| expand_origin(e, origin, id.is64)));
    dirs.extend(ld_so_conf_dirs().iter().cloned());
    if id.is64 {
        dirs.extend(["/lib64", "/usr/lib64"].map(PathBuf::from));
    }
    dirs.extend(["/lib", "/usr/lib"].map(PathBuf::from));
    dirs
}

fn is_loader(name: &str) -> bool {
    name.starts_with("ld-linux") || name.starts_with("ld64.so") || name.starts_with("linux-vdso")
}

fn resolve(name: &str, dirs: &[PathBuf], id: Ident) -> Option<PathBuf> {
    if name.contains('/') {
        let p = PathBuf::from(name);
        return (ident(&p) == Some(id)).then_some(p);
    }
    dirs.iter().map(|d| d.join(name)).find(|p| ident(p) == Some(id))
}

fn walk(file: &Path, deps: &Deps, id: Ident, depth: usize, seen: &mut HashSet<String>, out: &mut Vec<Reason>) {
    let origin = file.parent().unwrap_or(Path::new("/"));
    let dirs = search_dirs(deps, origin, id);
    for name in &deps.needed {
        if is_loader(name) || !seen.insert(name.clone()) {
            continue;
        }
        match resolve(name, &dirs, id) {
            None => out.push(Reason::MissingLibrary { name: name.clone(), needed_by: file.to_path_buf() }),
            // System libraries are maintained by the distro; only follow bundled ones.
            Some(lib) if depth < MAX_DEPTH && !SYSTEM_LIB_PREFIXES.iter().any(|p| lib.starts_with(p)) => {
                if let Some(sub) = read_deps(&lib) {
                    walk(&lib, &sub, id, depth + 1, seen, out);
                }
            }
            Some(_) => {}
        }
    }
}

/// Every needed shared library that the loader would fail to find.
pub fn missing_libraries(path: &Path) -> Vec<Reason> {
    let (Some(id), Some(deps)) = (ident(path), read_deps(path)) else { return Vec::new() };
    let mut out = Vec::new();
    walk(path, &deps, id, 0, &mut HashSet::new(), &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_binary_resolves() {
        let ls = Path::new("/bin/ls");
        assert!(read_deps(ls).is_some_and(|d| !d.needed.is_empty()), "expected a dynamically linked /bin/ls");
        let missing = missing_libraries(ls);
        assert!(missing.is_empty(), "{:?}", missing.iter().map(ToString::to_string).collect::<Vec<_>>());
    }

    #[test]
    fn missing_library_is_reported() {
        let ls = Path::new("/bin/ls");
        let id = ident(ls).unwrap();
        let deps = Deps { needed: vec!["libdefinitely-not-here.so.9".into()], ..Deps::default() };
        let mut out = Vec::new();
        walk(ls, &deps, id, 0, &mut HashSet::new(), &mut out);
        assert_eq!(out.len(), 1);
        assert!(out[0].to_string().contains("libdefinitely-not-here.so.9"));
    }

    #[test]
    fn origin_and_conf_includes() {
        assert_eq!(expand_origin("$ORIGIN/../lib", Path::new("/opt/app/bin"), true), PathBuf::from("/opt/app/bin/../lib"));
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("conf.d")).unwrap();
        fs::write(dir.path().join("main.conf"), "# comment\ninclude conf.d/*.conf\n/opt/one\n").unwrap();
        fs::write(dir.path().join("conf.d").join("a.conf"), "/opt/two:/opt/three\n").unwrap();
        fs::write(dir.path().join("conf.d").join("skip.txt"), "/opt/never\n").unwrap();
        let mut out = Vec::new();
        parse_conf(&dir.path().join("main.conf"), &mut out, 0);
        assert_eq!(out, ["/opt/two", "/opt/three", "/opt/one"].map(PathBuf::from));
    }

    #[test]
    fn non_elf_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("script");
        fs::write(&f, "#!/bin/sh\n").unwrap();
        assert!(missing_libraries(&f).is_empty());
    }
}
