pub mod bitbucket;
pub mod gitea;
pub mod github;
pub mod gitlab;

use crate::cli::{ProviderConfig, ProviderHint};
use crate::display::{color, Style, CYAN};
use crate::error::Error;
use crate::label::LabelMatcher;
use crate::remote::{self, RemoteUrl};

/// Result of looking up open PR/MRs for a given source branch.
#[derive(Debug)]
pub enum PrMatch {
    /// No open PR/MR has this source branch.
    None,
    /// Exactly one open PR/MR; we know its base branch, number and web URL.
    One {
        base_branch: String,
        pr_number: u64,
        url: String,
    },
    /// Multiple open PR/MRs have this source branch (ambiguous).
    /// Each entry is `(pr_number, base_branch)` for diagnostic messages.
    Multiple(Vec<(u64, String)>),
}

/// A single PR/MR contributing to a virtual branch.
#[derive(Debug, Clone)]
pub struct VirtualBranchMember {
    pub pr_number: u64,
    pub source_branch: String,
    pub title: String,
    /// Web URL of the PR/MR, usable as an OSC 8 hyperlink target.
    /// Empty when the provider couldn't determine it.
    pub url: String,
}

/// A virtual branch, identified by (captured label value, base branch).
#[derive(Debug, Clone)]
pub struct VirtualBranch {
    pub name: String,
    pub base_branch: String,
    pub members: Vec<VirtualBranchMember>,
}

/// Trait implemented by each CI / forge provider.
pub trait Provider {
    /// Source branch of the checkout context (the PR/MR source in CI, the current
    /// HEAD in local mode). `None` when HEAD is detached and no CI env var gives a hint.
    fn current_branch(&self) -> Option<&str>;

    /// Name of the repository's default branch (`main`, `master`, ...). Fetched
    /// from the forge API. Called at most once per invocation, only when the
    /// user did not pass `--allowed-bases`.
    fn default_branch(&self) -> Result<String, Error>;

    /// List every virtual branch currently active in the repository.
    fn list_virtual_branches(
        &self,
        matcher: &LabelMatcher,
    ) -> Result<Vec<VirtualBranch>, Error>;

    /// Find the open PR/MR whose source branch is `source`. Used by the
    /// `--always-rebase` fallback path, after vbranch resolution failed.
    fn pr_for_source(&self, source: &str) -> Result<PrMatch, Error>;
}

/// Detect a provider. Tries CI environment first, then falls back to local detection
/// based on the git remote URL. Emits exactly one line announcing the resolved
/// provider, with `{provider}` colored.
pub fn detect_provider(config: &ProviderConfig, style: Style) -> Result<Box<dyn Provider>, Error> {
    if let Some((provider, name)) = detect_ci_provider(config)? {
        println!("CI environment detected: {}", color(style, CYAN, name));
        return Ok(provider);
    }
    detect_local_provider(config, style)
}

/// Try to build a provider from CI environment variables. Returns `Ok(None)` if
/// no supported CI environment is detected. On success, returns the provider and
/// its short name (e.g. `"forgejo"`, `"github"`) for the caller to announce.
fn detect_ci_provider(
    config: &ProviderConfig,
) -> Result<Option<(Box<dyn Provider>, &'static str)>, Error> {
    // Forgejo Actions (sets GITHUB_ACTIONS too — check first)
    if std::env::var("FORGEJO_ACTIONS").is_ok() || std::env::var("FORGEJO").is_ok() {
        let p = Box::new(gitea::GiteaProvider::from_ci(config, gitea::Flavor::Forgejo)?);
        return Ok(Some((p, "forgejo")));
    }

    // Gitea Actions (sets GITHUB_ACTIONS too — check before GitHub)
    if std::env::var("GITEA_ACTIONS").is_ok() {
        let p = Box::new(gitea::GiteaProvider::from_ci(config, gitea::Flavor::Gitea)?);
        return Ok(Some((p, "gitea")));
    }

    // GitHub Actions
    if std::env::var("GITHUB_ACTIONS").is_ok() {
        let p = Box::new(github::GitHubProvider::from_ci(config)?);
        return Ok(Some((p, "github")));
    }

    // GitLab CI
    if std::env::var("GITLAB_CI").is_ok() {
        let p = Box::new(gitlab::GitLabProvider::from_ci(config)?);
        return Ok(Some((p, "gitlab")));
    }

    // Bitbucket Pipelines
    if std::env::var("BITBUCKET_PIPELINE_UUID").is_ok() {
        let p = Box::new(bitbucket::BitbucketProvider::from_ci(config)?);
        return Ok(Some((p, "bitbucket")));
    }

    Ok(None)
}

