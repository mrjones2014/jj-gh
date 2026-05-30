use crate::{
    cli::AuthArgs,
    config::{AutoMergeMethod, Config},
    gh::Gh,
    jj::Jj,
    pr::{self},
};
use anyhow::{Context, Result};
use serde::Serialize;

#[derive(Debug, clap::Args, Serialize)]
pub struct AutoMergeArgs {
    /// PR number, or revision ID to look up a PR from.
    #[arg(value_name = "PR_NUM|REV")]
    #[serde(skip)]
    pub number_or_rev: String,

    /// Merge method used when enabling auto-merge. Overrides config
    /// `auto_merge_method` (default `merge`).
    #[arg(long = "method", short = 'm', value_name = "METHOD", value_enum)]
    #[serde(rename = "auto_merge_method", skip_serializing_if = "Option::is_none")]
    pub method: Option<AutoMergeMethod>,

    #[command(flatten)]
    #[serde(flatten)]
    pub auth: AuthArgs,
}

pub async fn run<J, G>(jj: &J, gh: &G, config: &Config, args: &AutoMergeArgs) -> Result<()>
where
    J: Jj,
    G: Gh,
{
    let AutoMergeArgs {
        number_or_rev,
        // these are merged into Config by figment layering
        method: _,
        auth: _,
    } = args;
    let pr = pr::get_pr(jj, gh, config, number_or_rev, false).await?;

    gh.enable_auto_merge(
        &pr.graphql_node_id,
        pr.in_merge_queue,
        config.auto_merge_method,
    )
    .await
    .with_context(|| format!("enabling auto-merge on #{}", pr.number))?;

    log::info!("Enabled auto-merge for PR");
    println!("{}", pr.html_url);
    Ok(())
}
