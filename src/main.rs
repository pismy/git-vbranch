mod ci;
mod cli;
mod display;
mod error;
mod git;
mod label;
mod remote;

use std::io::Write;

use clap::Parser;

use ci::{Provider, VirtualBranch};
use cli::{CheckoutArgs, Cli, Command, ListArgs, OutputFormat, ProviderConfig};
use display::{bold, color, dim, hyperlink, Style, CYAN, GREEN, MAGENTA, RED, YELLOW};
use error::Error;
use label::LabelMatcher;
use regex::Regex;

/// Prefix for local branches materializing a virtual branch.
const VBRANCH_PREFIX: &str = "virtual/";

/// Env var pointing at the step-scoped outputs file in GitHub / Gitea / Forgejo
/// Actions. When set, `VBRANCH_*` values are written there so they become step
/// outputs, consumable via `${{ steps.<id>.outputs.VBRANCH_NAME }}`.
const GITHUB_OUTPUT: &str = "GITHUB_OUTPUT";

/// Fallback dotenv path used when neither `--output-dotenv` nor `$GITHUB_OUTPUT`
/// resolves to a file. Truncated on each invocation.
const DEFAULT_OUTPUT_DOTENV: &str = "vbranch-output.env";

fn run() -> Result<(), Error> {
    let cli = Cli::parse();
    let style = Style::new(cli.no_color);

    let default_level = if cli.verbose { "debug" } else { "warn" };
    let mut builder =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default_level));
    builder.format_target(false).format_timestamp(None);
    if !style.color {
        builder.write_style(env_logger::WriteStyle::Never);
    }
    builder.init();

    match cli.command {
        Command::Checkout(args) => checkout(
            &cli.config,
            &cli.label,
            cli.allowed_bases.as_deref(),
            cli.format,
            style,
            args,
        ),
        Command::List(args) => list(
            &cli.config,
            &cli.label,
            cli.allowed_bases.as_deref(),
            cli.format,
            style,
            args,
        ),
    }
}

/// Compile the `--allowed-bases` list into anchored regexes. When the list is
/// not provided, fall back to the repository's default branch (one API call).
fn compile_allowed_bases(
    provider: &dyn Provider,
    cli_list: Option<&[String]>,
) -> Result<Vec<Regex>, Error> {
    let patterns: Vec<String> = match cli_list {
        Some(list) if !list.is_empty() => list.iter().cloned().collect(),
        _ => {
            let default = provider.default_branch()?;
            log::debug!("--allowed-bases not set, using repo default branch: {default}");
            vec![regex::escape(&default)]
        }
    };
    patterns
        .iter()
        .map(|p| {
            let anchored = format!("^(?:{p})$");
            Regex::new(&anchored).map_err(|e| {
                Error::Config(format!("invalid --allowed-bases regex '{p}': {e}"))
            })
        })
        .collect()
}

/// Drop virtual branches whose base branch does not match any of the allowed regexes.
/// A warning is logged for each dropped vbranch; the command still succeeds.
fn filter_allowed_bases(
    vbranches: Vec<VirtualBranch>,
    allowed: &[Regex],
) -> Vec<VirtualBranch> {
    vbranches
        .into_iter()
        .filter(|vb| {
            if allowed.iter().any(|r| r.is_match(&vb.base_branch)) {
                true
            } else {
                println!(
                    "Ignoring virtual branch '{}' ({} PR/MR(s) on base '{}' which is not in --allowed-bases)",
                    vb.name,
                    vb.members.len(),
                    vb.base_branch,
                );
                false
            }
        })
        .collect()
}