/// Build a provider from the git remote URL (local mode), bypassing CI detection.
pub fn detect_local_provider(
    config: &ProviderConfig,
    style: Style,
) -> Result<Box<dyn Provider>, Error> {
    let url = remote::get_remote_url(&config.git_remote)?;
    let remote_url = remote::parse_remote(&url)?;

    let explicit = config.provider.is_some();
    let hint = config.provider.or_else(|| guess_provider(&remote_url));
    let hint = hint.ok_or_else(|| {
        Error::Config(format!(
            "cannot determine provider for host '{}'. Set --provider or GIT_VBRANCH_PROVIDER \
             to one of: github, gitlab, bitbucket, gitea, forgejo.",
            remote_url.host
        ))
    })?;

    let hint_str = hint.to_string();
    let colored = color(style, CYAN, &hint_str);
    if explicit {
        println!("Local mode with provider: {colored} (explicit via --provider)");
    } else {
        println!(
            "Local mode with provider: {colored} (guessed from remote '{}' {}/{}/{})",
            config.git_remote,
            remote_url.host,
            remote_url.owner,
            remote_url.repo,
        );
    }

    match hint {
        ProviderHint::Github => Ok(Box::new(github::GitHubProvider::from_remote(
            config,
            &remote_url,
        )?)),
        ProviderHint::Gitlab => Ok(Box::new(gitlab::GitLabProvider::from_remote(
            config,
            &remote_url,
        )?)),
        ProviderHint::Bitbucket => Ok(Box::new(bitbucket::BitbucketProvider::from_remote(
            config,
            &remote_url,
        )?)),
        ProviderHint::Gitea => Ok(Box::new(gitea::GiteaProvider::from_remote(
            config,
            &remote_url,
            gitea::Flavor::Gitea,
        )?)),
        ProviderHint::Forgejo => Ok(Box::new(gitea::GiteaProvider::from_remote(
            config,
            &remote_url,
            gitea::Flavor::Forgejo,
        )?)),
    }
}

/// Auto-detect a provider by asking each implementation whether it claims the
/// given remote URL (`matches_host`). Picks up hosted offerings (`github.com`,
/// `gitlab.com`, `bitbucket.org`, `codeberg.org`, ...) and self-hosted instances
/// whose hostname follows the common `{product}.company.com` convention.
///
/// Users with exotic hostnames (or ambiguous ones matching several providers)
/// must pass `--provider` explicitly. The ordering below determines the winner
/// when multiple providers match.
fn guess_provider(url: &RemoteUrl) -> Option<ProviderHint> {
    if github::GitHubProvider::matches_host(url) {
        return Some(ProviderHint::Github);
    }
    if gitlab::GitLabProvider::matches_host(url) {
        return Some(ProviderHint::Gitlab);
    }
    if bitbucket::BitbucketProvider::matches_host(url) {
        return Some(ProviderHint::Bitbucket);
    }
    if gitea::GiteaProvider::matches_host(url, gitea::Flavor::Forgejo) {
        return Some(ProviderHint::Forgejo);
    }
    if gitea::GiteaProvider::matches_host(url, gitea::Flavor::Gitea) {
        return Some(ProviderHint::Gitea);
    }
    None
}
