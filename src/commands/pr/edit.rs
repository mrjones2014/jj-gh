use crate::{
    cli::GlobalOpts,
    editor::{self, ApplyChangesCtx},
    frontmatter::Frontmatter,
    gh::{Gh, PrDetails},
    jj::Jj,
    model::Model,
    ui::Spinner,
};
use anyhow::{Context, Result, bail};
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

        /// Editor command, e.g. `--editor "nvim +7"`. Precedence: this flag,
        /// then `$VISUAL`, then `$EDITOR`, then `editor` in config.
        #[arg(short = 'e', long, value_name = "CMD", value_parser = crate::util::parse_shell_command)]
        #[config(fallback = "editor")]
        pub editor: Option<crate::util::ShellCommand>,

        /// Show a preview of the PR diffs while editing the PR.
        /// Overrides `pr_edit_show_diffs` configuration. Use `--no-diffs` to disable.
        #[arg(
            long = "diffs",
            num_args = 0,
            default_missing_value = "true",
            default_value_if("no_diffs", "true", Some("false"))
        )]
        #[config(maps_to = "pr_edit_show_diffs")]
        pub show_diffs: bool,

        /// Hide the PR diff preview while editing the PR. Overrides config.
        #[arg(long = "no-diffs", conflicts_with = "show_diffs")]
        pub no_diffs: bool,
    }
}

/// Fetch a PR, open the editor with its diff as a read-only preview, and apply
/// only the metadata changes. Properties the user didn't touch (labels added
/// by a bot, etc.) keep their current value because the editor buffer is seeded
/// from the current PR state.
///
/// # Errors
///
/// Returns an error from any step (rev resolution, GH API, editor, etc.).
pub async fn run(model: &impl Model, args: &EditArgs) -> Result<()> {
    let gh = model.gh().await?;
    let editor = model.editor();
    let EditArgs {
        number_or_rev,
        force,
        editor: editor_cfg,
        show_diffs,
        no_diffs: _,
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
    let editor_argv = editor::resolve_editor(editor_cfg, model.env()).await?;
    let spinner = Spinner::start("Resolving PR");

    let (target, pr_number) = model
        .resolve_pr_number_with_target(remote.as_ref(), upstream_remote, number_or_rev)
        .await?;
    let details = gh
        .get_pr(&target.owner, &target.repo, pr_number)
        .await
        .context("fetching PR from GitHub")?;

    let diff = if *show_diffs {
        match model
            .jj()
            .pr_diff(&details.base_sha, &details.head_sha)
            .await
        {
            Ok(diff) => Some(diff),
            Err(e) => {
                log::debug!("could not render PR diff from local commits: {e:#}");
                spinner.set_message("Loading diffs from GitHub".to_string());
                gh.get_pr_diff(&target.owner, &target.repo, details.number)
                    .await
                    .inspect_err(|e| log::warn!("Could not load PR diff preview: {e:#}"))
                    .ok()
            }
        }
    } else {
        None
    };
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

    let preview = diff
        .as_deref()
        .map(str::trim)
        .filter(|diff| !diff.is_empty());
    let (after_fm, after_body) =
        editor::round_trip(editor, &editor_argv, &before_fm, &before_body, preview).await?;

    validate_edit(&after_fm, &after_body, *force)?;

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

fn validate_edit(frontmatter: &Frontmatter, body: &str, force: bool) -> Result<()> {
    if body.is_empty() {
        if force {
            log::warn!("PR body is empty, but `--force` was passed");
        } else {
            bail!(
                "PR body is empty when attempting to edit. Refusing to edit to avoid data loss. Pass `--force` to override."
            );
        }
    }
    if frontmatter.title.trim().is_empty() {
        bail!("title is empty");
    }
    if frontmatter.base.trim().is_empty() {
        bail!("base is empty");
    }
    if body.trim().is_empty() {
        if force {
            log::warn!("PR body is empty, but `--force` was passed");
        } else {
            bail!(
                "PR body is empty when attempting to edit. Refusing to edit to avoid data loss. Pass `--force` to override."
            );
        }
    }
    Ok(())
}
