use crate::model::Model;
use anyhow::{Context, Result};
use jj_gh_config_derive::subcommand_args;

subcommand_args! {
    pub struct PrUrlArgs {
        /// PR number or revision ID to lookup PR from.
        #[arg(value_name = "PR_NUM|REV")]
        pub number_or_rev: String,

        /// Git remote used for the user's own pushes and PR head lookups.
        /// Default: `origin` (or `default_remote` in config).
        #[arg(long, value_name = "NAME", global = true)]
        #[config(maps_to = "default_remote")]
        pub remote: Option<String>,

        /// Git remote used as the PR target in fork workflows. Default:
        /// `upstream` (or `upstream_remote` in config).
        #[arg(long, value_name = "NAME", global = true)]
        #[config]
        pub upstream_remote: String,
    }
}

pub async fn run(model: &impl Model, args: &PrUrlArgs) -> Result<()> {
    let PrUrlArgs {
        globals: _,
        number_or_rev,
        remote,
        upstream_remote,
    } = args;
    let pr = model
        .resolve_pr(remote.as_ref(), upstream_remote.as_ref(), number_or_rev)
        .await
        .context("resolving PR")?;
    println!("{}", pr.html_url.trim());
    Ok(())
}
