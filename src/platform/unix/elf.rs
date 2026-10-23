//! ELF programs: resolve every needed shared library the way the dynamic
//! loader does (RPATH, LD_LIBRARY_PATH, RUNPATH, ld.so.conf, default folders).

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use object::elf::{DT_NEEDED, DT_RPATH, DT_RUNPATH, DT_SONAME, FileHeader32, FileHeader64};
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

#[derive(Debug, Clone, Default)]
struct Deps {
    needed: Vec<String>,
    rpath: Vec<String>,
    /// Presence matters: even an empty RUNPATH suppresses inherited RPATH.
    runpath: Option<Vec<String>>,
    soname: Option<String>,
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
    Some(Ident {
        is64: head[4] == 2,
        machine,
    })
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
            deps.rpath
                .extend(value.split(':').filter(|s| !s.is_empty()).map(String::from));
        } else if d.tag == DT_RUNPATH {
            deps.runpath = Some(
                value
                    .split(':')
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect(),
            );
        } else if d.tag == DT_SONAME {
            deps.soname = Some(value);
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
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let base = path.parent().unwrap_or(Path::new("/"));
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if let Some(pattern) = line.strip_prefix("include") {
            for file in expand_glob(&base.join(pattern.trim())) {
                parse_conf(&file, out, depth + 1);
            }
        } else if !line.is_empty() {
            out.extend(
                line.split([':', ',', ' ', '\t'])
                    .filter(|s| s.starts_with('/'))
                    .map(PathBuf::from),
            );
        }
    }
}

/// Just enough globbing for ld.so.conf: one `*` in the file name.
fn expand_glob(pattern: &Path) -> Vec<PathBuf> {
    let name = pattern
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Some((prefix, suffix)) = name.split_once('*') else {
        return vec![pattern.to_path_buf()];
    };
    let dir = pattern.parent().unwrap_or(Path::new("/"));
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
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
    PathBuf::from(
        entry
            .replace("${ORIGIN}", &o)
            .replace("$ORIGIN", &o)
            .replace("${LIB}", lib)
            .replace("$LIB", lib),
    )
}

/// Folders searched for `deps`' libraries, in the loader's order.
fn search_dirs(
    deps: &Deps,
    origin: &Path,
    id: Ident,
    inherited: &[PathBuf],
    env: &[PathBuf],
    system: &[PathBuf],
) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    // DT_RPATH only applies when there is no DT_RUNPATH.
    if deps.runpath.is_none() {
        dirs.extend(deps.rpath.iter().map(|e| expand_origin(e, origin, id.is64)));
        dirs.extend_from_slice(inherited);
    }
    dirs.extend_from_slice(env);
    dirs.extend(
        deps.runpath
            .iter()
            .flatten()
            .map(|e| expand_origin(e, origin, id.is64)),
    );
    dirs.extend_from_slice(system);
    dirs
}

fn system_dirs(id: Ident) -> Vec<PathBuf> {
    let mut dirs = ld_so_conf_dirs().to_vec();
    if id.is64 {
        dirs.extend(["/lib64", "/usr/lib64"].map(PathBuf::from));
    }
    dirs.extend(["/lib", "/usr/lib"].map(PathBuf::from));
    dirs
}

fn is_loader(name: &str) -> bool {
    name.starts_with("ld-linux") || name.starts_with("ld64.so") || name.starts_with("linux-vdso")
}

fn resolve(
    name: &str,
    dirs: &[PathBuf],
    load: &dyn Fn(&Path) -> Option<Deps>,
) -> Option<(PathBuf, Deps)> {
    if name.contains('/') {
        let p = PathBuf::from(name);
        return load(&p).map(|deps| (p, deps));
    }
    dirs.iter()
        .map(|d| d.join(name))
        .find_map(|p| load(&p).map(|deps| (p, deps)))
}

