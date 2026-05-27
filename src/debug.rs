//! Diagnostic subcommands. Used to inspect resolved state without mutating anything.

use crate::{
    auth,
    cli::DebugAction,
    config,
    gh::{Gh, real::OctocrabGh},
    git::url::parse_owner_repo,
    jj::{self, CommitInfo, Jj, real::JjCli},
    pr,
};
use anyhow::Result;

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
        DebugAction::Rev { rev } => print_rev(&rev).await,
        DebugAction::PrLookup { rev } => print_pr_lookup(&rev).await,
    }
}

fn print_config() -> Result<()> {
    let config = config::debug_load()?;
    println!("{config:#?}");
    Ok(())
}

async fn check_auth() -> Result<()> {
    let config = config::debug_load()?;
    auth::resolve_token(&config).await?;
    println!("ok");
    Ok(())
}

async fn print_rev(rev: &str) -> Result<()> {
    let jj = JjCli::new().await?;

    let CommitInfo {
        change_id,
        commit_id,
        description,
        bookmarks,
    } = jj.resolve_rev(rev).await?;
    let ancestor = jj.stacked_ancestor_bookmark(rev).await?;
    let title_revset = jj::title_base_revset(rev, ancestor.as_deref());
    let default_title = jj.first_commit_description(&title_revset).await?;

    let origin_url = jj.remote_url("origin").await?;
    let upstream_url = jj.remote_url("upstream").await?;
    let default_branch = jj.trunk_branch().await?;
    let default_branch_sha = match &default_branch {
        Some(branch) => jj.remote_bookmark_sha(branch, "origin").await?,
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
    let jj = JjCli::new().await?;
    let config = config::debug_load()?;
    let token = auth::resolve_token(&config).await?;
    let gh = OctocrabGh::new(&token)?;

    let lookup = pr::resolve_pr_for_rev(&jj, &gh, rev).await?;
    let base_exists = gh
        .branch_exists(
            &lookup.target.owner,
            &lookup.target.repo,
            &lookup.default_base,
        )
        .await?;

    println!("rev: {rev}");
    println!("branch: {}", lookup.branch);
    println!("target: {:#?}", lookup.target);
    println!("head_spec: {}", lookup.head_spec);
    println!("default_base: {}", lookup.default_base);
    println!("base_branch_exists: {base_exists}");
    println!("existing_open_pr: {:#?}", lookup.summary);
    Ok(())
}
