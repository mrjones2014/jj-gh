use crate::model::Model;
use anyhow::{Context, Result};
use jj_gh_config_derive::subcommand_args;

subcommand_args! {
    pub struct PrUrlArgs {
        /// PR number or revision ID to lookup PR from.
        #[arg(value_name = "PR_NUM|REV")]
        pub number_or_rev: String,

        /// Git remote used for the user's own pushes and PR head lookups.
        /// Precedence: this flag, then git's auto-detected default push remote,
        /// then `default_remote` in config.
        #[arg(long, value_name = "NAME", global = true)]
        #[config(fallback = "default_remote")]
        pub remote: Option<String>,

        /// Git remote used as the PR target in fork workflows. Precedence: this
        /// flag, then `upstream_remote` in config, else the default upstream.
        #[arg(long, value_name = "NAME", global = true)]
        #[config(fallback = "upstream_remote")]
        pub upstream_remote: Option<String>,
    }
}

pub async fn run(model: &impl Model, args: &PrUrlArgs) -> Result<()> {
    let PrUrlArgs {
        globals: _,
        number_or_rev,
        remote,
        upstream_remote,
    } = args;
    let upstream_remote = crate::gh::remote::resolved_upstream_remote(upstream_remote);
    let pr = model
        .resolve_pr(remote, upstream_remote, number_or_rev)
        .await
        .context("resolving PR")?;
    crate::ui::print_url(&pr.html_url);
    Ok(())
}