fn checkout(
    config: &ProviderConfig,
    label: &str,
    allowed_bases: Option<&[String]>,
    format: OutputFormat,
    style: Style,
    args: CheckoutArgs,
) -> Result<(), Error> {
    // Reset the output file first: any later "nothing to do" exit or error
    // must not leave a stale dotenv from a previous run behind (truncate mode only).
    reset_output_if_truncate(&args)?;

    let matcher = LabelMatcher::new(label)?;

    // Arg provided → always local mode. No arg → try CI first, fall back to local.
    let provider: Box<dyn Provider> = if args.branch.is_some() {
        ci::detect_local_provider(config, style)?
    } else {
        ci::detect_provider(config, style)?
    };

    let allowed = compile_allowed_bases(&*provider, allowed_bases)?;
    let vbranches = filter_allowed_bases(provider.list_virtual_branches(&matcher)?, &allowed);

    // Resolve the arg (or the current branch, if no arg) to a virtual branch.
    let target_arg: String = match args.branch.as_deref() {
        Some(t) => t.to_string(),
        None => match provider.current_branch() {
            Some(b) => b.to_string(),
            None => {
                return Err(Error::Config(
                    "no positional argument and current branch is unknown (detached HEAD?). \
                     Pass a branch or virtual branch name explicitly."
                        .into(),
                ));
            }
        },
    };

    let no_arg = args.branch.is_none();
    let resolution = match resolve_vbranch(&target_arg, &vbranches, &config.git_remote) {
        Ok(r) => r,
        Err(e) => {
            // When no arg was provided, a "not found" resolution is the common
            // CI case (PR with no matching label). Either fall back to an
            // always-rebase of the PR onto its base (opt-in), or exit 0 with
            // "nothing to do" (default).
            if no_arg {
                if args.fallback_rebase {
                    return fallback_rebase(
                        config,
                        &allowed,
                        style,
                        &args,
                        &*provider,
                        &target_arg,
                    );
                }
                println!(
                    "Current branch '{target_arg}' is not associated with any virtual branch matching '{}'. Nothing to do.",
                    color(style, YELLOW, label)
                );
                return Ok(());
            }
            return Err(e);
        }
    };

    match &resolution {
        Resolution::ByName(_) => println!("Virtual branch found:"),
        Resolution::ByMember(_) if no_arg => println!(
            "Virtual branch found, of which current branch '{}' is a member:",
            color(style, GREEN, &target_arg)
        ),
        Resolution::ByMember(_) => println!(
            "Virtual branch found, of which '{}' is a member:",
            color(style, GREEN, &target_arg)
        ),
    }
    let vbranch = resolution.vbranch();
    print_vbranches(std::slice::from_ref(vbranch), format, style);

    materialize_vbranch(config, style, &args, vbranch)
}

/// `--fallback-rebase` path: the current branch's PR/MR exists and targets
/// an allowed base, but it's not a member of any virtual branch. Rebase the
/// current branch onto the remote base and emit minimal `VBRANCH_*` output.
fn fallback_rebase(
    config: &ProviderConfig,
    allowed_bases: &[Regex],
    style: Style,
    args: &CheckoutArgs,
    provider: &dyn Provider,
    current_branch: &str,
) -> Result<(), Error> {
    use ci::PrMatch;

    // Rebase rewrites history of the local branch; in a developer's checkout
    // that's surprising and potentially destructive. Restrict to CI contexts,
    // detected via the universal `CI` env var set by every major orchestrator.
    if std::env::var_os("CI").is_none() {
        println!(
            "--fallback-rebase has no effect outside CI (CI env var not set); \
             treating current branch as not in any virtual branch. Nothing to do."
        );
        return Ok(());
    }

    let (base, pr_number, pr_url) = match provider.pr_for_source(current_branch)? {
        PrMatch::None => {
            println!(
                "Current branch '{}' has no open PR/MR. Nothing to do.",
                color(style, GREEN, current_branch)
            );
            return Ok(());
        }
        PrMatch::Multiple(matches) => {
            let desc: Vec<String> = matches
                .iter()
                .map(|(n, b)| format!("#{n} → {b}"))
                .collect();
            return Err(Error::Config(format!(
                "current branch '{current_branch}' is the source of multiple open PRs/MRs ({}). \
                 Ambiguous: cannot decide which base to rebase onto.",
                desc.join(", ")
            )));
        }
        PrMatch::One {
            base_branch,
            pr_number,
            url,
        } => (base_branch, pr_number, url),
    };

    if !allowed_bases.iter().any(|r| r.is_match(&base)) {
        println!(
            "Current branch's PR/MR targets '{base}', not in --allowed-bases. Nothing to do."
        );
        return Ok(());
    }

    let pr_label = format!("#{pr_number}");
    let pr_linked = hyperlink(style, &pr_url, &color(style, YELLOW, &pr_label));
    println!(
        "Found PR/MR {pr_linked} for current branch '{}'. Rebasing onto '{}' (--fallback-rebase enabled)",
        color(style, GREEN, current_branch),
        color(style, MAGENTA, &base)
    );

    let remote_ref = format!("{}/{}", config.git_remote, base);

    if args.dry_run {
        println!("Dry run: skipping fetch and rebase.");
        return Ok(());
    }

    git::fetch_branches(&config.git_remote, &[base.clone()])?;
    git::rebase(&remote_ref)?;

    write_rebase_output(current_branch, &base, args)?;

    println!(
        "Rebased {} onto {} successfully.",
        color(style, GREEN, current_branch),
        color(style, MAGENTA, &remote_ref)
    );
    Ok(())
}

