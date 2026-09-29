# CI in five minutes

ripbi fails a pull request that leaves a measure, column, or table no report
uses. On a pull request it scans twice, the change and its target branch, and
fails only on findings the change *introduces*, so a model with hundreds of old
findings can gate from day one (see
[Comparing against another checkout](output.md#comparing-against-another-checkout)).

Pick your CI, paste the file, open a pull request.

## GitHub Actions

`.github/workflows/ripbi.yml`:

```yaml
name: ripbi
on: pull_request

permissions:
  contents: read
  security-events: write # inline annotations through code scanning

jobs:
  scan:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: bgarcevic/ripbi-action@v1
```

What a pull request gets:

- a failed check when the change adds a finding;
- the new findings annotated on the changed files, through code scanning;
- counts and a findings table on the run's summary page.

Add `comment: true` (and `pull-requests: write`) for a pull request comment that
is updated in place. Code scanning needs a public repository or GitHub Advanced
Security; without it the SARIF upload fails without failing the job, and the
rest still works. Every input is in the
[action's README](https://github.com/bgarcevic/ripbi/tree/main/ci/github-action#inputs).

## Azure Pipelines

`azure-pipelines.yml`, using the template from this repository:

```yaml
trigger: [main]
pr: [main]

resources:
  repositories:
    - repository: ripbi
      type: github
      name: bgarcevic/ripbi
      ref: refs/tags/v0.8.0
      endpoint: github # a GitHub service connection in the project

pool:
  vmImage: ubuntu-latest

steps:
  - checkout: self
  - template: templates/ripbi-scan.yml@ripbi
```

A repository resource on GitHub needs a
[GitHub service connection](https://learn.microsoft.com/azure/devops/pipelines/library/service-endpoints#github-service-connection).
Without one, copy
[`templates/ripbi-scan.yml`](https://github.com/bgarcevic/ripbi/blob/main/templates/ripbi-scan.yml)
into your repository and use `- template: ripbi-scan.yml`. Pin `ref` to a
release tag: the template follows ripbi's flags, and `main` may run ahead of the
latest release.

On a Git repository in Azure Repos, a pull request runs through
[build validation](https://learn.microsoft.com/azure/devops/repos/git/branch-policies#build-validation),
not `pr:`. Add this pipeline as a build validation policy on the target branch.

What a pull request gets:

- a failed build when the change adds a finding;
- each new finding as a warning or error on the run's summary, with its file and
  line;
- counts and a findings table on the run's **Extensions** tab;
- the SARIF log as the `CodeAnalysisLogs` artifact, which the
  [SARIF SAST Scans Tab](https://marketplace.visualstudio.com/items?itemName=sariftools.scans)
  extension shows, if installed.

Parameters, all optional:

| Parameter | Default | Meaning |
|---|---|---|
| `path` | discovery | What to scan: a `.pbip`, `.pbix`, project folder, `.SemanticModel`, or `.Report` |
| `compare` | `auto` | `auto` (the target branch on a pull request build, else `none`), `none`, or any git ref, e.g. the last release tag |
| `failOn` | `auto` | `new`: findings the change adds. `any`: every finding. `never`: report only. `auto`: `new` with a comparison, else `any` |
| `version` | `latest` | The ripbi release to install, e.g. `0.8.0` |
| `logIssues` | `true` | Each finding as a warning or error on the run's summary |
| `publishSarif` | `true` | The SARIF log as the `CodeAnalysisLogs` build artifact |
| `args` | | Extra `rib scan` flags, e.g. `--type measure --strict` |
| `binary` | | A folder holding a ripbi binary to use instead of downloading a release |
| `workingDirectory` | `$(System.DefaultWorkingDirectory)` | The repository root; finding paths are relative to it |
| `name` | `ripbi` | The scan step's name and output-variable prefix; give each use in one job its own |

The scan step sets `$(ripbi.newFindings)`, `$(ripbi.fixedFindings)`,
`$(ripbi.sarifFile)`, and `$(ripbi.markdownFile)` for later steps.

The template's steps run in bash, with curl, git, and jq: the Microsoft-hosted
Linux, macOS, and Windows images have all three (Windows runs Git Bash). To
fetch the target branch it sends the job's access token on Azure Repos; on
other hosts, a private repository needs `persistCredentials: true` on the
checkout step.

## Anything else

The flags the action and the template use work in any CI:

```sh
git fetch --depth=1 origin main
git worktree add ../base FETCH_HEAD
rib scan --compare-root ../base \
  --sarif-file ripbi.sarif --json-file ripbi.json --markdown-file ripbi.md
```

Exit `1` means new findings, `2` an error. `ripbi.json`'s `summary.findings` is
the count, `ripbi.md` a summary for the run's page, and `ripbi.sarif` the
annotations for tools that read SARIF. See
[Several outputs from one scan](output.md#several-outputs-from-one-scan).

## When the check fails

- **The finding is real.** Delete the object, or use it in a report.
- **It is unused on purpose,** say a measure only an Excel pivot reads. Give it a
  `ripbi_keep` annotation with the reason, see
  [Keeping objects on purpose](output.md#keeping-objects-on-purpose).
- **It is in a whole folder of objects ripbi should not judge.** Add an
  `[scan].ignore` pattern to `ripbi.toml`, see [`ripbi.toml`](output.md#ripbitoml).

An old finding never fails a pull request, so none of this is needed to adopt the
gate: only new findings need an answer.
