# GitHub Action

Check the runner's PATH, for example after your setup steps:

```yaml
- uses: azerv1/clisok@0.4.0
  with:
    args: check --skip some-noisy-tool
```

To show each finding as an annotation on the run, add `--output-format github`:

```yaml
- uses: azerv1/clisok@0.4.0
  with:
    args: check --output-format github
```

| Input     | Default | Meaning |
|-----------|---------|---------|
| `args`    | `check` | Arguments passed to clisok, split on whitespace |
| `version` | the ref | Release to download; defaults to the ref you used, else the latest |

The step fails when `check` finds problems. Pass options explicitly through `args`, using the same flags locally and in CI. Linux and Windows runners only.

The action requires Python 3.11+ and the GitHub CLI, provided by the supported
GitHub-hosted runners. It downloads the release archive and checksum manifest,
verifies the archive checksum, extracts only the executable, and checks its
version before running it. A checksum mismatch or wrong version fails the step.

The executable runs by absolute path and is never added to PATH. An optional
`assets-directory` input points to pre-downloaded release files; all checksum and
version checks still apply. Release CI uses that input to test the final archives
before publication.