/// Follow the loader's breadth-first dependency order. Only successfully loaded
/// names/SONAMEs are reused. RPATH chains follow each object's first loader.
/// See glibc elf/dl-load.c (_dl_lookup_map and _dl_map_new_object).
fn walk(
    file: &Path,
    deps: Deps,
    id: Ident,
    env: &[PathBuf],
    system: &[PathBuf],
    load: &dyn Fn(&Path) -> Option<Deps>,
) -> Vec<Reason> {
    let mut loaded = HashMap::new();
    if let Some(name) = &deps.soname {
        loaded.insert(name.clone(), file.to_path_buf());
    }
    let mut visited = HashSet::from([file.to_path_buf()]);
    let mut queue = VecDeque::from([(file.to_path_buf(), deps, Vec::new(), 0)]);
    let mut out = Vec::new();
    while let Some((file, deps, inherited, depth)) = queue.pop_front() {
        let origin = file.parent().unwrap_or(Path::new("/"));
        let dirs = search_dirs(&deps, origin, id, &inherited, env, system);
        let mut chain = Vec::new();
        if deps.runpath.is_none() {
            chain.extend(deps.rpath.iter().map(|e| expand_origin(e, origin, id.is64)));
        }
        chain.extend(inherited);
        for name in &deps.needed {
            if is_loader(name) || loaded.contains_key(name) {
                continue;
            }
            match resolve(name, &dirs, load) {
                None => out.push(Reason::MissingLibrary {
                    name: name.clone(),
                    needed_by: file.clone(),
                }),
                Some((lib, sub)) => {
                    loaded.insert(name.clone(), lib.clone());
                    if let Some(soname) = &sub.soname {
                        loaded.entry(soname.clone()).or_insert_with(|| lib.clone());
                    }
                    // Keep the existing bound and don't recurse into distro libraries.
                    if visited.insert(lib.clone())
                        && depth < MAX_DEPTH
                        && !SYSTEM_LIB_PREFIXES.iter().any(|p| lib.starts_with(p))
                    {
                        queue.push_back((lib, sub, chain.clone(), depth + 1));
                    }
                }
            }
        }
    }
    out
}

fn walk_files(path: &Path, deps: Deps, id: Ident) -> Vec<Reason> {
    let env = std::env::var_os("LD_LIBRARY_PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    walk(path, deps, id, &env, &system_dirs(id), &|p| {
        (ident(p) == Some(id)).then(|| read_deps(p).unwrap_or_default())
    })
}

