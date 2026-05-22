//! Diagnostic subcommands. Used to inspect resolved state without mutating anything.

use crate::{
    auth,
    cli::DebugAction,
    config,
    git::url::parse_owner_repo,
    jj::{self, CommitInfo, Jj, real::JjCli},
};
use anyhow::Result;

/// Dispatch a `jj-gh debug` invocation.
///
/// # Errors
///
/// Returns an error from the underlying operation; for `auth` this means token
/// resolution failed, for `rev` it means the jj read failed.
pub async fn dispatch(action: DebugAction) -> Result<()> {
    match action {
        DebugAction::Config => print_config(),
        DebugAction::Auth => check_auth().await,
        DebugAction::Rev { rev } => print_rev(&rev),
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
    let default_branch = jj::default_branch(&jj, "origin")?;
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
