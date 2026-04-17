# git-vbranch

<img alt="vbranch logo" src="./logo.png" style="height: 150px">

> Virtual Branches for Git.

`git-vbranch` introduces the concept of **virtual branches** on top of Git: a virtual branch is a _logical_ branch defined by a shared label on several PRs/MRs. It has no storage of its own — it is materialized on demand by octopus-merging the concrete source branches of every PR/MR that carries the same label value.

## Why?

Typical use cases:

- **Integration preview in CI**: Validate that a set of related feature branches integrate cleanly together _before_ merging any of them.
- **Shared preview environments**: Deploy the materialized state of a virtual branch (e.g. `vbranch:dev`, `vbranch:staging`) to a shared environment — each PR tagged with the label contributes to that environment.
- **Ad-hoc groupings**: Assemble arbitrary subsets of open work without cutting a long-lived branch.

## Concepts

- **Virtual branch (vbranch)** — a logical branch identified by a label value. Example: `vbranch:dev` is the virtual branch whose concrete member branches are the source branches of every open PR/MR labeled `vbranch:dev`.
- **Member branch** — a concrete Git branch attached to a virtual branch via a labeled PR/MR.
- **Checkout** — materialize a virtual branch into the current working tree by octopus-merging all its member branches.

## How it works

`git-vbranch` exposes two subcommands:

```bash
git vbranch checkout                     # materialize vbranch of the current branch
git vbranch checkout my-feature-branch   # materialize vbranch that my-feature-branch participates in
git vbranch checkout virtual/dev         # materialize vbranch `dev` explicitly
git vbranch checkout dev                 # same — when `dev` is unambiguous
git vbranch list                         # enumerate every virtual branch in the repo
```

Each `checkout` produces a local git branch named `virtual/<name>` containing the base branch with every member branch octopus-merged in.

Both subcommands run either:

- **in CI**, where the repository context (provider, repo, tokens, current branch) is inferred from the CI environment, or
- **locally**, where the provider + repo are derived from the git remote URL (`origin` by default). Tokens are still required (via env vars like `GITHUB_TOKEN`).

### `checkout` flow

`git vbranch checkout [BRANCH]` resolves its optional positional argument to a virtual branch:

| `BRANCH` value                                            | Behavior                                                                       |
| --------------------------------------------------------- | ------------------------------------------------------------------------------ |
| `virtual/<name>`                                          | Explicit virtual branch.                                                       |
| `<branch>` matching an existing local/remote-tracking ref | Use the virtual branch this branch participates in (as a PR/MR source).        |
| `<branch>` matching a defined virtual branch name         | Use that virtual branch.                                                       |
| _omitted_                                                 | Use the **current** branch as `<branch>`. In CI, the PR source branch is used. |

Once resolved, the command **always** materializes the virtual branch as a local git branch named `virtual/<name>`:

