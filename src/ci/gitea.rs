use std::collections::BTreeMap;

use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, USER_AGENT};
use serde::Deserialize;

use crate::ci::{PrMatch, Provider, VirtualBranch, VirtualBranchMember};
use crate::cli::ProviderConfig;
use crate::error::Error;
use crate::label::LabelMatcher;
use crate::remote::{self, RemoteUrl};

/// Whether we're running in Gitea Actions or Forgejo Actions.
#[derive(Debug, Clone, Copy)]
pub enum Flavor {
    Gitea,
    Forgejo,
}

impl std::fmt::Display for Flavor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Flavor::Gitea => write!(f, "Gitea"),
            Flavor::Forgejo => write!(f, "Forgejo"),
        }
    }
}

/// Provider for Gitea Actions and Forgejo Actions.
///
/// Both platforms share the same API (Forgejo is a Gitea fork).
/// They set `GITHUB_*` environment variables for GitHub Actions compatibility,
/// but are distinguished by `GITEA_ACTIONS` or `FORGEJO_ACTIONS` env vars.
pub struct GiteaProvider {
    flavor: Flavor,
    client: Client,
    api_url: String,
    repo: String,
    token: String,
    current_branch: Option<String>,
    /// Web URL base, e.g. `https://gitea.example.com`. Used to build PR URLs.
    web_url: String,
}

#[derive(Deserialize)]
struct Repository {
    default_branch: String,
}

#[derive(Deserialize)]
struct PullRequest {
    number: u64,
    title: String,
    head: PrRef,
    base: PrRef,
    labels: Vec<PrLabel>,
}

#[derive(Deserialize)]
struct PrRef {
    #[serde(rename = "ref")]
    ref_name: String,
}

#[derive(Deserialize)]
struct PrLabel {
    name: String,
}

fn pr_target(pr: &PullRequest, matcher: &LabelMatcher) -> Option<String> {
    pr.labels
        .iter()
        .find_map(|l| matcher.match_target(&l.name))
}

impl GiteaProvider {
    /// Does the remote URL look like a Gitea or Forgejo instance (depending on `flavor`)?
    /// Forgejo also recognizes `codeberg`, the well-known public Forgejo instance.
    pub fn matches_host(url: &RemoteUrl, flavor: Flavor) -> bool {
        match flavor {
            Flavor::Gitea => url.host.contains("gitea"),
            Flavor::Forgejo => url.host.contains("forgejo") || url.host.contains("codeberg"),
        }
    }

    pub fn from_ci(config: &ProviderConfig, flavor: Flavor) -> Result<Self, Error> {
        let token = Self::resolve_token(config, flavor)?;

        let repo = std::env::var("GITHUB_REPOSITORY").map_err(|_| {
            Error::CiDetection("GITHUB_REPOSITORY environment variable not set".into())
        })?;

        let api_url = std::env::var("GITHUB_API_URL").map_err(|_| {
            Error::CiDetection("GITHUB_API_URL environment variable not set".into())
        })?;

        let web_url = std::env::var("GITHUB_SERVER_URL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                // Fallback: strip the `/api/v1` suffix from api_url
                api_url
                    .strip_suffix("/api/v1")
                    .unwrap_or(&api_url)
                    .to_string()
            });

        let current_branch = Some(Self::resolve_ci_branch()?);
        log::debug!("current branch: {}", current_branch.as_deref().unwrap_or("?"));

