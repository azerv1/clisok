# Related tools

pathsok asks whether every tool on PATH actually works. Two other PATH tools ask different questions, and they pair well with it.

- **[pathlint](https://github.com/ShortArrow/pathlint)** — does the right copy win? Declare which installer each command should come from (`[[expect]]` with `prefer` and `avoid`) and pathlint checks the resolution order. pathsok doesn't do this.
- **[patholog](https://github.com/techouse/patholog)** — is PATH tidy? Reports duplicate, missing and badly ordered entries, explains why a command is or isn't found, and can write a cleaned PATH into your shell profile. pathsok never edits shell profiles.

All three check PATH entries for missing folders and duplicates. Only pathsok follows each program to what it needs (DLLs and shared libraries, interpreters, shim targets, Python modules), runs `--help` headless, and checks Windows registry state: the saved PATH, App Paths and app execution aliases. pathsok is also the only one with stable numbered [error codes](codes.md).