1. Fetches the base branch and every member branch from the remote.
2. `git checkout -B virtual/<name> <remote>/<base>` — create or reset the local branch to the remote base.
3. `git merge <remote>/<member1> <remote>/<member2> ...` — octopus-merge the members.
4. Publishes a set of `VBRANCH_*` variables for downstream CI steps (see [Output variables](#output-variables)).

With `--force` / `-f`, the checkout passes `-f` to `git checkout -B`, discarding uncommitted local changes that would otherwise block the switch.

Exit cases:

- Arg resolution fails when `BRANCH` is **provided** → error.
- Arg omitted and the current branch is not associated with any virtual branch → `Nothing to do.` message, exit 0. Useful in CI pipelines running on every PR.

> [!IMPORTANT]
> **Ambiguity:** when `<branch>` matches both an existing branch AND a defined virtual branch name, the participating branch wins. If the existing branch doesn't participate in any vbranch, the name is tried as a vbranch name.

## Supported CI environments

| Platform            | Detection                     | Label mechanism              | Auth token                        |
| ------------------- | ----------------------------- | ---------------------------- | --------------------------------- |
| GitHub Actions      | `GITHUB_ACTIONS`              | Native PR labels             | `GITHUB_TOKEN`                    |
| GitLab CI           | `GITLAB_CI`                   | Native MR labels             | `GITLAB_TOKEN` or `CI_JOB_TOKEN`  |
| Bitbucket Pipelines | `BITBUCKET_PIPELINE_UUID`     | `[<label>]` in PR title (\*) | `BITBUCKET_TOKEN`                 |
| Gitea Actions       | `GITEA_ACTIONS`               | Native PR labels             | `GITEA_TOKEN` or `GITHUB_TOKEN`   |
| Forgejo Actions     | `FORGEJO_ACTIONS` / `FORGEJO` | Native PR labels             | `FORGEJO_TOKEN` or `GITHUB_TOKEN` |

> [!TIP]
> (\*) Bitbucket Cloud does not support labels on Pull Requests. As a workaround, `git-vbranch` matches PRs whose title contains `[<label>]` (e.g. `[vbranch:dev]`).

## Local mode

When no CI environment is detected, `git-vbranch` reads the URL of the `origin` remote (configurable with `--git-remote`) and auto-detects the provider by looking for the product name as a substring of the hostname:

| Hostname contains       | Provider  |
| ----------------------- | --------- |
| `github`                | GitHub    |
| `gitlab`                | GitLab    |
| `bitbucket`             | Bitbucket |
| `forgejo` or `codeberg` | Forgejo   |
| `gitea`                 | Gitea     |

This covers the cloud offerings (`github.com`, `gitlab.com`, `bitbucket.org`, `codeberg.org`) and self-hosted instances following the common `{product}.company.com` convention.

For hosts that don't match (e.g. a Gitea at `git.company.com`), pass `--provider <github|gitlab|bitbucket|gitea|forgejo>` or set `GIT_VBRANCH_PROVIDER`. The flag also forces a specific provider when several substrings match.

Tokens must still be provided via env vars: `GITHUB_TOKEN`, `GITLAB_TOKEN`, `BITBUCKET_TOKEN`, `GITEA_TOKEN`, `FORGEJO_TOKEN`.

## Installation

`git-vbranch` installs as a **native Git subcommand**. Git itself requires no configuration: any executable named `git-<something>` that sits in your `$PATH` is automatically callable as `git <something>`. So once the binary is on the path you can use either form interchangeably:

```bash
git-vbranch checkout   # direct
git vbranch checkout   # Git subcommand form — recommended
```

Verify with:

```bash
git vbranch --help
```

Pick one of the installation methods below.

### Pre-built binary (recommended)

Pre-built archives are attached to every [GitHub release](../../releases/latest) for:

- Linux x86_64 / aarch64
- macOS x86_64 (Intel) / aarch64 (Apple Silicon)
- Windows x86_64

**Linux / macOS:**

```bash
# Replace <ASSET> with one of:
#   linux-amd64, linux-arm64, darwin-amd64, darwin-arm64
ASSET=darwin-arm64
curl -L -o git-vbranch.tar.gz \
  https://github.com/<owner>/<repo>/releases/latest/download/git-vbranch-${ASSET}.tar.gz
tar -xzf git-vbranch.tar.gz
sudo install -m 755 git-vbranch /usr/local/bin/
```

On macOS, the binary is unsigned; the first run is blocked by Gatekeeper. Clear the quarantine flag once: `xattr -d com.apple.quarantine /usr/local/bin/git-vbranch`.

**Windows (PowerShell):**

```powershell
# Download and extract
Invoke-WebRequest -Uri https://github.com/<owner>/<repo>/releases/latest/download/git-vbranch-windows-amd64.zip -OutFile git-vbranch.zip
Expand-Archive git-vbranch.zip -DestinationPath .
# Move somewhere on PATH (example: a user-local bin dir)
Move-Item git-vbranch.exe "$HOME\bin\git-vbranch.exe"
```

Make sure `$HOME\bin` (or whichever directory holds `git-vbranch.exe`) is on your `PATH`.

### From crates.io

```bash
cargo install git-vbranch
```

Cargo places the binary in `~/.cargo/bin`, which should already be on your `PATH` if Rust is set up normally.

### From source

```bash
git clone https://github.com/<owner>/<repo>.git
cd git-vbranch
cargo install --path .
```

### Uninstall

```bash
# If installed via cargo install
cargo uninstall git-vbranch

# If installed manually
sudo rm /usr/local/bin/git-vbranch      # Linux / macOS
Remove-Item "$HOME\bin\git-vbranch.exe" # Windows
```

## Usage

### `git vbranch checkout`

```
git vbranch checkout [OPTIONS] [BRANCH]
```

#### Arguments

| Argument   | Description                                                                                                                                                           |
| ---------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `[BRANCH]` | Optional. Real branch name to check out first, **or** virtual branch name to materialize directly. If omitted, the virtual branch is derived from the current branch. |

#### Options

| Option          | Env var                   | Default        | Description                                                                   |
| --------------- | ------------------------- | -------------- | ----------------------------------------------------------------------------- |
| `-l`, `--label` | `GIT_VBRANCH_LABEL`       | `vbranch:(.+)` | Label (or regex with one capture group) identifying the virtual branch.       |
| `--output-dotenv` | `GIT_VBRANCH_OUTPUT_DOTENV` |              | Dotenv file the `VBRANCH_*` variables are written to. Falls back to `$GITHUB_OUTPUT` in GH / Gitea / Forgejo Actions, then `vbranch-output.env` (truncated). |
| `--dry-run`     | `GIT_VBRANCH_DRY_RUN`     | `false`        | Print what would be done without merging.                                     |
| `--fallback-rebase` | `GIT_VBRANCH_FALLBACK_REBASE` | `false` | Fallback: if the current PR has no vbranch label but targets an allowed base, rebase onto `<remote>/<base>` instead of exiting with nothing to do. Implicit mode only (no positional arg); CI only (requires `CI` env var set), ignored with a warning outside CI to avoid rewriting a developer's branch history. |

### `git vbranch list`

```
git vbranch list [OPTIONS]
```

#### Options

| Option          | Env var             | Default        | Description                                    |
| --------------- | ------------------- | -------------- | ---------------------------------------------- |
| `-l`, `--label` | `GIT_VBRANCH_LABEL` | `vbranch:(.+)` | Label (or regex) identifying virtual branches. |
| `--format`      |                     | `tree`         | Output format: `tree` (default) or `table`.    |

Example `tree` output:

```
dev (base: main)
  - #42    feat-login      (Add OAuth login)
  - #45    refactor-user   (Split user service)

staging (base: main)
  - #43    feat-billing    (Invoicing MVP)
```

Example `table` output:

```
VBRANCH   BASE  PRS  MEMBERS
dev       main    2  feat-login(#42), refactor-user(#45)
staging   main    1  feat-billing(#43)
```

### Common options (both subcommands)

| Option              | Env var                     | Default                     | Description                                                                                                          |
| ------------------- | --------------------------- | --------------------------- | -------------------------------------------------------------------------------------------------------------------- |
| `--git-remote`      | `GIT_VBRANCH_REMOTE`        | `origin`                    | Git remote used to fetch member branches (checkout) and derive the provider (local).                                 |
| `--provider`        | `GIT_VBRANCH_PROVIDER`      |                             | Provider hint: `github`, `gitlab`, `bitbucket`, `gitea`, `forgejo`. Auto-detected from the remote URL when possible. |
| `--allowed-bases`   | `GIT_VBRANCH_ALLOWED_BASES` | _repo default branch_       | Comma-separated regex list of accepted base branches. PRs targeting other bases are ignored (warning logged).        |
| `--github-token`    | `GITHUB_TOKEN`              |                             | GitHub API token.                                                                                                    |
| `--gitlab-token`    | `GITLAB_TOKEN`              |                             | GitLab API token.                                                                                                    |
| `--bitbucket-token` | `BITBUCKET_TOKEN`           |                             | Bitbucket API token.                                                                                                 |
| `--gitea-token`     | `GITEA_TOKEN`               |                             | Gitea API token.                                                                                                     |
| `--forgejo-token`   | `FORGEJO_TOKEN`             |                             | Forgejo API token.                                                                                                   |

Logging verbosity can be controlled via the `RUST_LOG` environment variable (e.g. `RUST_LOG=debug`).

## Labels: static vs. regex

The `--label` option accepts either a static string or a regex with a single capture group.

- **Static label** (e.g. `vbranch`) — there is exactly one virtual branch; all PRs carrying the label are merged together. The published `VBRANCH_NAME` is `default`.
- **Regex label** (default: `vbranch:(.+)`) — each distinct captured value defines a separate virtual branch. PRs labeled `vbranch:dev` are merged together; PRs labeled `vbranch:staging` form their own virtual branch.

The default `vbranch:(.+)` is the recommended setup: tag PRs with `vbranch:<name>` to place them in the virtual branch `<name>`.

## Output variables

On a successful `checkout`, `git-vbranch` emits five `KEY=VALUE` lines:

| Variable                | Description                                         | Example                  |
| ----------------------- | --------------------------------------------------- | ------------------------ |
| `VBRANCH_NAME`          | Captured label value (`default` for static labels). | `dev`                    |
| `VBRANCH_REF`           | Local ref name materializing the virtual branch.    | `virtual/dev`            |
| `VBRANCH_BASE`          | Base branch of the member PRs/MRs (the merge base). | `main`                   |
| `VBRANCH_MEMBER_PR_IDS` | Comma-separated list of member PR/MR IDs.           | `42,45,46`               |
| `VBRANCH_MEMBER_REFS`   | Comma-separated list of member source branches.     | `feat-a,feat-b,bugfix-x` |

They are published through two channels:

1. Always printed to stdout.
2. Written to a dotenv-formatted file, resolved as:
   1. `--output-dotenv` / `GIT_VBRANCH_OUTPUT_DOTENV` when set (appended to).
   2. otherwise `$GITHUB_OUTPUT` when defined (appended to) — exposes each variable as a step output on GitHub / Gitea / Forgejo Actions, accessible via `${{ steps.<id>.outputs.VBRANCH_NAME }}`.
   3. otherwise `vbranch-output.env` in the current directory — **truncated** on every run so the file always reflects the latest invocation. Useful as a GitLab `dotenv` artifact or when sourcing the file from a shell (`set -a && . vbranch-output.env && set +a`).

Downstream jobs can consume these to drive environment selection, deployment targets, status reporting, etc.

## CI configuration examples

The examples below download the latest `git-vbranch` release from GitHub via `curl`. For reproducible builds, replace `latest` with a specific tag (e.g. `download/v1.2.0/`). Adapt `linux-amd64` to the archive matching your runner architecture (`linux-arm64`, `darwin-amd64`, `darwin-arm64`, `windows-amd64`).

> [!IMPORTANT]
> **About triggering on label changes.** Virtual branches are defined by labels on PRs/MRs, so pipelines should rerun whenever a matching label is added or removed. CI/CD orchestrators differ widely in what they support here — each section below spells out what works natively and what requires a manual re-run.
>
> **A universal caveat:** even when label events trigger the pipeline, they only run for the PR whose label changed. **Sibling members of the same virtual branch do not re-run automatically.** If you rely on every member's latest pipeline reflecting the final vbranch state (for example, to deploy a preview env), trigger sibling pipelines manually or via a cron job after a label edit.

### GitHub Actions

GitHub Actions triggers on `labeled` / `unlabeled` natively. We filter at the job level so only label changes whose name matches the `--label` pattern actually run the job.

The simplest setup is the composite action shipped with this repo, which runs `actions/checkout`, installs the right binary for the runner, and calls `git vbranch checkout`:

```yaml
name: Virtual branch checkout
on:
  pull_request:
    types: [opened, synchronize, labeled, unlabeled]

jobs:
  vbranch:
    # On label events, skip when the label does not match our `--label` regex.
    # Adjust the prefix if you customise `label:` below.
    if: >-
      (github.event.action != 'labeled' && github.event.action != 'unlabeled')
      || startsWith(github.event.label.name, 'vbranch:')
    runs-on: ubuntu-latest
    steps:
      - id: vbranch
        uses: pismy/git-vbranch@v1
        # Every tool flag is exposed as an input; all are optional.
        # with:
        #   label: 'vbranch:(.+)'
        #   allowed-bases: 'main,release/.*'

      - name: Use outputs
        if: steps.vbranch.outputs.VBRANCH_NAME != ''
        run: echo "deploying ${{ steps.vbranch.outputs.VBRANCH_REF }} (base ${{ steps.vbranch.outputs.VBRANCH_BASE }})"
```

Pin to a specific major / minor / exact version: `pismy/git-vbranch@v1`, `@v1.2`, `@v1.2.0`.

If you prefer to stay close to the raw binary (no composite action):

```yaml
      - uses: actions/checkout@v4

      - name: Install git-vbranch
        run: |
          curl -fsSL https://github.com/pismy/git-vbranch/releases/latest/download/git-vbranch-linux-amd64.tar.gz \
            | tar -xz -C "$RUNNER_TEMP"
          echo "$RUNNER_TEMP" >> "$GITHUB_PATH"

      - name: Checkout virtual branch
        run: git vbranch checkout
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
```

### GitLab CI

> [!WARNING]
> **Label changes on an open MR do not trigger a new pipeline in GitLab CI** (only pushes, opening, and a few other MR events do). When you add or remove a `vbranch:…` label, you must **manually re-run the target pipeline** for the MR from the GitLab UI (_Pipelines → Run pipeline_ on the MR). A scheduled pipeline (GitLab _Schedules_) is also a reasonable workaround.

```yaml
vbranch-checkout:
  image: debian:stable-slim
  before_script:
    - apt-get update && apt-get install -y --no-install-recommends curl ca-certificates git
    - curl -fsSL https://github.com/pismy/git-vbranch/releases/latest/download/git-vbranch-linux-amd64.tar.gz | tar -xz -C /usr/local/bin
  script:
    - git vbranch checkout
  artifacts:
    reports:
      dotenv: vbranch-output.env # default output dotenv file
  rules:
    - if: $CI_MERGE_REQUEST_IID
```

Downstream jobs can then use `$VBRANCH_NAME`, `$VBRANCH_REF`, etc. to pick the deployment environment, tag Docker images, etc.

### Bitbucket Pipelines

> [!WARNING]
> Bitbucket Cloud **has no PR labels** (that's why `git-vbranch` matches a `[<label>]` marker in PR titles on this provider). Bitbucket Pipelines also **does not trigger on PR title changes** — only on pushes to the source branch. Whenever you edit the `[vbranch:…]` marker on a PR, you must **manually re-run the PR's pipeline** (_Pipelines → Rerun_ on the PR's commit).

```yaml
pipelines:
  pull-requests:
    "**":
      - step:
          name: Virtual branch checkout
          script:
            - curl -fsSL https://github.com/pismy/git-vbranch/releases/latest/download/git-vbranch-linux-amd64.tar.gz | tar -xz -C /usr/local/bin
            - git vbranch checkout
          # BITBUCKET_TOKEN is automatically available
```

> Remember to include `[vbranch:<name>]` in your PR titles (e.g. `[vbranch:dev]`).

### Gitea Actions

Gitea Actions is GitHub Actions compatible and supports `labeled` / `unlabeled` the same way. Same filter pattern applies.

```yaml
name: Virtual branch checkout
on:
  pull_request:
    types: [opened, synchronize, labeled, unlabeled]

jobs:
  vbranch:
    if: >-
      (github.event.action != 'labeled' && github.event.action != 'unlabeled')
      || startsWith(github.event.label.name, 'vbranch:')
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install git-vbranch
        run: |
          curl -fsSL https://github.com/pismy/git-vbranch/releases/latest/download/git-vbranch-linux-amd64.tar.gz \
            | tar -xz -C "$RUNNER_TEMP"
          echo "$RUNNER_TEMP" >> "$GITHUB_PATH"

      - name: Checkout virtual branch
        run: git vbranch checkout
        env:
          GITEA_TOKEN: ${{ secrets.GITEA_TOKEN }}
```

### Forgejo Actions

Same capabilities and filter as Gitea / GitHub Actions.

```yaml
name: Virtual branch checkout
on:
  pull_request:
    types: [opened, synchronize, labeled, unlabeled]

jobs:
  vbranch:
    if: >-
      (github.event.action != 'labeled' && github.event.action != 'unlabeled')
      || startsWith(github.event.label.name, 'vbranch:')
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install git-vbranch
        run: |
          curl -fsSL https://github.com/pismy/git-vbranch/releases/latest/download/git-vbranch-linux-amd64.tar.gz \
            | tar -xz -C "$RUNNER_TEMP"
          echo "$RUNNER_TEMP" >> "$GITHUB_PATH"

      - name: Checkout virtual branch
        run: git vbranch checkout
        env:
          FORGEJO_TOKEN: ${{ secrets.FORGEJO_TOKEN }}
```

## Exit codes

| Code | Meaning                                                                               |
| ---- | ------------------------------------------------------------------------------------- |
| `0`  | Success: virtual branch checked out, or nothing to do (informational message printed) |
| `1`  | Error: merge conflict, API failure, missing configuration, etc.                       |

## License

MIT