/// How `resolve_vbranch` matched a positional argument to a virtual branch.
enum Resolution<'a> {
    /// Matched by vbranch name — either `virtual/<name>` or a bare vbranch name.
    ByName(&'a VirtualBranch),
    /// Matched because the given branch is a member of the vbranch.
    ByMember(&'a VirtualBranch),
}

impl<'a> Resolution<'a> {
    fn vbranch(&self) -> &'a VirtualBranch {
        match self {
            Resolution::ByName(vb) | Resolution::ByMember(vb) => vb,
        }
    }
}

/// Resolve a positional argument to a virtual branch. Implements the three forms
/// documented on `CheckoutArgs::branch`.
fn resolve_vbranch<'a>(
    arg: &str,
    vbranches: &'a [VirtualBranch],
    remote: &str,
) -> Result<Resolution<'a>, Error> {
    // Form 1: `virtual/<name>` — explicit virtual branch.
    if let Some(name) = arg.strip_prefix(VBRANCH_PREFIX) {
        return vbranches
            .iter()
            .find(|v| v.name == name)
            .map(Resolution::ByName)
            .ok_or_else(|| Error::Config(format!("no virtual branch named '{name}'")));
    }

    // Form 2a: existing real branch that participates in a virtual branch.
    if git::branch_exists(arg, remote) {
        if let Some(vb) = vbranches
            .iter()
            .find(|v| v.members.iter().any(|m| m.source_branch == arg))
        {
            return Ok(Resolution::ByMember(vb));
        }
    }

    // Form 2b: name of a defined virtual branch.
    if let Some(vb) = vbranches.iter().find(|v| v.name == arg) {
        return Ok(Resolution::ByName(vb));
    }

    Err(Error::Config(format!(
        "'{arg}' is neither a branch participating in a virtual branch nor a defined virtual branch name"
    )))
}

fn materialize_vbranch(
    config: &ProviderConfig,
    style: Style,
    args: &CheckoutArgs,
    vbranch: &VirtualBranch,
) -> Result<(), Error> {
    let local_name = format!("{VBRANCH_PREFIX}{}", vbranch.name);
    let members: Vec<String> = vbranch
        .members
        .iter()
        .map(|m| m.source_branch.clone())
        .collect();

    let remote = &config.git_remote;
    let base_ref = format!("{remote}/{}", vbranch.base_branch);
    let member_refs: Vec<String> = members
        .iter()
        .map(|b| format!("{remote}/{b}"))
        .collect();

    if args.dry_run {
        println!("Dry run: skipping fetch, checkout and merge.");
        return Ok(());
    }

    let mut to_fetch = vec![vbranch.base_branch.clone()];
    to_fetch.extend(members.clone());
    git::fetch_branches(remote, &to_fetch)?;

    git::checkout_b(&local_name, &base_ref, args.force)?;

    if !member_refs.is_empty() {
        git::octopus_merge(&member_refs)?;
    }

    write_vbranch_output(vbranch, args)?;

    println!(
        "Virtual branch {} checked out successfully.",
        color(style, CYAN, &local_name)
    );
    Ok(())
}

