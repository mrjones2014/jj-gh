use crate::{
    auth::EnvReader,
    cli::GlobalOpts,
    editor::{self, ApplyChangesCtx, Editor, resolve_editor_argv},
    frontmatter::Frontmatter,
    gh::{
        Gh, PrDetails,
        pr_lookup::{self, PrLookup},
        remote::Target,
    },
    jj::{Jj, JjExt},
    ui::Spinner,
};
use anyhow::{Context, Result, anyhow, bail};
use jj_gh_config_derive::subcommand_args;
use std::collections::HashMap;

subcommand_args! {
    pub struct EditArgs {
        /// PR number, or revision ID to look up a PR from.
        #[arg(value_name = "PR_NUM|REV")]
        pub number_or_rev: String,

        /// Edit even if the PR body is empty. By default, `jj-gh` refuses to edit
        /// an empty body to avoid clobbering one that exists but failed to load.
        #[arg(short = 'f', long)]
        pub force: bool,

        /// Editor command, e.g. `--editor "nvim +7"`. Default:
        /// `editor` in config, then `$VISUAL`, then `$EDITOR`.
        #[arg(short = 'e', long, value_name = "CMD", value_parser = shell_words::split)]
        #[config]
        pub editor: Option<Vec<String>>,
    }
}

/// Fetch a PR, open the editor, and apply only the diff. Properties the user
/// didn't touch (labels added by a bot, etc.) keep their current value because
/// the editor buffer is seeded from the current PR state.
///
/// # Errors
///
/// Returns an error from any step (rev resolution, GH API, editor, etc.).
pub async fn run<J, G, E, ENV>(jj: &J, gh: &G, env: &ENV, editor: &E, args: &EditArgs) -> Result<()>
where
    J: Jj,
    G: Gh,
    E: Editor,
    ENV: EnvReader,
{
    let EditArgs {
        number_or_rev,
        force,
        editor: editor_cfg,
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

    let editor_argv = resolve_editor_argv(editor_cfg.as_deref(), env)?;
    let remote = jj.resolve_default_remote(remote.as_ref()).await?;

    let spinner = Spinner::start("Resolving PR");

    let (target, pr_number) =
        resolve_pr_target(jj, gh, &remote, upstream_remote, number_or_rev).await?;
    let details = gh
        .get_pr(&target.owner, &target.repo, pr_number)
        .await
        .context("fetching PR from GitHub")?;

    spinner.stop();

    let PrDetails {
        number,
        title,
        html_url,
        base_ref,
        graphql_node_id,
        in_merge_queue,
        labels,
        is_draft,
        auto_merge,
        auto_merge_method,
        reviewers,
        body,
        ..
    } = details;

    let before_body = body;
    let before_label_ids = labels
        .iter()
        .map(|l| (l.name.clone(), l.node_id.clone()))
        .collect::<HashMap<String, String>>();
    let before_fm = Frontmatter {
        title,
        base: target.base_spec(&base_ref),
        labels: labels.into_iter().map(|l| l.name).collect(),
        reviewers,
        draft: is_draft,
        auto_merge,
        auto_merge_method: auto_merge_method.unwrap_or_default(),
    };

    let (after_fm, after_body) =
        editor::round_trip(editor, &editor_argv, &before_fm, &before_body, None).await?;

    if after_body.is_empty() {
        if *force {
            log::warn!("PR body is empty, but `--force` was passed");
        } else {
            bail!(
                "PR body is empty when attempting to edit. Refusing to edit to avoid data loss. Pass `--force` to override."
            );
        }
    }

    if after_fm.title.trim().is_empty() {
        bail!("title is empty");
    }
    if after_fm.base.trim().is_empty() {
        bail!("base is empty");
    }

    if after_body.trim().is_empty() {
        if *force {
            log::warn!("PR body is empty, but `--force` was passed");
        } else {
            bail!(
                "PR body is empty when attempting to edit. Refusing to edit to avoid data loss. Pass `--force` to override."
            );
        }
    }

    let ctx = ApplyChangesCtx {
        owner: &target.owner,
        repo: &target.repo,
        pr_number: number,
        pr_node_id: &graphql_node_id,
        has_merge_queue: in_merge_queue,
        before_label_ids,
    };
    editor::apply_frontmatter_diff(gh, &ctx, &before_fm, &before_body, &after_fm, &after_body)
        .await
        .with_context(|| format!("editing PR #{number}"))?;

    log::info!("Updated PR #{number}");
    println!("{html_url}");
    Ok(())
}

/// Resolve the target repo plus PR number from either a numeric PR or a
/// revision whose local bookmark has an open PR.
async fn resolve_pr_target<J: Jj, G: Gh>(
    jj: &J,
    gh: &G,
    default_remote: &str,
    upstream_remote: &str,
    number_or_rev: &str,
) -> Result<(Target, u64)> {
    if let Ok(num) = number_or_rev.parse::<u64>() {
        let origin_url = jj
            .remote_url(default_remote)
            .await?
            .ok_or_else(|| anyhow!("`{default_remote}` remote is not configured"))?;
        let upstream_url = jj.remote_url(upstream_remote).await?;
        let target = crate::gh::remote::target(&origin_url, upstream_url.as_deref())?;
        return Ok((target, num));
    }
    let PrLookup {
        target,
        head_spec,
        summary,
        ..
    } = pr_lookup::resolve_pr_for_rev(jj, gh, default_remote, upstream_remote, number_or_rev)
        .await?;
    let summary = summary
        .ok_or_else(|| anyhow!("no open PR for revision `{number_or_rev}` (head `{head_spec}`)"))?;
    Ok((target, summary.number))
}
