# ripbi scan action

Find the measures, columns, and tables no Power BI report uses, and fail the pull
requests that add new ones. [ripbi](https://github.com/bgarcevic/ripbi) reads PBIP,
PBIR, TMDL, `model.bim`, `.pbix`, `.pbit`, and `.abf` projects straight from the
repository; nothing connects to Power BI.

```yaml
name: ripbi
on: pull_request

permissions:
  contents: read
  security-events: write # inline annotations through code scanning
  pull-requests: write   # only for comment: true

jobs:
  scan:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: bgarcevic/ripbi-action@v1
```

On a pull request, the action checks out the base branch beside the repository and
compares the two scans, so the check fails only on findings the pull request
introduces. Findings that already exist on the base branch stay tracked in code
scanning but never fail the check. The default one-commit-deep checkout is enough:
the action fetches the base branch itself.

Each run:

1. installs a checksum-verified ripbi release;
2. runs `rib scan`, with `--compare-root` against the compared ref;
3. writes the counts and findings to the job summary;
4. uploads SARIF to code scanning, which annotates the pull request's files;
5. optionally posts one pull request comment, updated in place on later runs;
6. fails the step as `fail-on` says.

## Inputs

| Input | Default | Meaning |
|---|---|---|
| `path` | discovery | What to scan: a `.pbip`, `.pbix`, project folder, `.SemanticModel`, or `.Report`. Empty discovers one project in the repository root |
| `compare` | `base` on `pull_request`, else `none` | `base` (the pull request's base branch), `none`, or any git ref, e.g. the last release tag |
| `fail-on` | `new` with a comparison, else `any` | `new`: findings the change adds. `any`: every finding, existing ones included. `never`: report only |
| `version` | `latest` | The ripbi release to install, e.g. `0.8.0` |
| `upload-sarif` | `auto` | `auto` uploads and carries on when the job lacks `security-events: write` or code scanning is off; `true` fails the step instead; `false` skips it |
| `comment` | `false` | `true` posts the summary as a pull request comment (needs `pull-requests: write`; skipped with a warning on forks, whose token is read-only) |
| `args` | | Extra `rib scan` flags, split on whitespace, e.g. `--type measure --strict` |
| `binary` | | A folder holding a ripbi binary to use instead of downloading a release |
| `github-token` | `github.token` | Used for the release lookup and the comment |

An error that stops the scan itself (a bad `path`, a model that cannot be read, or
skip notices under `--strict`) always fails the step.

## Outputs

| Output | Meaning |
|---|---|
| `new-findings` | The findings the scan reports: under a comparison, only the new ones |
| `fixed-findings` | Findings of the compared ref that are gone; `0` without a comparison |
| `sarif-file` | The SARIF log's path |

## Keeping an object on purpose

An object that is unused on purpose, such as a measure only an Excel pivot reads, gets a
`ripbi_keep` annotation in the model with the reason as its value:

```tmdl
measure 'Budget Variance' = [Budget] - [Actual]
    annotation ripbi_keep = Used by the Finance Excel pivot (FIN-231)
```

`[scan].ignore` patterns in a `ripbi.toml` also work. See
[Keeping objects on purpose](https://bgarcevic.github.io/ripbi/output.html#keeping-objects-on-purpose).

## Versions

`@v1` follows the latest release of this action and never changes its inputs or
outputs incompatibly. `@v1.0.0` and later tags never move, for a workflow that
pins one exact version. Either way, the action installs the `version` input's
ripbi release, `latest` by default.

## Several models

Run the action once per model, with a distinct `path` each. Each run uploads its SARIF
under its own code scanning category and keeps its own comment.

This action is developed in [bgarcevic/ripbi](https://github.com/bgarcevic/ripbi/tree/main/ci/github-action)
and mirrored here on every ripbi release. File issues there.
