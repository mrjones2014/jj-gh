use crate::{
    cli::GlobalOpts,
    config::AutoMergeMethod,
    gh::Gh,
    jj::{Jj, JjExt},
    pr,
};
use anyhow::{Context, Result, bail};
use jj_gh_config_derive::subcommand_args;

subcommand_args! {
    pub struct AutoMergeArgs {
        /// PR number, or revision ID to look up a PR from.
        #[arg(value_name = "PR_NUM|REV")]
        pub number_or_rev: String,

        /// Merge method used when enabling auto-merge. Overrides config
        /// `auto_merge_method` (default `merge`).
        #[arg(long = "method", short = 'm', value_name = "METHOD", value_enum)]
        #[config]
        pub auto_merge_method: AutoMergeMethod,
    }
}

pub async fn run<J, G>(jj: &J, gh: &G, args: &AutoMergeArgs) -> Result<()>
where
    J: Jj,
    G: Gh,
{
    let AutoMergeArgs {
        number_or_rev,
        auto_merge_method,
        globals:
            GlobalOpts {
                remote,
                upstream_remote,
                verbose: _,
                quiet: _,
                log_level: _,
                gh_askpass: _,
                askpass_timeout_secs: _,
            },
    } = args;

    let remote = jj.resolve_default_remote(remote.as_ref()).await?;
    let pr = pr::get_pr(jj, gh, &remote, upstream_remote, number_or_rev).await?;

    if pr.in_merge_queue {
        bail!(
            "auto-merge not supported for repos with merge queues enabled; this is a limitation of the GitHub API. See https://github.com/mrjones2014/jj-gh/issues/103"
        );
    }

    gh.enable_auto_merge(&pr.graphql_node_id, *auto_merge_method)
        .await
        .with_context(|| format!("enabling auto-merge on #{}", pr.number))?;

    log::info!("Enabled auto-merge for PR");
    println!("{}", pr.html_url);
    Ok(())
}
