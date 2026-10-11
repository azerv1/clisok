# pathsok

pathsok finds broken paths: bad PATH entries, dead launchers and shims, missing DLLs and shared libraries, and tools whose `--help` fails. It runs on Windows and Linux.

A tidy PATH isn't the same as a working one. Every folder on PATH can exist while the programs inside them point at a Python that was upgraded away, a DLL that was uninstalled, or an npm package that's gone. pathsok follows each program on PATH to what it depends on and checks that it's there. Then it runs `--help` on the programs that are safe to run, headless, so nothing opens a window.

```
pathsok check            # check PATH entries and every tool on PATH
pathsok check --json     # same, as JSON on stdout
pathsok list             # list every command and the copies it shadows; runs nothing
```

`pathsok check` exits 0 when everything is fine and 1 when it finds problems. Every problem has a stable [code](codes.md) like `PATH002`.

## Install

Download a binary from [Releases](https://github.com/azerv1/pathsok/releases): Windows x86_64 `.zip`, or a static Linux x86_64 `.tar.gz`.
