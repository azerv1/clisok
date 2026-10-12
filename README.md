# binok

Check that the executables on your system work: PATH entries, launchers and
shims, DLLs and shared libraries, interpreters, and a headless test run of each
tool that is safe to run. Runs on Windows and Linux.

```
binok check            # check PATH entries and every tool on PATH
binok check --json     # same, as JSON on stdout
binok list             # list every command and the copies it shadows; runs nothing
```

`binok check` exits 0 when everything is fine, nonzero when it finds problems.
Every problem has a stable code like `BOK002`.

**Docs:** [azerv1.github.io/binok](https://azerv1.github.io/binok/) —
[usage](https://azerv1.github.io/binok/usage.html),
[`.binok.toml` configuration](https://azerv1.github.io/binok/configuration.html),
[error codes](https://azerv1.github.io/binok/codes.html),
[GitHub Action](https://azerv1.github.io/binok/github-action.html).

## Install

Download a binary from [Releases](https://github.com/azerv1/binok/releases):
Windows x86_64 `.zip`, or a static Linux x86_64 `.tar.gz`.

## GitHub Action

```yaml
- uses: azerv1/binok@0.4.0
  with:
    args: check --skip some-noisy-tool
```

The step fails when `check` finds problems. Linux and Windows runners only.
