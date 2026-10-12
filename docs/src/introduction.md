# binok

binok checks that the executables on your system actually work: PATH entries, launchers and shims, DLLs and shared libraries, interpreters, Windows registry entries, and programs in extra places you name. It runs on Windows and Linux.

A tidy PATH isn't the same as a working system. Every folder on PATH can exist while the programs inside them point at a Python that was upgraded away, a DLL that was uninstalled, or an npm package that's gone. binok follows each program to what it depends on and checks that it's there. Then it runs `--version` (or `--help`, for tools without it) on the programs that are safe to run, headless, so nothing opens a window.

```
binok check            # check PATH entries and every tool on PATH
binok check --json     # same, as JSON on stdout
binok list             # list every command and the copies it shadows; runs nothing
```

`binok check` exits 0 when everything is fine and 1 when it finds problems. Every problem has a stable [code](codes.md) like `BIN002`.

## Install

Download a binary from [Releases](https://github.com/azerv1/binok/releases): Windows x86_64 `.zip`, or a static Linux x86_64 `.tar.gz`.