fn list(
    config: &ProviderConfig,
    label: &str,
    allowed_bases: Option<&[String]>,
    format: OutputFormat,
    style: Style,
    _args: ListArgs,
) -> Result<(), Error> {
    let matcher = LabelMatcher::new(label)?;
    let provider = ci::detect_provider(config, style)?;
    let allowed = compile_allowed_bases(&*provider, allowed_bases)?;
    let vbranches = filter_allowed_bases(provider.list_virtual_branches(&matcher)?, &allowed);

    if vbranches.is_empty() {
        println!(
            "No virtual branch found (no open PR/MR carrying a label matching '{}').",
            color(style, YELLOW, label)
        );
        return Ok(());
    }

    print_vbranches(&vbranches, format, style);
    Ok(())
}

fn print_vbranches(vbranches: &[VirtualBranch], format: OutputFormat, style: Style) {
    match format {
        OutputFormat::Tree => print_tree(vbranches, style),
        OutputFormat::Table => print_table(vbranches, style),
    }
}

fn print_tree(vbranches: &[VirtualBranch], style: Style) {
    for (i, vb) in vbranches.iter().enumerate() {
        if i > 0 {
            println!();
        }
        println!(
            "{} {}",
            bold(style, &color(style, CYAN, &vb.name)),
            dim(style, &format!("(base: {})", vb.base_branch))
        );
        for m in &vb.members {
            let pr_label = format!("#{}", m.pr_number);
            let pr_linked = hyperlink(style, &m.url, &color(style, YELLOW, &pr_label));
            println!(
                "  - {:<width$}  {}  {}",
                pr_linked,
                color(style, GREEN, &m.source_branch),
                dim(style, &m.title),
                // width is for the raw "#123" text so columns align even with
                // ANSI escapes present in pr_linked.
                width = pr_label.len().max(5),
            );
        }
    }
}

fn print_table(vbranches: &[VirtualBranch], style: Style) {
    let header_vbranch = "VBRANCH";
    let header_base = "BASE";
    let header_prs = "PRS";
    let header_members = "MEMBERS";

    let w_name = vbranches
        .iter()
        .map(|v| v.name.len())
        .max()
        .unwrap_or(0)
        .max(header_vbranch.len());
    let w_base = vbranches
        .iter()
        .map(|v| v.base_branch.len())
        .max()
        .unwrap_or(0)
        .max(header_base.len());
    let w_count = vbranches
        .iter()
        .map(|v| v.members.len().to_string().len())
        .max()
        .unwrap_or(0)
        .max(header_prs.len());

    println!(
        "{:<w_name$}  {:<w_base$}  {:>w_count$}  {}",
        bold(style, header_vbranch),
        bold(style, header_base),
        bold(style, header_prs),
        bold(style, header_members),
        w_name = w_name,
        w_base = w_base,
        w_count = w_count,
    );
    for vb in vbranches {
        let members: Vec<String> = vb
            .members
            .iter()
            .map(|m| {
                let pr_label = format!("#{}", m.pr_number);
                let pr_linked = hyperlink(style, &m.url, &color(style, YELLOW, &pr_label));
                format!("{}({})", color(style, GREEN, &m.source_branch), pr_linked)
            })
            .collect();
        println!(
            "{:<w_name$}  {:<w_base$}  {:>w_count$}  {}",
            color(style, CYAN, &vb.name),
            color(style, MAGENTA, &vb.base_branch),
            vb.members.len(),
            members.join(", "),
            w_name = w_name,
            w_base = w_base,
            w_count = w_count,
        );
    }
}

