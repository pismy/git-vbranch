use std::collections::BTreeMap;

use reqwest::blocking::Client;
use reqwest::header::USER_AGENT;
use serde::Deserialize;

use crate::ci::{PrMatch, Provider, VirtualBranch, VirtualBranchMember};
use crate::cli::ProviderConfig;
use crate::error::Error;
use crate::label::LabelMatcher;
use crate::remote::{self, RemoteUrl};

pub struct GitLabProvider {
    client: Client,
    api_url: String,
    project_id: String,
    token: String,
    token_header: &'static str,
    current_branch: Option<String>,
    /// Web URL base, e.g. `https://gitlab.com`. Used to build MR URLs.
    web_url: String,
    /// Project path (`group/project`) used for web URLs.
    project_path: String,
}

#[derive(Deserialize)]
struct Project {
    default_branch: String,
}

#[derive(Deserialize)]
struct MergeRequest {
    iid: u64,
    title: String,
    source_branch: String,
    target_branch: String,
    labels: Vec<String>,
}

fn mr_target(mr: &MergeRequest, matcher: &LabelMatcher) -> Option<String> {
    mr.labels.iter().find_map(|l| matcher.match_target(l))
}

impl GitLabProvider {
    /// Does the remote URL look like a GitLab instance?
    pub fn matches_host(url: &RemoteUrl) -> bool {
        url.host.contains("gitlab")
    }

    pub fn from_ci(config: &ProviderConfig) -> Result<Self, Error> {
        let project_id = std::env::var("CI_PROJECT_ID")
            .map_err(|_| Error::CiDetection("CI_PROJECT_ID environment variable not set".into()))?;

        let api_url = std::env::var("CI_API_V4_URL")
            .unwrap_or_else(|_| "https://gitlab.com/api/v4".into());

        let web_url = std::env::var("CI_SERVER_URL")
            .unwrap_or_else(|_| "https://gitlab.com".into());
        let project_path = std::env::var("CI_PROJECT_PATH").unwrap_or_default();

        let (token, token_header) = Self::resolve_token(config)?;

        let current_branch = Some(Self::resolve_ci_branch()?);
        log::debug!("current branch: {}", current_branch.as_deref().unwrap_or("?"));

        Ok(Self {
            client: Client::new(),
            api_url,
            project_id,
            token,
            token_header,
            current_branch,
            web_url,
            project_path,
        })
    }

    pub fn from_remote(config: &ProviderConfig, url: &RemoteUrl) -> Result<Self, Error> {
        let (api_url, web_url) = if url.host == "gitlab.com" {
            (
                "https://gitlab.com/api/v4".to_string(),
                "https://gitlab.com".to_string(),
            )
        } else {
            (
                format!("https://{}/api/v4", url.host),
                format!("https://{}", url.host),
            )
        };

        let (token, token_header) = Self::resolve_token(config)?;

        let project_path = format!("{}/{}", url.owner, url.repo);
        // GitLab accepts a URL-encoded `namespace/project` as project id.
        let project_id = url_encode_path(&project_path);

        Ok(Self {
            client: Client::new(),
            api_url,
            project_id,
            token,
            token_header,
            current_branch: remote::current_branch(),
            web_url,
            project_path,
        })
    }

    fn mr_url(&self, iid: u64) -> String {
        if self.project_path.is_empty() {
            String::new()
        } else {
            format!(
                "{}/{}/-/merge_requests/{iid}",
                self.web_url, self.project_path
            )
        }
    }

    fn resolve_token(config: &ProviderConfig) -> Result<(String, &'static str), Error> {
        if let Some(ref t) = config.gitlab_token {
            Ok((t.clone(), "PRIVATE-TOKEN"))
        } else if let Ok(t) = std::env::var("CI_JOB_TOKEN") {
            Ok((t, "JOB-TOKEN"))
        } else {
            Err(Error::Config(
                "no GitLab token found: set GITLAB_TOKEN or ensure CI_JOB_TOKEN is available".into(),
            ))
        }
    }

