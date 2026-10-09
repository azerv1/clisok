# pathsok

Find broken paths: bad PATH entries, dead launchers and shims, missing DLLs and
shared libraries, and tools whose `--help` fails. Runs on Windows and Linux.

```
pathsok check            # check PATH entries and every tool on PATH
pathsok check --json     # same, as JSON on stdout
pathsok list             # list every command and the copies it shadows; runs nothing
```

`pathsok check` exits 0 when everything is fine, nonzero when it finds problems.
Run `pathsok check --help` for all options.

## Install

Download a binary from [Releases](https://github.com/azerv1/pathsok/releases):
Windows x86_64 `.zip`, or a static Linux x86_64 `.tar.gz`.

## GitHub Action

Check the runner's PATH, for example after your setup steps:

```yaml
- uses: azerv1/pathsok@0.4.0
  with:
    args: check --skip some-noisy-tool
```

| Input     | Default | Meaning                                                              |
|-----------|---------|----------------------------------------------------------------------|
| `args`    | `check` | Arguments passed to pathsok, split on whitespace                     |
| `version` | the ref | Release to download; defaults to the ref you used, else the latest  |

The step fails when `check` finds problems. Linux and Windows runners only.
