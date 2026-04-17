use std::collections::BTreeMap;

use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, USER_AGENT};
use serde::Deserialize;

use crate::ci::{PrMatch, Provider, VirtualBranch, VirtualBranchMember};
use crate::cli::ProviderConfig;
use crate::error::Error;
use crate::label::LabelMatcher;
use crate::remote::{self, RemoteUrl};

/// Bitbucket Cloud does not support labels on Pull Requests.
/// As a workaround, this provider matches PRs whose title contains `[<label>]`.
/// For example, with the default regex label `vbranch:(.+)`, a PR title must contain
/// `[vbranch:<name>]` (e.g. `[vbranch:dev]`), and PRs are grouped by the captured value.
pub struct BitbucketProvider {
    client: Client,
    api_url: String,
    workspace: String,
    repo_slug: String,
    token: String,
    current_branch: Option<String>,
}

impl BitbucketProvider {
    /// Does the remote URL look like a Bitbucket instance?
    pub fn matches_host(url: &RemoteUrl) -> bool {
        url.host.contains("bitbucket")
    }

    fn pr_url(&self, id: u64) -> String {
        format!(
            "https://bitbucket.org/{}/{}/pull-requests/{id}",
            self.workspace, self.repo_slug
        )
    }
}

#[derive(Deserialize)]
struct PaginatedResponse<T> {
    values: Vec<T>,
    next: Option<String>,
}

#[derive(Deserialize)]
struct Repository {
    mainbranch: MainBranch,
}

#[derive(Deserialize)]
struct MainBranch {
    name: String,
}

#[derive(Deserialize)]
struct PullRequest {
    id: u64,
    title: String,
    source: PrEndpoint,
    destination: PrEndpoint,
}

#[derive(Deserialize)]
struct PrEndpoint {
    branch: PrBranch,
}

#[derive(Deserialize)]
struct PrBranch {
    name: String,
}

impl BitbucketProvider {
    pub fn from_ci(config: &ProviderConfig) -> Result<Self, Error> {
        let token = config.bitbucket_token.clone().ok_or_else(|| {
            Error::Config("BITBUCKET_TOKEN is required in Bitbucket Pipelines".into())
        })?;

        let workspace = std::env::var("BITBUCKET_WORKSPACE").map_err(|_| {
            Error::CiDetection("BITBUCKET_WORKSPACE environment variable not set".into())
        })?;

        let repo_slug = std::env::var("BITBUCKET_REPO_SLUG").map_err(|_| {
            Error::CiDetection("BITBUCKET_REPO_SLUG environment variable not set".into())
        })?;

        let api_url = std::env::var("BITBUCKET_API_URL")
            .unwrap_or_else(|_| "https://api.bitbucket.org/2.0".into());

        let current_branch = Some(std::env::var("BITBUCKET_BRANCH").map_err(|_| {
            Error::CiDetection("BITBUCKET_BRANCH environment variable not set".into())
        })?);
        log::debug!("current branch: {}", current_branch.as_deref().unwrap_or("?"));

        Ok(Self {
            client: Client::new(),
            api_url,
            workspace,
            repo_slug,
            token,
            current_branch,
        })
    }

    pub fn from_remote(config: &ProviderConfig, url: &RemoteUrl) -> Result<Self, Error> {
        let token = config
            .bitbucket_token
            .clone()
            .ok_or_else(|| Error::Config("BITBUCKET_TOKEN is required for Bitbucket".into()))?;

        Ok(Self {
            client: Client::new(),
            api_url: "https://api.bitbucket.org/2.0".into(),
            workspace: url.owner.clone(),
            repo_slug: url.repo.clone(),
            token,
            current_branch: remote::current_branch(),
        })
    }

    fn get(&self, url: &str) -> Result<reqwest::blocking::Response, Error> {
        let resp = self
            .client
            .get(url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, "git-vbranch")
            .send()?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(Error::Api(format!(
                "Bitbucket API returned {status}: {body}"
            )));
        }
        Ok(resp)
    }

    /// List open pull requests, optionally filtered by a `q=` expression.
    fn list_pull_requests(&self, query: Option<&str>) -> Result<Vec<PullRequest>, Error> {
        let mut all_prs = Vec::new();
        let mut url = match query {
            Some(q) => format!(
                "{}/repositories/{}/{}/pullrequests?state=OPEN&pagelen=50&q={q}",
                self.api_url, self.workspace, self.repo_slug
            ),
            None => format!(
                "{}/repositories/{}/{}/pullrequests?state=OPEN&pagelen=50",
                self.api_url, self.workspace, self.repo_slug
            ),
        };

        loop {
            log::debug!("GET {url}");
            let resp = self.get(&url)?;
            let page: PaginatedResponse<PullRequest> = resp.json()?;

            all_prs.extend(page.values);

            match page.next {
                Some(next_url) => url = next_url,
                None => break,
            }
        }

        Ok(all_prs)
    }
}

impl Provider for BitbucketProvider {
    fn current_branch(&self) -> Option<&str> {
        self.current_branch.as_deref()
    }

    fn default_branch(&self) -> Result<String, Error> {
        let url = format!(
            "{}/repositories/{}/{}",
            self.api_url, self.workspace, self.repo_slug
        );
        log::debug!("GET {url}");
        let resp = self.get(&url)?;
        let repo: Repository = resp.json()?;
        Ok(repo.mainbranch.name)
    }

    fn list_virtual_branches(
        &self,
        matcher: &LabelMatcher,
    ) -> Result<Vec<VirtualBranch>, Error> {
        let prs = self.list_pull_requests(None)?;
        Ok(self.group_into_vbranches(&prs, matcher))
    }

    fn pr_for_source(&self, source: &str) -> Result<PrMatch, Error> {
        let query = format!("source.branch.name=\"{source}\"");
        let prs = self.list_pull_requests(Some(&query))?;
        let matches: Vec<(u64, String)> = prs
            .iter()
            .map(|pr| (pr.id, pr.destination.branch.name.clone()))
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

impl BitbucketProvider {
    fn group_into_vbranches(
        &self,
        prs: &[PullRequest],
        matcher: &LabelMatcher,
    ) -> Vec<VirtualBranch> {
        let mut groups: BTreeMap<(String, String), Vec<VirtualBranchMember>> = BTreeMap::new();
        for pr in prs {
            if let Some(target) = matcher.match_target_in_title(&pr.title) {
                let key = (target, pr.destination.branch.name.clone());
                groups
                    .entry(key)
                    .or_default()
                    .push(VirtualBranchMember {
                        pr_number: pr.id,
                        source_branch: pr.source.branch.name.clone(),
                        title: pr.title.clone(),
                        url: self.pr_url(pr.id),
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
