# clisok

Check that the executables on your system work: PATH entries, launchers and
shims, DLLs and shared libraries, interpreters, and a headless test run of each
tool that is safe to run. Runs on Windows and Linux.

```
clisok check            # check PATH entries and every tool on PATH
clisok check --output-format json  # same, as JSON on stdout
clisok check --registry --fix   # also apply safe fixes to the saved user PATH (Windows)
clisok list             # list every command and the copies it shadows; runs nothing
clisok rule list        # list every rule, its default severity and fix availability
clisok rule show CLI007 # explain a code
```

`clisok check` exits 0 when everything is fine, nonzero when it finds problems.
Every problem has a stable code like `CLI002`.

**Docs:** [azerv1.github.io/clisok](https://azerv1.github.io/clisok/) —
[usage](https://azerv1.github.io/clisok/usage.html),
[`.clisok.toml` configuration](https://azerv1.github.io/clisok/configuration.html),
[fixes](https://azerv1.github.io/clisok/fixes.html),
[error codes](https://azerv1.github.io/clisok/codes.html),
[GitHub Action](https://azerv1.github.io/clisok/github-action.html).

## Install

Download a binary from [Releases](https://github.com/azerv1/clisok/releases):
Windows x86_64 `.zip`, or a static Linux x86_64 `.tar.gz`.

## GitHub Action

```yaml
- uses: azerv1/clisok@0.4.0
  with:
    args: check --skip some-noisy-tool
```

The step fails when `check` finds problems. Linux and Windows runners only.