/// Publish the resolved virtual branch as a set of `KEY=VALUE` lines:
///
/// - `VBRANCH_NAME` — captured label value (e.g. `dev` for `vbranch:dev`)
/// - `VBRANCH_REF` — local ref name (`virtual/<name>`)
/// - `VBRANCH_BASE` — base branch of the member PRs/MRs (the merge base)
/// - `VBRANCH_MEMBER_PR_IDS` — comma-separated list of member PR/MR ids
/// - `VBRANCH_MEMBER_REFS` — comma-separated list of member source branches
///
/// Lines are always printed to stdout, and also written to a single dotenv file
/// whose path is resolved as:
/// 1. `--output-dotenv` / `GIT_VBRANCH_OUTPUT_DOTENV` if set — appended to;
/// 2. otherwise `$GITHUB_OUTPUT` when defined (GitHub / Gitea / Forgejo Actions)
///    — appended to, so values become step outputs;
/// 3. otherwise `vbranch-output.env` in the current directory — truncated first,
///    so each run produces a self-contained file.
fn write_vbranch_output(vbranch: &VirtualBranch, args: &CheckoutArgs) -> Result<(), Error> {
    let pr_ids = vbranch
        .members
        .iter()
        .map(|m| m.pr_number.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let member_refs = vbranch
        .members
        .iter()
        .map(|m| m.source_branch.clone())
        .collect::<Vec<_>>()
        .join(",");
    let local_ref = format!("{VBRANCH_PREFIX}{}", vbranch.name);

    let vars: [(&str, String); 5] = [
        ("VBRANCH_BASE", vbranch.base_branch.clone()),
        ("VBRANCH_MEMBER_PR_IDS", pr_ids),
        ("VBRANCH_MEMBER_REFS", member_refs),
        ("VBRANCH_NAME", vbranch.name.clone()),
        ("VBRANCH_REF", local_ref),
    ];

    emit_output_vars(&vars, args)
}

/// Publish the output variables for a `--fallback-rebase` run.
/// `VBRANCH_REF` holds the current branch name (not `virtual/<name>` — no
/// vbranch was materialized), `VBRANCH_BASE` is the rebase target, and the
/// vbranch-specific fields are empty.
fn write_rebase_output(
    current_branch: &str,
    base: &str,
    args: &CheckoutArgs,
) -> Result<(), Error> {
    let vars: [(&str, String); 5] = [
        ("VBRANCH_BASE", base.to_string()),
        ("VBRANCH_MEMBER_PR_IDS", String::new()),
        ("VBRANCH_MEMBER_REFS", String::new()),
        ("VBRANCH_NAME", String::new()),
        ("VBRANCH_REF", current_branch.to_string()),
    ];
    emit_output_vars(&vars, args)
}

fn emit_output_vars(vars: &[(&str, String); 5], args: &CheckoutArgs) -> Result<(), Error> {
    let block: String = vars
        .iter()
        .map(|(k, v)| format!("{k}={v}\n"))
        .collect();

    let (path, _) = resolve_output_path(args);
    append_line(&path, &block)?;
    log::debug!("wrote output vbranch variables to {path}");
    Ok(())
}

/// Resolve where the `VBRANCH_*` variables are written and whether the file is
/// truncated or appended to. See `write_vbranch_output`'s docstring for the rules.
fn resolve_output_path(args: &CheckoutArgs) -> (String, bool) {
    if let Some(p) = args.output_dotenv.clone() {
        (p, false)
    } else if let Some(p) = std::env::var(GITHUB_OUTPUT).ok().filter(|s| !s.is_empty()) {
        (p, false)
    } else {
        (DEFAULT_OUTPUT_DOTENV.to_string(), true)
    }
}

/// In truncate mode, empty the output file upfront so a previous run's content
/// can't be mistaken for the current one on "nothing to do" exits or failures.
/// No-op in append mode (`--output-dotenv` / `$GITHUB_OUTPUT`), where history
/// is preserved.
fn reset_output_if_truncate(args: &CheckoutArgs) -> Result<(), Error> {
    let (path, truncate) = resolve_output_path(args);
    if !truncate {
        return Ok(());
    }
    std::fs::write(&path, "")?;
    log::debug!("reset output vbranch variables file {path} (emptied)");
    Ok(())
}

fn append_line(path: &str, line: &str) -> Result<(), Error> {
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        use std::io::IsTerminal;
        let use_color = std::env::var_os("NO_COLOR").is_none()
            && std::io::stderr().is_terminal()
            && !std::env::args().any(|a| a == "--no-color");
        let prefix = if use_color {
            format!("\x1b[1m{RED}error:\x1b[0m")
        } else {
            "error:".to_string()
        };
        eprintln!("{prefix} {e}");
        std::process::exit(1);
    }
}