/// Every needed shared library that the loader would fail to find.
pub fn missing_libraries(path: &Path) -> Vec<Reason> {
    let (Some(id), Some(deps)) = (ident(path), read_deps(path)) else {
        return Vec::new();
    };
    walk_files(path, deps, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_binary_resolves() {
        let ls = Path::new("/bin/ls");
        assert!(
            read_deps(ls).is_some_and(|d| !d.needed.is_empty()),
            "expected a dynamically linked /bin/ls"
        );
        let missing = missing_libraries(ls);
        assert!(
            missing.is_empty(),
            "{:?}",
            missing.iter().map(ToString::to_string).collect::<Vec<_>>()
        );
    }

    #[test]
    fn missing_library_is_reported() {
        let ls = Path::new("/bin/ls");
        let id = ident(ls).unwrap();
        let deps = Deps {
            needed: vec!["libdefinitely-not-here.so.9".into()],
            ..Deps::default()
        };
        let out = walk_files(ls, deps, id);
        assert_eq!(out.len(), 1);
        assert!(out[0].to_string().contains("libdefinitely-not-here.so.9"));
    }

    #[test]
    fn direct_dependency_is_loaded_before_a_siblings_transitive_dependencies() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        let lib = root.path().join("lib");
        fs::create_dir(&bin).unwrap();
        fs::create_dir(&lib).unwrap();
        let original = fs::read("/bin/true").unwrap();
        let mut dependent = original.clone();
        let old = b"libc.so.6\0";
        let mut replacements = 0;
        for at in 0..=dependent.len() - old.len() {
            if &dependent[at..at + old.len()] == old {
                dependent[at..at + old.len()].copy_from_slice(b"libB.so.6\0");
                replacements += 1;
            }
        }
        assert!(replacements > 0, "expected a dynamically linked /bin/true");
        fs::write(lib.join("libA.so.6"), dependent).unwrap();
        fs::write(lib.join("libB.so.6"), original).unwrap();
        let deps = Deps {
            needed: vec!["libA.so.6".into(), "libB.so.6".into()],
            runpath: Some(vec!["$ORIGIN/../lib".into()]),
            ..Deps::default()
        };
        let out = walk_files(
            &bin.join("main"),
            deps,
            ident(Path::new("/bin/true")).unwrap(),
        );
        assert!(out.is_empty(), "{out:?}");
    }

    fn mock_walk(root: Deps, files: &[(&str, Deps)]) -> Vec<Reason> {
        walk(
            Path::new("/app/main"),
            root,
            Ident {
                is64: true,
                machine: 62,
            },
            &[],
            &[],
            &|path| {
                files
                    .iter()
                    .find(|(name, _)| path == Path::new(name))
                    .map(|(_, deps)| deps.clone())
            },
        )
    }

    #[test]
    fn rpath_is_inherited_but_runpath_is_not() {
        let a = Deps {
            needed: vec!["libB.so".into()],
            ..Deps::default()
        };
        let mut root = Deps {
            needed: vec!["libA.so".into()],
            runpath: Some(vec!["$ORIGIN/lib".into()]),
            ..Deps::default()
        };
        let files = [
            ("/app/lib/libA.so", a.clone()),
            ("/app/lib/libB.so", Deps::default()),
        ];
        let missing = mock_walk(root.clone(), &files);
        assert_eq!(missing.len(), 1);
        assert!(
            matches!(&missing[0], Reason::MissingLibrary { name, needed_by } if name == "libB.so" && needed_by == Path::new("/app/lib/libA.so"))
        );
        root.runpath = None;
        root.rpath = vec!["$ORIGIN/lib".into()];
        assert!(mock_walk(root.clone(), &files).is_empty());
        // A present but empty RUNPATH suppresses all RPATH searches for A.
        let mut a = a;
        a.runpath = Some(vec![]);
        let files = [
            ("/app/lib/libA.so", a),
            ("/app/lib/libB.so", Deps::default()),
        ];
        assert_eq!(mock_walk(root, &files).len(), 1);
    }

    #[test]
    fn already_loaded_sonames_and_cycles_are_reused() {
        let root = Deps {
            needed: vec!["/bundle/alias.so".into(), "actual.so".into()],
            ..Deps::default()
        };
        let library = Deps {
            needed: vec!["actual.so".into()],
            soname: Some("actual.so".into()),
            ..Deps::default()
        };
        assert!(mock_walk(root, &[("/bundle/alias.so", library)]).is_empty());
    }

    #[test]
    fn search_priority_matches_loader_rpath_environment_and_runpath_rules() {
        let id = Ident {
            is64: true,
            machine: 62,
        };
        let mut deps = Deps {
            rpath: vec!["$ORIGIN/own".into()],
            ..Deps::default()
        };
        let inherited = [PathBuf::from("/parent")];
        let env = [PathBuf::from("/environment")];
        let system = [PathBuf::from("/system")];
        assert_eq!(
            search_dirs(&deps, Path::new("/app"), id, &inherited, &env, &system),
            ["/app/own", "/parent", "/environment", "/system"].map(PathBuf::from)
        );
        deps.runpath = Some(vec!["$ORIGIN/run".into()]);
        assert_eq!(
            search_dirs(&deps, Path::new("/app"), id, &inherited, &env, &system),
            ["/environment", "/app/run", "/system"].map(PathBuf::from)
        );
    }

    #[test]
    fn origin_and_conf_includes() {
        assert_eq!(
            expand_origin("$ORIGIN/../lib", Path::new("/opt/app/bin"), true),
            PathBuf::from("/opt/app/bin/../lib")
        );
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("conf.d")).unwrap();
        fs::write(
            dir.path().join("main.conf"),
            "# comment\ninclude conf.d/*.conf\n/opt/one\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("conf.d").join("a.conf"),
            "/opt/two:/opt/three\n",
        )
        .unwrap();
        fs::write(dir.path().join("conf.d").join("skip.txt"), "/opt/never\n").unwrap();
        let mut out = Vec::new();
        parse_conf(&dir.path().join("main.conf"), &mut out, 0);
        assert_eq!(
            out,
            ["/opt/two", "/opt/three", "/opt/one"].map(PathBuf::from)
        );
    }

    #[test]
    fn non_elf_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("script");
        fs::write(&f, "#!/bin/sh\n").unwrap();
        assert!(missing_libraries(&f).is_empty());
    }
}
