use crate::{
    cli::AuthArgs,
    config::{AutoMergeMethod, Config},
    gh::{Gh, remote},
    jj::Jj,
    pr::resolve_pr,
};
use anyhow::{Context, Result, anyhow};
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
    let pr = if let Ok(num) = number_or_rev.parse::<u64>() {
        let origin_url = jj
            .remote_url("origin")
            .await?
            .ok_or_else(|| anyhow!("origin remote is not configured"))?;
        let upstream_url = jj.remote_url("upstream").await?;
        let target = remote::target(&origin_url, upstream_url.as_deref())?;
        gh.get_pr(&target.owner, &target.repo, num).await?
    } else {
        let lookup = resolve_pr(jj, gh, number_or_rev).await?;
        let summary = lookup.summary.ok_or_else(|| {
            anyhow!(
                "no open PR for revision `{number_or_rev}` (head `{}`)",
                lookup.head_spec,
            )
        })?;
        gh.get_pr(&lookup.target.owner, &lookup.target.repo, summary.number)
            .await?
    };

    gh.enable_auto_merge(&pr.graphql_node_id, config.auto_merge_method)
        .await
        .with_context(|| format!("enabling auto-merge on #{}", pr.number))?;

    log::info!("Enabled auto-merge for PR");
    println!("{}", pr.html_url);
    Ok(())
}
