# GitHub Action

Check the runner's PATH, for example after your setup steps:

```yaml
- uses: azerv1/clisok@0.4.0
  with:
    args: check --skip some-noisy-tool
```

| Input     | Default | Meaning |
|-----------|---------|---------|
| `args`    | `check` | Arguments passed to clisok, split on whitespace |
| `version` | the ref | Release to download; defaults to the ref you used, else the latest |

The step fails when `check` finds problems. A `.clisok.toml` in the repository is picked up, so the same settings apply locally and in CI. Linux and Windows runners only.
