# pathsok

Find broken paths: bad PATH entries, dead launchers and shims, missing DLLs and
shared libraries, and tools whose `--help` fails. Runs on Windows and Linux.

```
pathsok check            # check PATH entries and every tool on PATH
pathsok check --json     # same, as JSON on stdout
pathsok list             # list every command and the copies it shadows; runs nothing
```

`pathsok check` exits 0 when everything is fine, nonzero when it finds problems.
Every problem has a stable code like `PATH002`.

**Docs:** [azerv1.github.io/pathsok](https://azerv1.github.io/pathsok/) —
[usage](https://azerv1.github.io/pathsok/usage.html),
[`.pathsok.toml` configuration](https://azerv1.github.io/pathsok/configuration.html),
[error codes](https://azerv1.github.io/pathsok/codes.html),
[GitHub Action](https://azerv1.github.io/pathsok/github-action.html).

## Install

Download a binary from [Releases](https://github.com/azerv1/pathsok/releases):
Windows x86_64 `.zip`, or a static Linux x86_64 `.tar.gz`.

## GitHub Action

```yaml
- uses: azerv1/pathsok@0.4.0
  with:
    args: check --skip some-noisy-tool
```

The step fails when `check` finds problems. Linux and Windows runners only.
