use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "git-vbranch",
    version,
    about = "Virtual branches for Git: group related PRs/MRs by label and materialize them as a single merged state.",
    long_about = "git-vbranch manages *virtual branches*: logical branches defined by a label \
        carried by multiple PRs/MRs. A virtual branch has no storage of its own — it is \
        materialized on demand by octopus-merging the concrete source branches of all PRs/MRs \
        sharing the same label value.\n\n\
        Typical use case: run `git-vbranch checkout` in CI to assemble a merged state from \
        every PR/MR tagged with a common label, in order to validate that they integrate cleanly \
        together (and optionally deploy the result to a shared environment). `git-vbranch list` \
        enumerates every virtual branch currently active in the repo.\n\n\
        Note: Bitbucket does not support PR labels natively. As a workaround, PRs are matched \
        by a `[<label>]` marker in their title (e.g. `[vbranch:dev]`)."
)]
pub struct Cli {
    #[command(flatten)]
    pub config: ProviderConfig,

    /// Label (or regex with one capture group) identifying virtual branches.
    /// With the default `vbranch:(.+)`, PRs/MRs sharing the same captured value
    /// (e.g. `vbranch:dev`) are grouped into the same virtual branch.
    #[arg(long, short, default_value = "vbranch:(.+)", env = "GIT_VBRANCH_LABEL", global = true)]
    pub label: String,

    /// Output format for virtual branch listings.
    #[arg(long, value_enum, default_value_t = OutputFormat::Tree, global = true)]
    pub format: OutputFormat,

    /// Comma-separated regex list of base branches accepted as merge targets.
    /// PRs/MRs targeting a branch that matches none of these regexes are
    /// silently ignored (a warning is logged, the command still exits 0).
    ///
    /// Defaults to the repository's default branch (fetched from the forge API).
    /// Each item is a full-match regex, e.g. `main,release/.*` allows `main`
    /// plus any `release/<something>` branch.
    #[arg(long, value_delimiter = ',', env = "GIT_VBRANCH_ALLOWED_BASES", global = true)]
    pub allowed_bases: Option<Vec<String>>,

    /// Disable ANSI colors and terminal hyperlinks in the output.
    /// Also respects the `NO_COLOR` environment variable and auto-disables
    /// when stdout is not a terminal.
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Enable verbose (debug-level) logging. Overridden by `RUST_LOG` when set.
    #[arg(long, short = 'v', global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Materialize a virtual branch by octopus-merging its member branches.
    Checkout(CheckoutArgs),
    /// List every virtual branch currently active in the repository.
    List(ListArgs),
}

/// Explicit provider hint, used in local mode when the host cannot be auto-detected.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
#[value(rename_all = "lowercase")]
pub enum ProviderHint {
    Github,
    Gitlab,
    Bitbucket,
    Gitea,
    Forgejo,
}

impl std::fmt::Display for ProviderHint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Single source of truth: the name clap derives from `ValueEnum` + `rename_all`.
        self.to_possible_value()
            .expect("every ProviderHint variant has a PossibleValue")
            .get_name()
            .fmt(f)
    }
}

/// Output format for the `list` subcommand.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
#[value(rename_all = "lowercase")]
pub enum OutputFormat {
    Tree,
    Table,
}

/// Authentication and provider-selection flags, shared by every subcommand.
/// Every field is marked `global = true` so it can be passed either before
/// or after the subcommand name.
#[derive(Args, Debug, Clone)]
pub struct ProviderConfig {
    /// Name of the git remote used (a) to fetch member branches in `checkout`
    /// and (b) to derive the provider + repo in local mode.
    #[arg(long, default_value = "origin", env = "GIT_VBRANCH_REMOTE", global = true)]
    pub git_remote: String,

    /// Provider override. By default the provider is auto-detected by looking
    /// for `github`, `gitlab`, `bitbucket`, `gitea`, `forgejo` (or `codeberg`)
    /// as a substring of the git remote hostname. Set this flag for hosts that
    /// don't follow that convention, or to force a specific provider.
    #[arg(long, value_enum, env = "GIT_VBRANCH_PROVIDER", global = true)]
    pub provider: Option<ProviderHint>,

    /// GitHub API token
    #[arg(long, env = "GITHUB_TOKEN", hide = true, global = true)]
    pub github_token: Option<String>,

    /// GitLab API token (takes precedence over CI_JOB_TOKEN)
    #[arg(long, env = "GITLAB_TOKEN", hide = true, global = true)]
    pub gitlab_token: Option<String>,

    /// Bitbucket API token
    #[arg(long, env = "BITBUCKET_TOKEN", hide = true, global = true)]
    pub bitbucket_token: Option<String>,

    /// Gitea API token
    #[arg(long, env = "GITEA_TOKEN", hide = true, global = true)]
    pub gitea_token: Option<String>,

    /// Forgejo API token
    #[arg(long, env = "FORGEJO_TOKEN", hide = true, global = true)]
    pub forgejo_token: Option<String>,
}

#[derive(Args, Debug)]
pub struct CheckoutArgs {
    /// Target to check out. Resolved as follows:
    ///
    /// - `virtual/<name>`: explicit virtual branch.
    /// - `<branch>` matching an **existing** local or remote-tracking branch that
    ///   participates in a virtual branch (as a PR/MR source): use that vbranch.
    /// - `<branch>` matching a **defined** virtual branch name: use that vbranch.
    /// - omitted: use the **current** branch as `<branch>`.
    ///
    /// The result is always materialized as a local branch `virtual/<name>`,
    /// reset to the target branch of the members and with the members octopus-merged in.
    pub branch: Option<String>,

    /// Dotenv-formatted file the `VBRANCH_*` variables are written to.
    ///
    /// Resolution order when this flag (and `GIT_VBRANCH_OUTPUT_DOTENV`) is not set:
    /// 1. `$$GITHUB_OUTPUT` when defined (GitHub / Gitea / Forgejo Actions) — appended to.
    /// 2. otherwise `vbranch-output.env` in the current directory — **truncated**
    ///    before writing, so each invocation produces a self-contained file.
    ///
    /// When explicitly set (flag or `GIT_VBRANCH_OUTPUT_DOTENV`), the file is
    /// appended to (consistent with `$$GITHUB_OUTPUT` semantics).
    #[arg(long, env = "GIT_VBRANCH_OUTPUT_DOTENV")]
    pub output_dotenv: Option<String>,

    /// Print what would be done without actually merging
    #[arg(long, env = "GIT_VBRANCH_DRY_RUN")]
    pub dry_run: bool,

    /// Pass `-f` to `git checkout -B`, discarding any uncommitted local changes
    /// that would otherwise block the branch switch.
    #[arg(long, short)]
    pub force: bool,

    /// When the current branch's PR/MR exists and targets an allowed base but
    /// does NOT carry a virtual-branch label, rebase the current branch onto
    /// `<remote>/<base>` instead of exiting with "Nothing to do".
    ///
    /// Only effective in implicit mode (no positional argument) **and** in a
    /// CI context (the universal `CI` env var is set). Outside CI the flag is
    /// ignored with a warning, to avoid silently rewriting a developer's
    /// branch history. Disabled by default.
    #[arg(long, env = "GIT_VBRANCH_FALLBACK_REBASE")]
    pub fallback_rebase: bool,
}

#[derive(Args, Debug)]
pub struct ListArgs {}
