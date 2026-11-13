# CI with GitHub Actions

This repository is also a GitHub Action. In one step, it installs the tool and runs
`idl-drift diff`, `idl-drift source`, `idl-verify` or `idl-drift watch`. It writes a Markdown
report to the job summary and a JSON report to a file, and it fails the step on drift.
`idl-drift` finds a change that you make to the IDL. `idl-verify` finds a change that the
program makes without an IDL update.

## Example workflows

Copy a workflow from [`examples/github-actions/`](../examples/github-actions/) into
`.github/workflows/` of the repository that holds your IDLs. Put your IDL paths in `matrix`.

| Workflow | Runs | Fails when |
|---|---|---|
| [`idl-drift-pr.yml`](../examples/github-actions/idl-drift-pr.yml) | On each pull request that changes a file under an `idls/` directory. | An IDL has a change at or above `fail-on` against the base branch. |
| [`idl-verify-nightly.yml`](../examples/github-actions/idl-verify-nightly.yml) | Each night at 03:17 UTC, and manually. | Live mainnet data does not match your IDL or your decoder under test. |
| [`idl-watch.yml`](../examples/github-actions/idl-watch.yml) | Each hour, and manually. | A program could not be checked, or a pull request or issue could not be opened. See [IDL update bot](#idl-update-bot). |

The smallest step compares an IDL with the same file on the base branch of the pull request:

```yaml
- uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
- uses: senzenn/idl-drift@v0.3.1
  with:
    idl: idls/my_program.json
```

## Inputs

| Input | Default | Meaning |
|---|---|---|
| `command` | `diff` | `diff` runs `idl-drift diff`. `verify` runs `idl-verify`. `source` runs [`idl-drift source`](idl-drift.md#source) on the workspace. Give the workspace directory in `args` when it is not the current directory. `watch` runs the [IDL update bot](#idl-update-bot). |
| `idl` | none | Path of the new IDL. Required for `diff` and `verify`. `source`: one more IDL to check (`--idl`). |
| `old` | the base branch | `diff`: path of the old IDL. Without it, the action reads the file at the same path on the base branch of the pull request with `git show`. When the base branch has no such file, the step passes with a notice. On an event other than a pull request, set `old` or `program`. |
| `program` | none | `diff`: compare with the IDL that this program publishes on chain (`--onchain`), not with `old`. `verify`: the program to sample (`--program`). |
| `rpc-url` | none | The JSON-RPC URL for `program` with `diff`, for `verify` and for `watch`. With `source`, it turns on `--onchain`. See [RPC URL](#rpc-url). |
| `fail-on` | `breaking`, `error` | The lowest tier (`diff`, `source`) or severity (`verify`) that fails the step. |
| `allow` | none | Path of an allow file. See [idl-drift](idl-drift.md#allow-file) and [idl-verify](idl-verify.md#allow-file). `source` takes no allow file. |
| `programs` | none | `watch`: path of the programs file, one `<program> <local IDL path> [metadata\|anchor]` per line. Required for `watch`. |
| `state` | none | `watch`: path of the state file that keeps the slots of the last run. Keep it between runs, for example with `actions/cache`. |
| `token` | `github.token` | `watch`: the token that `gh` uses. Other commands get no token. In `watch`, the environment of the run step holds the token, and only `gh` uses it. |
| `args` | none | More options, separated by spaces, such as `--min-coverage 50` or `--decoder target/release/verify-decoder`. The action does not expand quotes, globs or variables in them. |
| `version` | the action's ref | The idl-drift tag or commit SHA to install. See [Install](#install). |

## Outputs

| Output | Content |
|---|---|
| `exit-code` | The exit code of the tool. See [Job results](#job-results). |
| `counts` | The `counts` object of the JSON report, such as `{"errors":1,"warnings":0,"notes":2}` for `verify`. Allowed items are not counted. |
| `report` | Path of the JSON report. To keep it, upload it as an artifact. `watch`: path of the events, one JSON object per line. |
| `samples` | `verify` with live sampling only: path of the samples that the run read. |

## Install

When `version` is a tag with a release, the action downloads the release archive for the
runner: Linux or macOS, x86_64 or arm64. It stops when the archive does not agree with the
`.sha256` file of the release. For other versions and runners, it builds the tool with
`cargo install --locked --git`. This needs Rust on the runner, and GitHub-hosted runners have it.
For stable results, pin the action to a full commit SHA, and set `version` to the tag of that
commit. Then the action downloads the release binary:

```yaml
- uses: senzenn/idl-drift@<full commit SHA> # v0.3.1
  with:
    version: v0.3.1
    idl: idls/my_program.json
```

## Job results

| Step result | Cause | What to do |
|---|---|---|
| Passes | Exit 0: no change or finding at or above `fail-on`, other than allowed ones. The report can still list smaller changes, warnings and notes. | Nothing. |
| `IDL drift` | Exit 1. `diff`: at least one change at or above `fail-on`. `verify`: at least one error. Examples are an unknown discriminator, bytes that the IDL does not read, and parser values that differ from the IDL. | Read the job summary, the [tiers](idl-drift.md#tiers) and the [checks](idl-verify.md#checks). To accept a change, add it to the allow file. |
| `Program id drift` | Exit 1. `source`: at least one finding at or above `fail-on`, such as an `Anchor.toml` or IDL id that is not the `declare_id!`. | Read the job summary and the [findings](idl-drift.md#findings). `source` has no allow file. |
| `idl-drift could not run`, `idl-verify could not run` | Exit 2, for one of these causes: a file could not be read; an input is wrong; the RPC failed or rate-limited after 5 attempts; the decoder did not start; nothing was sampled. | This is not a finding about the IDL. Fix the setup, or run the workflow again. |
| Timeout | The RPC is too slow for `--max-tx`. | Run again, or lower the sampling limits in `args`. |

## IDL update bot

`command: watch` runs [`idl-drift watch --once`](idl-drift.md#watch) on the programs file. Then
the action looks at each program with a new deploy or a new on-chain IDL since the last run. It
compares the on-chain IDL with your local IDL:

| Highest change | What the action does |
|---|---|
| None | Nothing. The job summary lists the program. |
| Additive or Cosmetic | It writes the on-chain IDL, formatted with `jq .`, to the local path on a new branch `idl-drift/<program>-<key>`. It pushes the branch and opens a pull request to the branch of the run with `gh pr create`. The body has the Markdown diff. |
| Breaking or Dangerous | It opens an issue with the Markdown diff with `gh issue create`, because the parser needs a change before the IDL file. |

`<key>` is the first 12 hex digits of the sha256 of the on-chain IDL text. A transaction that
names the IDL account moves its slot but does not change its text. So the action opens one pull
request or issue for each IDL. It skips a branch that has a pull request (open, closed or
merged), and an issue with the same title (open or closed). A branch with no pull request comes
from a run in which `gh pr create` failed. The action replaces it with a force push, but only
under `idl-drift/`. A program deployed after its last IDL write also gets a `Stale on-chain IDL`
warning. The action copies the state entry of a program to the `state` file only after its pull
request or issue. So the next run tries a failed program again.

To set up the bot, start from [`idl-watch.yml`](../examples/github-actions/idl-watch.yml):

- Give the job the `contents: write`, `pull-requests: write` and `issues: write` permissions.
- In the repository settings, turn on *Allow GitHub Actions to create and approve pull
  requests*. If you do not, `gh pr create` fails.
- GitHub starts no workflow for a pull request that `github.token` opens. For CI on the bot's
  pull requests, give a GitHub App token or a fine-grained token to `token`. Also give it to
  `actions/checkout`, which keeps the credentials that the push uses.
- Run the job on `schedule` or `workflow_dispatch`, from the branch that the pull requests go to.
- The commit does not change the checkout or its index, so later steps see the files of the run.

The step fails (exit 2) when a program could not be checked, or a pull request or issue could
not be opened. The other programs still get their pull request or issue.

## RPC URL

Providers put the API key in the URL. Keep the URL in a repository secret
(`gh secret set IDL_VERIFY_RPC_URL`). Then give the secret as
`rpc-url: ${{ secrets.IDL_VERIFY_RPC_URL }}`.
The action masks the URL in the log. It gives the URL to the tool only in the
`IDL_VERIFY_RPC_URL` or `IDL_DRIFT_RPC_URL` environment variable, never on the command line.
Do not put the URL in `args`. If your RPC plan rate-limits parallel runs, lower `max-parallel`
in the matrix.

A live `verify` samples one time and saves the samples, and the Markdown report reads them. So
do not give `--save-samples` in `args`. To run the same check again on your machine with no
RPC, download the `samples` file and run
`idl-verify --idl idls/my_program.json --samples samples.json`.
GitHub stops scheduled workflows in a public repository after 60 days with no activity.

## Security

- IDL text can come from the author of a pull request. The Markdown report escapes it. The
  action stops workflow commands while it prints the report to the log, and control characters
  reach the log as `?`.
- The action gives each input to its script in an environment variable, not in the script text.
- The `idl`, `allow` and `programs` paths must resolve to a file in the workspace, also through
  a symlink. The directory of `state` must be in the workspace. If not, the step stops.
- A pull request from a fork gets no secrets. Run `verify` on `schedule` or
  `workflow_dispatch`, not on `pull_request_target`.
- `watch` writes only to local IDL paths in the workspace. It puts only checked program ids and
  slots in branch names and titles.
- The CI of this repository runs `scripts/test-action.sh`. It checks the `?` replacement, the
  workspace path rule and the `exit-code` output. It tests `watch` with a fake `gh`.