    fn resolve_ci_branch() -> Result<String, Error> {
        if let Ok(branch) = std::env::var("CI_MERGE_REQUEST_SOURCE_BRANCH_NAME") {
            if !branch.is_empty() {
                return Ok(branch);
            }
        }
        std::env::var("CI_COMMIT_REF_NAME").map_err(|_| {
            Error::CiDetection(
                "neither CI_MERGE_REQUEST_SOURCE_BRANCH_NAME nor CI_COMMIT_REF_NAME is set".into(),
            )
        })
    }

    fn get(&self, url: &str) -> Result<reqwest::blocking::Response, Error> {
        let resp = self
            .client
            .get(url)
            .header(self.token_header, &self.token)
            .header(USER_AGENT, "git-vbranch")
            .send()?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(Error::Api(format!(
                "GitLab API returned {status}: {body}"
            )));
        }
        Ok(resp)
    }

    fn list_merge_requests(&self, query: &str) -> Result<Vec<MergeRequest>, Error> {
        let mut all_mrs = Vec::new();
        let mut page = 1u32;
        let separator = if query.is_empty() { "" } else { "&" };

        loop {
            let url = format!(
                "{}/projects/{}/merge_requests?state=opened&per_page=100&page={page}{separator}{query}",
                self.api_url, self.project_id
            );
            log::debug!("GET {url}");
            let resp = self.get(&url)?;

            let mrs: Vec<MergeRequest> = resp.json()?;
            let count = mrs.len();
            all_mrs.extend(mrs);

            if count < 100 {
                break;
            }
            page += 1;
        }

        Ok(all_mrs)
    }

}

impl Provider for GitLabProvider {
    fn current_branch(&self) -> Option<&str> {
        self.current_branch.as_deref()
    }

    fn default_branch(&self) -> Result<String, Error> {
        let url = format!("{}/projects/{}", self.api_url, self.project_id);
        log::debug!("GET {url}");
        let resp = self.get(&url)?;
        let project: Project = resp.json()?;
        Ok(project.default_branch)
    }

    fn list_virtual_branches(
        &self,
        matcher: &LabelMatcher,
    ) -> Result<Vec<VirtualBranch>, Error> {
        let mrs = self.list_merge_requests("")?;
        Ok(self.group_into_vbranches(&mrs, matcher))
    }

    fn pr_for_source(&self, source: &str) -> Result<PrMatch, Error> {
        let query = format!("source_branch={source}");
        let mrs = self.list_merge_requests(&query)?;
        let matches: Vec<(u64, String)> = mrs
            .iter()
            .map(|mr| (mr.iid, mr.target_branch.clone()))
            .collect();
        Ok(match matches.len() {
            0 => PrMatch::None,
            1 => PrMatch::One {
                pr_number: matches[0].0,
                base_branch: matches[0].1.clone(),
                url: self.mr_url(matches[0].0),
            },
            _ => PrMatch::Multiple(matches),
        })
    }
}

impl GitLabProvider {
    fn group_into_vbranches(
        &self,
        mrs: &[MergeRequest],
        matcher: &LabelMatcher,
    ) -> Vec<VirtualBranch> {
        let mut groups: BTreeMap<(String, String), Vec<VirtualBranchMember>> = BTreeMap::new();
        for mr in mrs {
            if let Some(target) = mr_target(mr, matcher) {
                let key = (target, mr.target_branch.clone());
                groups
                    .entry(key)
                    .or_default()
                    .push(VirtualBranchMember {
                        pr_number: mr.iid,
                        source_branch: mr.source_branch.clone(),
                        title: mr.title.clone(),
                        url: self.mr_url(mr.iid),
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

/// URL-encode a path that may contain `/`. Equivalent to replacing unreserved chars.
fn url_encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}
