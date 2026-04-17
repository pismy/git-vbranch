use std::collections::BTreeMap;

use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::Deserialize;

use crate::ci::{PrMatch, Provider, VirtualBranch, VirtualBranchMember};
use crate::cli::ProviderConfig;
use crate::error::Error;
use crate::label::LabelMatcher;
use crate::remote::{self, RemoteUrl};

pub struct GitHubProvider {
    client: Client,
    api_url: String,
    repo: String,
    token: String,
    current_branch: Option<String>,
    /// Web URL base, e.g. `https://github.com`. Used to build PR URLs.
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
    head: PrHead,
    base: PrBase,
    labels: Vec<PrLabel>,
}

#[derive(Deserialize)]
struct PrHead {
    #[serde(rename = "ref")]
    ref_name: String,
}

#[derive(Deserialize)]
struct PrBase {
    #[serde(rename = "ref")]
    ref_name: String,
}

#[derive(Deserialize)]
struct PrLabel {
    name: String,
}

/// Return the first matching target key found among a PR's labels.
fn pr_target(pr: &PullRequest, matcher: &LabelMatcher) -> Option<String> {
    pr.labels
        .iter()
        .find_map(|l| matcher.match_target(&l.name))
}

impl GitHubProvider {
    /// Does the remote URL look like a GitHub instance?
    pub fn matches_host(url: &RemoteUrl) -> bool {
        url.host.contains("github")
    }

    pub fn from_ci(config: &ProviderConfig) -> Result<Self, Error> {
        let token = config
            .github_token
            .clone()
            .ok_or_else(|| Error::Config("GITHUB_TOKEN is required in GitHub Actions".into()))?;

        let repo = std::env::var("GITHUB_REPOSITORY").map_err(|_| {
            Error::CiDetection("GITHUB_REPOSITORY environment variable not set".into())
        })?;

        let api_url = std::env::var("GITHUB_API_URL")
            .unwrap_or_else(|_| "https://api.github.com".into());

        let web_url = std::env::var("GITHUB_SERVER_URL")
            .unwrap_or_else(|_| "https://github.com".into());

        let current_branch = Some(Self::resolve_ci_branch()?);
        log::debug!("current branch: {}", current_branch.as_deref().unwrap_or("?"));

        Ok(Self {
            client: Client::new(),
            api_url,
            repo,
            token,
            current_branch,
            web_url,
        })
    }

    pub fn from_remote(config: &ProviderConfig, url: &RemoteUrl) -> Result<Self, Error> {
        let token = config
            .github_token
            .clone()
            .ok_or_else(|| Error::Config("GITHUB_TOKEN is required for GitHub".into()))?;

        let (api_url, web_url) = if url.host == "github.com" {
            (
                "https://api.github.com".to_string(),
                "https://github.com".to_string(),
            )
        } else {
            // GitHub Enterprise server
            (
                format!("https://{}/api/v3", url.host),
                format!("https://{}", url.host),
            )
        };

        Ok(Self {
            client: Client::new(),
            api_url,
            repo: url.slug(),
            token,
            current_branch: remote::current_branch(),
            web_url,
        })
    }

    fn pr_url(&self, number: u64) -> String {
        format!("{}/{}/pull/{number}", self.web_url, self.repo)
    }

    fn resolve_ci_branch() -> Result<String, Error> {
        // In pull_request events, GITHUB_HEAD_REF is the source branch
        if let Ok(head_ref) = std::env::var("GITHUB_HEAD_REF") {
            if !head_ref.is_empty() {
                return Ok(head_ref);
            }
        }
        // Fallback: GITHUB_REF (e.g. refs/heads/my-branch)
        let github_ref = std::env::var("GITHUB_REF")
            .map_err(|_| Error::CiDetection("neither GITHUB_HEAD_REF nor GITHUB_REF is set".into()))?;
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
        let resp = self
            .client
            .get(url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(ACCEPT, "application/vnd.github+json")
            .header(USER_AGENT, "git-vbranch")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(Error::Api(format!(
                "GitHub API returned {status}: {body}"
            )));
        }
        Ok(resp)
    }

    /// List open pull requests with pagination. `query` is appended as additional filters.
    fn list_pulls(&self, query: &str) -> Result<Vec<PullRequest>, Error> {
        let mut all_prs = Vec::new();
        let mut page = 1u32;
        let separator = if query.is_empty() { "" } else { "&" };

        loop {
            let url = format!(
                "{}/repos/{}/pulls?state=open&per_page=100&page={page}{separator}{query}",
                self.api_url, self.repo
            );
            log::debug!("GET {url}");
            let resp = self.get(&url)?;

            let prs: Vec<PullRequest> = resp.json()?;
            let count = prs.len();
            all_prs.extend(prs);

            if count < 100 {
                break;
            }
            page += 1;
        }

        Ok(all_prs)
    }

}

impl Provider for GitHubProvider {
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
        let prs = self.list_pulls("")?;
        Ok(self.group_into_vbranches(&prs, matcher))
    }

    fn pr_for_source(&self, source: &str) -> Result<PrMatch, Error> {
        let owner = self.repo.split('/').next().unwrap_or_default();
        let query = format!("head={owner}:{source}");
        let prs = self.list_pulls(&query)?;
        let matches: Vec<(u64, String)> = prs
            .iter()
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

impl GitHubProvider {
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
