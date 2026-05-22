//! Diagnostic subcommands. Used to inspect resolved state without mutating anything.

use crate::{
    auth,
    cli::DebugAction,
    config,
    gh::{Gh, real::OctocrabGh, remote},
    git::url::parse_owner_repo,
    jj::{self, CommitInfo, Jj, real::JjCli},
};
use anyhow::{Result, anyhow};

/// Dispatch a `jj-gh debug` invocation.
///
/// # Errors
///
/// Returns an error from the underlying operation; for `auth` this means token
/// resolution failed, for `rev`/`pr-lookup` the jj or GH read failed.
pub async fn dispatch(action: DebugAction) -> Result<()> {
    match action {
        DebugAction::Config => print_config(),
        DebugAction::Auth => check_auth().await,
        DebugAction::Rev { rev } => print_rev(&rev),
        DebugAction::PrLookup { rev } => print_pr_lookup(&rev).await,
    }
}

fn print_config() -> Result<()> {
    let config = config::load()?;
    println!("{config:#?}");
    Ok(())
}

async fn check_auth() -> Result<()> {
    let config = config::load()?;
    auth::resolve_token(&config).await?;
    println!("ok");
    Ok(())
}

fn print_rev(rev: &str) -> Result<()> {
    let jj = JjCli;

    let CommitInfo {
        change_id,
        commit_id,
        description,
        bookmarks,
    } = jj.resolve_rev(rev)?;
    let ancestor = jj.stacked_ancestor_bookmark(rev)?;
    let title_revset = jj::title_base_revset(rev, ancestor.as_deref());
    let default_title = jj.first_commit_description(&title_revset)?;

    let origin_url = jj.remote_url("origin")?;
    let upstream_url = jj.remote_url("upstream")?;
    let default_branch = jj.trunk_branch()?;
    let default_branch_sha = match &default_branch {
        Some(branch) => jj.remote_bookmark_sha(branch, "origin")?,
        None => None,
    };

    println!("rev: {rev}");
    println!("change_id: {change_id}");
    println!("commit_id: {commit_id}");
    println!("bookmarks: {bookmarks:?}");
    println!("description: {description:?}");
    println!("ancestor_bookmark: {ancestor:?}");
    println!("title_revset: {title_revset}");
    println!("default_title: {default_title:?}");
    println!("origin_url: {origin_url:?}");
    println!("upstream_url: {upstream_url:?}");
    println!("default_branch_on_origin: {default_branch:?}");
    println!("default_branch_sha: {default_branch_sha:?}");
    if let Some(url) = &origin_url {
        match parse_owner_repo(url) {
            Ok((owner, repo)) => println!("parsed_origin: ({owner}, {repo})"),
            Err(e) => println!("parsed_origin: error: {e}"),
        }
    }
    Ok(())
}

async fn print_pr_lookup(rev: &str) -> Result<()> {
    let jj = JjCli;
    let info = jj.resolve_rev(rev)?;
    let branch = info
        .bookmarks
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("no local bookmark on `{rev}`; nothing to look up"))?;

    let origin_url = jj
        .remote_url("origin")?
        .ok_or_else(|| anyhow!("origin remote is not configured"))?;
    let upstream_url = jj.remote_url("upstream")?;
    let target = remote::target(&origin_url, upstream_url.as_deref())?;
    let head_spec = target.head_spec(&branch);

    let default_base = jj
        .trunk_branch()?
        .ok_or_else(|| anyhow!("could not detect trunk() bookmark"))?;

    let config = config::load()?;
    let token = auth::resolve_token(&config).await?;
    let gh = OctocrabGh::new(&token)?;

    let existing = gh
        .find_open_pr(&target.owner, &target.repo, &head_spec)
        .await?;
    let base_exists = gh
        .branch_exists(&target.owner, &target.repo, &default_base)
        .await?;

    println!("rev: {rev}");
    println!("branch: {branch}");
    println!("target: {target:#?}");
    println!("head_spec: {head_spec}");
    println!("default_base: {default_base}");
    println!("base_branch_exists: {base_exists}");
    println!("existing_open_pr: {existing:#?}");
    Ok(())
}
