# git-vbranch

Rust CLI that implements **virtual branches** on top of Git: a virtual branch is a logical branch defined by a shared label on several PRs/MRs, materialized on demand by octopus-merging the concrete source branches into a local `virtual/<name>` branch.

The binary is named `git-vbranch`, which lets Git dispatch it as a subcommand (`git vbranch checkout`) whenever it sits on the user's `$PATH`. Keep the binary name intact.

## Build & test

- `cargo build --release` — release binary at `target/release/git-vbranch`
- `cargo test` — unit tests (`src/label.rs`, `src/remote.rs`)
- No integration tests against real forge APIs.

## Subcommands

- `git vbranch checkout [BRANCH]` — always produces a local `virtual/<name>` branch via:
  1. `git fetch <remote> <base> <members...>` (default refspec, updates remote-tracking refs)
  2. `git checkout [-f] -B virtual/<name> --no-track <remote>/<base>`
  3. `git merge <remote>/<member1> <remote>/<member2> ...`
- `git vbranch list` — enumerate active virtual branches.

Both support `--format tree|table` (global) and emit colored output with OSC 8 hyperlinks on PR numbers.

## Architecture

- `src/cli.rs` — clap definitions. `ProviderConfig` is flattened on `Cli` with `#[arg(global = true)]` on every field (tokens, `--git-remote`, `--provider`). `--label`, `--format`, `--no-color` are likewise global.
- `src/ci/mod.rs` — `Provider` trait (`current_branch()`, `list_virtual_branches()`), CI env detection, local fallback via `detect_local_provider`. `guess_provider(&RemoteUrl)` delegates to each provider's `matches_host`.
- `src/ci/{github,gitlab,bitbucket,gitea}.rs` — implementations. Each exposes:
  - `matches_host(url) -> bool` (static) — auto-detection predicate.
  - `from_ci(config[, flavor])` — constructor from CI env vars.
  - `from_remote(config, url[, flavor])` — constructor from a parsed git remote URL.
- `src/display.rs` — ANSI colors + OSC 8 hyperlinks, gated by `Style::new(no_color_flag)`.
- `src/git.rs` — thin wrappers around the `git` CLI (`fetch_branches`, `octopus_merge`, `checkout_b`, `branch_exists`).
- `src/label.rs` — `LabelMatcher`, static or regex-with-one-capture.
- `src/remote.rs` — parses git remote URLs (HTTPS/SSH/scp-like), resolves current branch via `git rev-parse`.
- `src/main.rs` — wires everything, implements `resolve_vbranch`, `materialize_vbranch`, `print_tree`/`print_table`.

## Key design choices

- **Uniform `checkout` semantics**: regardless of the arg form (`virtual/<name>`, a real branch, a vbranch name, or no arg → current branch), the output is always `virtual/<name>` built fresh from `<remote>/<base>` + octopus merge of `<remote>/<members...>`.
- **Remote-tracking refs are the source of truth**: `fetch_branches` uses git's default refspec; all merges target `<remote>/<branch>`, never local branches.
- **Arg resolution order** in `resolve_vbranch` (`src/main.rs`):
  1. `virtual/<name>` prefix → explicit vbranch (error if undefined)
  2. existing local/remote-tracking branch participating in a vbranch → that vbranch
  3. name matching a defined vbranch → that vbranch
  4. else → error (but no-arg + current branch not participating → exit 0 with "Nothing to do", useful in CI)
- **Provider auto-detection**: single source of truth is each provider's `matches_host`. `--provider` / `GIT_VBRANCH_PROVIDER` bypasses detection entirely.
- **List is primary**: `checkout` reuses `list_virtual_branches` + filters; there is no separate "find current PR" fast path.
- **`--force` / `-f`**: passes `-f` to `git checkout -B`, discarding uncommitted local changes.
- **`--fallback-rebase`** (`GIT_VBRANCH_FALLBACK_REBASE`): implicit-mode-only fallback, **CI only** (check `std::env::var_os("CI")`). When the current branch's PR exists and targets an allowed base but is not a member of any vbranch, rebase onto `<remote>/<base>` instead of exiting 0. Uses `Provider::pr_for_source`. Outside CI, the flag is warn-and-skipped (prevents silent rewrite of developer history). Multiple open PRs for the same source → error (ambiguous). Rebase conflicts → error (user must fix). Output: `VBRANCH_REF` = current branch name, `VBRANCH_BASE` = the rebased-onto base; other `VBRANCH_*` vars are empty.

## Conventions

- Everything English in code, comments, logs.
- Env vars follow `GIT_VBRANCH_*` (label, remote, provider, output-dotenv, dry-run, allowed-bases).
- `--allowed-bases` (global): comma-separated full-match regex list of accepted base branches. When unset, defaults to the repo's default branch via `Provider::default_branch()` (one extra API call). PRs targeting non-allowed bases are dropped with a `log::warn!`; the command still exits 0.
- On successful `checkout`, five `VBRANCH_*` lines are emitted (see `write_vbranch_output` in `src/main.rs`): `VBRANCH_NAME`, `VBRANCH_REF`, `VBRANCH_BASE`, `VBRANCH_MEMBER_PR_IDS`, `VBRANCH_MEMBER_REFS`. Always printed to stdout, and written to a single dotenv file resolved as:
  1. `--output-dotenv` / `GIT_VBRANCH_OUTPUT_DOTENV` (appended), if set.
  2. else `$GITHUB_OUTPUT` (appended) — GH/Gitea/Forgejo Actions runners always set it; feeds the `outputs:` contract of the composite action.
  3. else `vbranch-output.env` (truncated on every run).
- `--no-color` disables both colors and OSC 8 hyperlinks. Auto-disabled when stdout is not a TTY or when `NO_COLOR` is set.
- Bitbucket has no PR labels, so its provider matches `[<label>]` markers in PR titles (e.g. `[vbranch:dev]`).

## Companion GitHub Action

`action.yml` at the repo root defines a composite GitHub Action (`pismy/git-vbranch@v1`) that wraps the tool: `actions/checkout`, install-from-release, then `git vbranch checkout` with all tool flags exposed as inputs. Outputs mirror the `VBRANCH_*` dotenv variables.

Kept in-repo on purpose: atomic versioning with the tool. On each release, `.releaserc.json`'s `@semantic-release/exec` `successCmd` runs `.github/scripts/update-floating-tags.sh` to force-update `vX` and `vX.Y` tags to the freshly-tagged `vX.Y.Z` commit, so `uses: pismy/git-vbranch@v1` always resolves to the latest.

## Before editing

Before adding/changing a behavior, check whether it belongs:
- at the **provider** level (API specifics, URL construction, auth)
- in `ci/mod.rs` (detection, dispatch)
- in `main.rs` (orchestration, display)
- in `git.rs` (git CLI wrapping)

Changes that touch the `Provider` trait must be replicated across all four providers.
