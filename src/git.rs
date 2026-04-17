use std::process::Command;

use crate::error::Error;

/// Run a git command and return its stdout. Fails if the command exits non-zero.
fn run_git(args: &[&str]) -> Result<String, Error> {
    log::debug!("running: git {}", args.join(" "));

    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|e| Error::Git(format!("failed to execute git: {e}")))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !stdout.trim().is_empty() {
        log::debug!("stdout: {stdout}");
    }
    if !stderr.trim().is_empty() {
        log::debug!("stderr: {stderr}");
    }

    if !output.status.success() {
        return Err(Error::Git(format!(
            "git {} exited with {}\n{}",
            args.join(" "),
            output.status,
            stderr
        )));
    }

    Ok(stdout)
}

/// Fetch the given branches from `remote` into the corresponding remote-tracking refs
/// (`refs/remotes/<remote>/<branch>`). Uses Git's default refspec, so local heads are
/// left untouched.
pub fn fetch_branches(remote: &str, branches: &[String]) -> Result<(), Error> {
    if branches.is_empty() {
        return Ok(());
    }

    let mut args: Vec<&str> = vec!["fetch", remote, "--no-tags"];
    for b in branches {
        args.push(b.as_str());
    }

    log::debug!("fetching from {remote}: {}", branches.join(", "));
    run_git(&args)?;
    Ok(())
}

/// Perform an octopus merge of the given refs.
pub fn octopus_merge(refs: &[String]) -> Result<(), Error> {
    let mut args: Vec<&str> = vec!["merge", "--no-edit"];
    for r in refs {
        args.push(r.as_str());
    }

    run_git(&args)?;
    Ok(())
}

/// `git rebase <onto>` — rebase the current branch onto `onto`. Fails with
/// a clean `Error::Git` if the rebase cannot complete (uncommitted changes,
/// conflicts, ...).
pub fn rebase(onto: &str) -> Result<(), Error> {
    run_git(&["rebase", onto])?;
    Ok(())
}

/// `git checkout [-f] -B <branch> --no-track <start_point>` — create or reset `branch` to
/// point at `start_point`, then switch to it. With `force=true`, local changes that would
/// block the switch are discarded. `--no-track` prevents the virtual branch from tracking
/// the remote base, which would otherwise let a stray `git pull`/`git push` interact with
/// the base branch.
pub fn checkout_b(branch: &str, start_point: &str, force: bool) -> Result<(), Error> {
    let mut args: Vec<&str> = vec!["checkout"];
    if force {
        args.push("-f");
    }
    args.push("-B");
    args.push(branch);
    args.push("--no-track");
    args.push(start_point);
    run_git(&args)?;
    Ok(())
}

/// Report whether a branch exists locally or as a remote-tracking ref.
pub fn branch_exists(branch: &str, remote: &str) -> bool {
    let local = format!("refs/heads/{branch}");
    let remote_tracking = format!("refs/remotes/{remote}/{branch}");
    git_ref_exists(&local) || git_ref_exists(&remote_tracking)
}

fn git_ref_exists(git_ref: &str) -> bool {
    Command::new("git")
        .args(["show-ref", "--verify", "--quiet", git_ref])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
