use std::process::Command;

use crate::error::Error;

/// Parsed git remote URL pointing at a hosted repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteUrl {
    /// Host name (e.g. `github.com`, `gitlab.com`, `gitlab.example.com`).
    pub host: String,
    /// Owner/namespace (e.g. `octocat`, `group/subgroup`).
    pub owner: String,
    /// Repository name, without the trailing `.git`.
    pub repo: String,
}

impl RemoteUrl {
    /// `owner/repo` identifier usable in GitHub/Gitea/Forgejo API paths.
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

/// Resolve the current branch name via `git rev-parse --abbrev-ref HEAD`.
/// Returns `None` on detached HEAD or any failure (the caller decides whether that's fatal).
pub fn current_branch() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if name.is_empty() || name == "HEAD" {
        return None;
    }
    Some(name)
}

/// Resolve a git remote name to its configured URL via `git remote get-url`.
pub fn get_remote_url(remote: &str) -> Result<String, Error> {
    let output = Command::new("git")
        .args(["remote", "get-url", remote])
        .output()
        .map_err(|e| Error::Git(format!("failed to execute git: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Config(format!(
            "git remote '{remote}' not found (git remote get-url exited with {}): {stderr}",
            output.status
        )));
    }

    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if url.is_empty() {
        return Err(Error::Config(format!(
            "git remote '{remote}' returned an empty URL"
        )));
    }
    Ok(url)
}

/// Parse a remote URL into host/owner/repo. Supports the three common forms:
///
/// - `https://host/owner/repo(.git)`
/// - `ssh://git@host[:port]/owner/repo(.git)`
/// - `git@host:owner/repo(.git)` (scp-like)
///
/// `owner` may include slashes (GitLab subgroups).
pub fn parse_remote(url: &str) -> Result<RemoteUrl, Error> {
    let url = url.trim();

    let (host, path) = if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("ssh://"))
    {
        // Strip optional `user@`
        let rest = rest.splitn(2, '@').last().unwrap_or(rest);
        let (authority, path) = rest
            .split_once('/')
            .ok_or_else(|| Error::Config(format!("cannot parse remote URL '{url}'")))?;
        // Strip optional port
        let host = authority.split(':').next().unwrap_or(authority).to_string();
        (host, path.to_string())
    } else if let Some(rest) = url.strip_prefix("git@") {
        // scp-like: git@host:owner/repo
        let (host, path) = rest
            .split_once(':')
            .ok_or_else(|| Error::Config(format!("cannot parse remote URL '{url}'")))?;
        (host.to_string(), path.to_string())
    } else {
        return Err(Error::Config(format!(
            "unsupported remote URL scheme: '{url}'"
        )));
    };

    // Drop trailing `.git` and any trailing slash
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);

    // Split path into owner(s) and repo — repo is the last segment, owner is everything before.
    let (owner, repo) = path
        .rsplit_once('/')
        .ok_or_else(|| Error::Config(format!("remote URL '{url}' has no owner/repo path")))?;

    if owner.is_empty() || repo.is_empty() {
        return Err(Error::Config(format!(
            "remote URL '{url}' has empty owner or repo"
        )));
    }

    Ok(RemoteUrl {
        host,
        owner: owner.to_string(),
        repo: repo.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_https() {
        let r = parse_remote("https://github.com/octocat/hello-world.git").unwrap();
        assert_eq!(r.host, "github.com");
        assert_eq!(r.owner, "octocat");
        assert_eq!(r.repo, "hello-world");
    }

    #[test]
    fn parse_https_no_git_suffix() {
        let r = parse_remote("https://gitlab.com/group/project").unwrap();
        assert_eq!(r.host, "gitlab.com");
        assert_eq!(r.owner, "group");
        assert_eq!(r.repo, "project");
    }

    #[test]
    fn parse_ssh_scp_like() {
        let r = parse_remote("git@bitbucket.org:workspace/repo.git").unwrap();
        assert_eq!(r.host, "bitbucket.org");
        assert_eq!(r.owner, "workspace");
        assert_eq!(r.repo, "repo");
    }

    #[test]
    fn parse_ssh_url() {
        let r = parse_remote("ssh://git@gitea.example.com:2222/owner/repo.git").unwrap();
        assert_eq!(r.host, "gitea.example.com");
        assert_eq!(r.owner, "owner");
        assert_eq!(r.repo, "repo");
    }

    #[test]
    fn parse_gitlab_subgroup() {
        let r = parse_remote("https://gitlab.com/group/subgroup/project.git").unwrap();
        assert_eq!(r.host, "gitlab.com");
        assert_eq!(r.owner, "group/subgroup");
        assert_eq!(r.repo, "project");
        assert_eq!(r.slug(), "group/subgroup/project");
    }

    #[test]
    fn parse_https_with_user() {
        let r = parse_remote("https://user@github.com/owner/repo").unwrap();
        assert_eq!(r.host, "github.com");
        assert_eq!(r.owner, "owner");
        assert_eq!(r.repo, "repo");
    }

    #[test]
    fn parse_invalid_scheme_rejected() {
        assert!(parse_remote("ftp://x/y/z").is_err());
    }

    #[test]
    fn parse_missing_path_rejected() {
        assert!(parse_remote("https://github.com").is_err());
    }
}
