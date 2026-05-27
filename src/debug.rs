//! Diagnostic subcommands. Used to inspect resolved state without mutating anything.

use crate::{
    auth,
    cli::{DebugAction, GlobalOpts},
    config::{self, Config},
    gh::{Gh, real::OctocrabGh},
    git::url::parse_owner_repo,
    jj::{self, CommitInfo, Jj, real::JjCli},
    pr,
};
use anyhow::Result;
use figment::providers::Serialized;

/// Dispatch a `jj-gh debug` invocation.
///
/// # Errors
///
/// Returns an error from the underlying operation; for `auth` this means token
/// resolution failed, for `rev`/`pr-lookup` the jj or GH read failed.
pub async fn dispatch(global: &GlobalOpts, action: DebugAction) -> Result<()> {
    match action {
        DebugAction::Config => print_config(global),
        DebugAction::Auth => check_auth(global).await,
        DebugAction::Rev { rev } => print_rev(global, &rev).await,
        DebugAction::PrLookup { rev } => print_pr_lookup(global, &rev).await,
    }
}

fn load_config(global: &GlobalOpts) -> Result<Config> {
    let fig = config::load_figment().merge(Serialized::defaults(global));
    let config = config::extract(&fig)?;
    config::validate(&config)?;
    Ok(config)
}

fn print_config(global: &GlobalOpts) -> Result<()> {
    let config = load_config(global)?;
    println!("{config:#?}");
    Ok(())
}

async fn check_auth(global: &GlobalOpts) -> Result<()> {
    let config = load_config(global)?;
    auth::resolve_token(&config).await?;
    println!("ok");
    Ok(())
}

async fn print_rev(global: &GlobalOpts, rev: &str) -> Result<()> {
    let config = load_config(global)?;
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

    let origin_url = jj.remote_url(&config.default_remote).await?;
    let upstream_url = jj.remote_url(&config.upstream_remote).await?;
    let default_branch = jj.trunk_branch().await?;
    let default_branch_sha = match &default_branch {
        Some(branch) => {
            jj.remote_bookmark_sha(branch, &config.default_remote)
                .await?
        }
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

async fn print_pr_lookup(global: &GlobalOpts, rev: &str) -> Result<()> {
    let config = load_config(global)?;
    let jj = JjCli::new().await?;
    let token = auth::resolve_token(&config).await?;
    let gh = OctocrabGh::new(&token)?;

    let lookup = pr::resolve_pr_for_rev(&jj, &gh, &config, rev).await?;
    let base = gh
        .lookup_base(
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
    println!("base_branch_exists: {}", base.branch_exists);
    println!("base_repo_node_id: {}", base.repo_node_id);
    println!("existing_open_pr: {:#?}", lookup.summary);
    Ok(())
}
