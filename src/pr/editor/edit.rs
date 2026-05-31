use crate::{
    auth::EnvReader,
    config::Config,
    gh::{Gh, PrDetails, remote::Target},
    jj::Jj,
    pr::{
        self, PrLookup,
        editor::{self, ApplyChangesCtx, Editor, resolve_editor_argv},
        frontmatter::Frontmatter,
    },
};
use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, clap::Args, Serialize)]
pub struct EditArgs {
    /// PR number, or revision ID to look up a PR from.
    #[arg(value_name = "PR_NUM|REV")]
    #[serde(skip)]
    pub number_or_rev: String,

    /// Edit even if the PR body is empty. By default, `jj-gh` refuses to edit
    /// an empty body to avoid clobbering one that exists but failed to load.
    #[arg(short = 'f', long)]
    #[serde(skip)]
    pub force: bool,

    /// Editor command; shell-words split, e.g. `--editor "nvim +7"`. Default:
    /// `editor` in config, then `$VISUAL`, then `$EDITOR`.
    #[arg(short = 'e', long, value_name = "CMD", value_parser = shell_words::split)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor: Option<Vec<String>>,
}

/// Fetch a PR, open the editor, and apply only the diff. Properties the user
/// didn't touch (labels added by a bot, etc.) keep their current value because
/// the editor buffer is seeded from the current PR state.
///
/// # Errors
///
/// Returns an error from any step (rev resolution, GH API, editor, etc.).
pub async fn run<J, G, E, ENV>(
    jj: &J,
    gh: &G,
    env: &ENV,
    editor: &E,
    config: &Config,
    args: &EditArgs,
) -> Result<()>
where
    J: Jj,
    G: Gh,
    E: Editor,
    ENV: EnvReader,
{
    let editor_argv = resolve_editor_argv(config, env)?;

    let (owner, repo, pr_number) = resolve_pr_target(jj, gh, config, &args.number_or_rev).await?;
    let details = gh
        .get_pr(&owner, &repo, pr_number, true)
        .await
        .context("fetching PR from GitHub")?;
    let PrDetails {
        number,
        title,
        html_url,
        head_ref,
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

    if body.is_none() {
        if args.force {
            log::warn!("PR body is empty, but `--force` was passed");
        } else {
            bail!(
                "PR body is empty when attempting to edit. Refusing to edit to avoid data loss. Pass `--force` to override."
            );
        }
    }

    let before_body = body.unwrap_or_default();
    let before_label_ids: HashMap<String, String> = labels
        .iter()
        .map(|l| (l.name.clone(), l.node_id.clone()))
        .collect();
    let before_fm = Frontmatter {
        title,
        base: head_ref,
        labels: labels.into_iter().map(|l| l.name).collect(),
        reviewers,
        draft: is_draft,
        auto_merge,
        auto_merge_method: auto_merge_method.unwrap_or_default(),
    };

    let (after_fm, after_body) =
        editor::round_trip(editor, &editor_argv, &before_fm, &before_body).await?;

    if after_fm.title.trim().is_empty() {
        bail!("title is empty");
    }
    if after_fm.base.trim().is_empty() {
        bail!("base is empty");
    }
    if after_body.trim().is_empty() && !args.force {
        bail!("body is empty; pass `--force` to confirm");
    }

    let ctx = ApplyChangesCtx {
        owner: &owner,
        repo: &repo,
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

/// Resolve the `(owner, repo, pr_number)` tuple from either a numeric PR or a
/// revision whose local bookmark has an open PR.
async fn resolve_pr_target<J: Jj, G: Gh>(
    jj: &J,
    gh: &G,
    config: &Config,
    number_or_rev: &str,
) -> Result<(String, String, u64)> {
    if let Ok(num) = number_or_rev.parse::<u64>() {
        let origin_url = jj
            .remote_url(&config.default_remote)
            .await?
            .ok_or_else(|| anyhow!("`{}` remote is not configured", config.default_remote))?;
        let upstream_url = jj.remote_url(&config.upstream_remote).await?;
        let target = crate::gh::remote::target(&origin_url, upstream_url.as_deref())?;
        return Ok((target.owner, target.repo, num));
    }
    let PrLookup {
        target: Target { owner, repo, .. },
        head_spec,
        summary,
        ..
    } = pr::resolve_pr_for_rev(jj, gh, config, number_or_rev).await?;
    let summary = summary
        .ok_or_else(|| anyhow!("no open PR for revision `{number_or_rev}` (head `{head_spec}`)"))?;
    Ok((owner, repo, summary.number))
}
