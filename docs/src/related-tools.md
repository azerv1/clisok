# Related tools

binok asks whether the executables on your system actually work. PATH is one of the places it looks; the registry, launcher targets, libraries and extra places off PATH are others. Two PATH tools ask narrower questions, and they pair well with it.

- **[pathlint](https://github.com/ShortArrow/pathlint)** — does the right copy win? Declare which installer each command should come from (`[[expect]]` with `prefer` and `avoid`) and pathlint checks the resolution order. binok doesn't do this.
- **[patholog](https://github.com/techouse/patholog)** — is PATH tidy? Reports duplicate, missing and badly ordered entries, explains why a command is or isn't found, and can write a cleaned PATH into your shell profile. binok never edits shell profiles.

All three check PATH entries for missing folders and duplicates. Only binok follows each program to what it needs (DLLs and shared libraries, interpreters, shim targets, Python modules), test-runs each tool headless, and checks Windows registry state: the saved PATH, App Paths and app execution aliases. binok is also the only one with stable numbered [error codes](codes.md).