        Ok(Self {
            flavor,
            client: Client::new(),
            api_url,
            repo,
            token,
            current_branch,
            web_url,
        })
    }

    pub fn from_remote(
        config: &ProviderConfig,
        url: &RemoteUrl,
        flavor: Flavor,
    ) -> Result<Self, Error> {
        let token = Self::resolve_token(config, flavor)?;

        // Gitea/Forgejo API is at https://<host>/api/v1
        let api_url = format!("https://{}/api/v1", url.host);
        let web_url = format!("https://{}", url.host);

        Ok(Self {
            flavor,
            client: Client::new(),
            api_url,
            repo: url.slug(),
            token,
            current_branch: remote::current_branch(),
            web_url,
        })
    }

    fn pr_url(&self, number: u64) -> String {
        format!("{}/{}/pulls/{number}", self.web_url, self.repo)
    }

    fn resolve_token(config: &ProviderConfig, flavor: Flavor) -> Result<String, Error> {
        match flavor {
            Flavor::Gitea => config.gitea_token.clone(),
            Flavor::Forgejo => config.forgejo_token.clone(),
        }
        .or_else(|| config.github_token.clone())
        .ok_or_else(|| {
            let var = match flavor {
                Flavor::Gitea => "GITEA_TOKEN",
                Flavor::Forgejo => "FORGEJO_TOKEN",
            };
            Error::Config(format!("{var} (or GITHUB_TOKEN) is required for {flavor}"))
        })
    }

    fn resolve_ci_branch() -> Result<String, Error> {
        if let Ok(head_ref) = std::env::var("GITHUB_HEAD_REF") {
            if !head_ref.is_empty() {
                return Ok(head_ref);
            }
        }
        let github_ref = std::env::var("GITHUB_REF").map_err(|_| {
            Error::CiDetection("neither GITHUB_HEAD_REF nor GITHUB_REF is set".into())
        })?;
        github_ref
            .strip_prefix("refs/heads/")
            .map(|s| s.to_string())
            .ok_or_else(|| {
                Error::CiDetection(format!(
                    "GITHUB_REF '{github_ref}' does not look like a branch ref"
                ))
            })
    }

    fn get(&self, url: &str) -> Result<reqwest::blocking::Response, Error> {
        // Gitea/Forgejo use `token <value>` format for Authorization
        let resp = self
            .client
            .get(url)
            .header(AUTHORIZATION, format!("token {}", self.token))
            .header(USER_AGENT, "git-vbranch")
            .send()?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(Error::Api(format!(
                "{} API returned {status}: {body}",
                self.flavor
            )));
        }
        Ok(resp)
    }

    fn list_pulls(&self) -> Result<Vec<PullRequest>, Error> {
        let mut all_prs = Vec::new();
        let mut page = 1u32;

        loop {
            let url = format!(
                "{}/repos/{}/pulls?state=open&limit=50&page={page}",
                self.api_url, self.repo
            );
            log::debug!("GET {url}");
            let resp = self.get(&url)?;

            let prs: Vec<PullRequest> = resp.json()?;
            let count = prs.len();
            all_prs.extend(prs);

            if count < 50 {
                break;
            }
            page += 1;
        }

        Ok(all_prs)
    }

}

impl Provider for GiteaProvider {
    fn current_branch(&self) -> Option<&str> {
        self.current_branch.as_deref()
    }

    fn default_branch(&self) -> Result<String, Error> {
        let url = format!("{}/repos/{}", self.api_url, self.repo);
        log::debug!("GET {url}");
        let resp = self.get(&url)?;
        let repo: Repository = resp.json()?;
        Ok(repo.default_branch)
    }

    fn list_virtual_branches(
        &self,
        matcher: &LabelMatcher,
    ) -> Result<Vec<VirtualBranch>, Error> {
        let prs = self.list_pulls()?;
        Ok(self.group_into_vbranches(&prs, matcher))
    }

    fn pr_for_source(&self, source: &str) -> Result<PrMatch, Error> {
        // Gitea's /pulls endpoint has no server-side source filter; list all and filter locally.
        let prs = self.list_pulls()?;
        let matches: Vec<(u64, String)> = prs
            .iter()
            .filter(|pr| pr.head.ref_name == source)
            .map(|pr| (pr.number, pr.base.ref_name.clone()))
            .collect();
        Ok(match matches.len() {
            0 => PrMatch::None,
            1 => PrMatch::One {
                pr_number: matches[0].0,
                base_branch: matches[0].1.clone(),
                url: self.pr_url(matches[0].0),
            },
            _ => PrMatch::Multiple(matches),
        })
    }
}

impl GiteaProvider {
    fn group_into_vbranches(
        &self,
        prs: &[PullRequest],
        matcher: &LabelMatcher,
    ) -> Vec<VirtualBranch> {
        let mut groups: BTreeMap<(String, String), Vec<VirtualBranchMember>> = BTreeMap::new();
        for pr in prs {
            if let Some(target) = pr_target(pr, matcher) {
                let key = (target, pr.base.ref_name.clone());
                groups
                    .entry(key)
                    .or_default()
                    .push(VirtualBranchMember {
                        pr_number: pr.number,
                        source_branch: pr.head.ref_name.clone(),
                        title: pr.title.clone(),
                        url: self.pr_url(pr.number),
                    });
            }
        }
        groups
            .into_iter()
            .map(|((name, base_branch), members)| VirtualBranch {
                name,
                base_branch,
                members,
            })
            .collect()
    }
}
