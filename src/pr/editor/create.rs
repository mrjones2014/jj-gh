use crate::{
    auth::EnvReader,
    cli::GlobalOpts,
    config::AutoMergeMethod,
    gh::{CreatePrRequest, Gh, remote},
    jj::{self, Jj, JjExt},
    logging::ResultExt,
    pr::{
        editor::{self, ApplyChangesCtx, Editor, resolve_editor_argv},
        frontmatter::Frontmatter,
        load_template_for, validation,
    },
};
use anyhow::{Context, Result, anyhow};
use jj_gh_config_derive::subcommand_args;
use std::collections::HashMap;

subcommand_args! {
    pub struct CreateArgs {
        /// Revision to create the PR from.
        #[arg(value_name = "REV")]
        pub rev: String,

        /// Override the base bookmark. Default: closest ancestor bookmark on
        /// the stack, falling back to jj `trunk()`, then to the configured
        /// `default_base_branch`. Errors if none resolve.
        #[arg(long, value_name = "BRANCH")]
        #[config(fallback = "default_base_branch")]
        pub base: Option<String>,

        /// Force the PR to be a draft. Overrides config (default: `draft = false`).
        /// Use `--no-draft` to force non-draft.
        #[arg(
            long,
            num_args = 0,
            default_missing_value = "true",
            default_value_if("no_draft", "true", Some("false"))
        )]
        #[config]
        pub draft: bool,

        /// Force the PR to be non-draft. Overrides config.
        #[arg(long, conflicts_with = "draft")]
        pub no_draft: bool,

        /// Enable auto-merge on the PR after creation (merges once required checks
        /// pass). Overrides config (default: `auto_merge = false`). Use
        /// `--no-auto-merge` to force no auto-merge.
        #[arg(
            long,
            num_args = 0,
            default_missing_value = "true",
            default_value_if("no_auto_merge", "true", Some("false"))
        )]
        #[config]
        pub auto_merge: bool,

        /// Disable auto-merge on the created PR. Overrides config.
        #[arg(long, conflicts_with = "auto_merge")]
        pub no_auto_merge: bool,

        /// Merge method used when auto-merge is enabled. Overrides config
        /// `auto_merge_method` (default `merge`).
        #[arg(long, value_name = "METHOD", value_enum)]
        #[config]
        pub auto_merge_method: AutoMergeMethod,

        /// jj template string used to render the PR body. Evaluated against the
        /// revset being PR'd in chronological order (`--reversed`), so a
        /// multi-commit stack renders bottom-up.
        ///
        /// Mutually exclusive with `--template-file` and `--no-template`.
        ///
        /// All standard jj template builtins are available (`description`,
        /// `commit_id`, `author`, etc.). The following template aliases are also
        /// injected:
        ///
        /// - `pr_title`: default title (first-line description of the oldest commit on the stack).
        ///
        /// - `pr_base`: resolved base branch; owner-qualified (`owner:branch`) for cross-fork PRs.
        ///
        /// - `pr_head_branch`: existing local bookmark on the rev, or empty if the rev is unpushed.
        ///
        /// - `pr_oldest_rev_id`: 40-char hex commit SHA of the oldest commit in the revset.
        #[arg(short = 'T', long, value_name = "TEMPLATE", conflicts_with_all = ["template_file", "no_template"])]
        pub template: Option<String>,

        /// Path or name (under `.github/PULL_REQUEST_TEMPLATE/`) of a markdown
        /// template file to use as the PR body. Mutually exclusive with `-T` and
        /// `--no-template`.
        #[arg(long, value_name = "PATH_OR_NAME", conflicts_with_all = ["template", "no_template"])]
        pub template_file: Option<String>,

        /// Skip body templating entirely.
        #[arg(long, conflicts_with_all = ["template", "template_file"])]
        pub no_template: bool,

        /// Editor command, e.g. `--editor "nvim +7"`. Default:
        /// `editor` in config, then `$VISUAL`, then `$EDITOR`.
        #[arg(short = 'e', long, value_name = "CMD", value_parser = shell_words::split)]
        #[config]
        pub editor: Option<Vec<String>>,

        /// Show a preview of the PR diffs while creating the PR body.
        /// Overrides `pr_create_show_diffs` configuration. Use `--no-diffs` to disable.
        #[arg(
            long = "diffs",
            num_args = 0,
            default_missing_value = "true",
            default_value_if("no_diffs", "true", Some("false"))
        )]
        #[config(maps_to = "pr_create_show_diffs")]
        pub show_diffs: bool,

        /// Hide the PR diff preview while creating the PR body. Overrides config.
        #[arg(long = "no-diffs", conflicts_with = "show_diffs")]
        pub no_diffs: bool,
    }
}

/// Run the full pr-create flow.
///
/// # Errors
///
/// Returns an error from any step (rev resolution, GH API, push, editor, etc.).
#[expect(clippy::too_many_lines)]
pub async fn run<J, G, E, ENV>(
    jj: &J,
    gh: &G,
    env: &ENV,
    editor: &E,
    args: &CreateArgs,
) -> Result<()>
where
    J: Jj,
    G: Gh,
    E: Editor,
    ENV: EnvReader,
{
    let args @ CreateArgs {
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
        rev,
        base,
        draft,
        auto_merge,
        editor: editor_argv,
        auto_merge_method,
        template: _,
        show_diffs,
        template_file: _,
        // these are resolved by clap/macro into positive fields or standalone control flags
        no_diffs: _,
        no_auto_merge: _,
        no_draft: _,
        no_template: _,
    } = args;

    let remote = jj.resolve_default_remote(remote.as_ref()).await?;
    let info = jj.resolve_rev(rev).await?;
    let existing_branch = info.bookmarks.first().cloned();

    let origin_url = jj
        .remote_url(&remote)
        .await?
        .ok_or_else(|| anyhow!("`{remote}` remote is not configured"))?;
    let upstream_url = jj.remote_url(upstream_remote).await?;
    let target = remote::target(&origin_url, upstream_url.as_deref())?;

    // Pre-flight only when we already have a bookmark; an unpushed rev can't have
    // a matching open PR.
    if let Some(branch) = &existing_branch {
        let head_spec = target.head_spec(branch);
        if let Some(existing) = gh
            .find_open_pr(&target.owner, &target.repo, &head_spec)
            .await?
        {
            log::info!(
                "PR #{} is already {} for `{}`: {}",
                existing.number,
                existing.state,
                head_spec,
                existing.title,
            );
            println!("{}", existing.html_url);
            return Ok(());
        }
    }

    let ancestor = jj.stacked_ancestor_bookmark(rev).await?;
    let base_branch = base
        .resolve_or(
            || async {
                if let Some(a) = &ancestor {
                    return Some(a.clone());
                }
                jj.trunk_branch().await.log_err().ok().flatten()
            },
            "could not detect base branch: `--base` not passed, no ancestor \
             bookmark on the stack, jj `trunk()` resolves to nothing, and \
             `default_base_branch` is not set in config",
        )
        .await?;

    let base_lookup = gh
        .lookup_base(&target.owner, &target.repo, &base_branch)
        .await?;
    if !base_lookup.branch_exists {
        return Err(anyhow!(
            "base branch `{base_branch}` does not exist on {}/{}",
            target.owner,
            target.repo,
        ));
    }
    let base_display = target.base_spec(&base_branch);

    let title_revset = jj::title_base_revset(rev, ancestor.as_deref());
    let default_title = jj.first_commit_description(&title_revset).await?;

    let raw_template = load_template_for(
        args,
        jj,
        &title_revset,
        &default_title,
        &base_display,
        existing_branch.as_deref(),
    )
    .await?;
    let initial_fm = Frontmatter {
        title: default_title,
        base: base_display,
        labels: vec![],
        reviewers: vec![],
        draft: *draft,
        auto_merge: *auto_merge,
        auto_merge_method: *auto_merge_method,
    };
    let raw_template_body = raw_template.clone().unwrap_or_default();

    let editor_argv = resolve_editor_argv(editor_argv.as_deref(), env)?;
    let diff_preview = if *show_diffs {
        jj.diff(&title_revset).await.log_err().ok()
    } else {
        None
    };
    let preview = diff_preview
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let (final_fm, body) = editor::round_trip(
        editor,
        &editor_argv,
        &initial_fm,
        raw_template.as_deref().unwrap_or_default(),
        preview,
    )
    .await?;
    validation::validate(&final_fm, &body, &raw_template_body)?;
    let final_base_branch = remote::branch_from_base_spec(&target.owner, &final_fm.base)?;
    let final_base_lookup = if final_base_branch == base_branch {
        base_lookup
    } else {
        let lookup = gh
            .lookup_base(&target.owner, &target.repo, &final_base_branch)
            .await?;
        if !lookup.branch_exists {
            return Err(anyhow!(
                "base branch `{final_base_branch}` does not exist on {}/{}",
                target.owner,
                target.repo,
            ));
        }
        lookup
    };

    jj.push(rev, remote).await?;

    let branch = if let Some(b) = existing_branch {
        b
    } else {
        let refreshed = jj.resolve_rev(rev).await?;
        refreshed
            .bookmarks
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("`jj git push -c {rev}` did not create a bookmark"))?
    };
    let head_spec = target.head_spec(&branch);

    let created = gh
        .create_pr(CreatePrRequest {
            title: final_fm.title.clone(),
            body: body.clone(),
            draft: final_fm.draft,
            repo_node_id: final_base_lookup.repo_node_id,
            head: head_spec,
            base: final_base_branch,
        })
        .await?;

    // Synthesize "before" so the diff fires only for labels/reviewers/auto-merge.
    // `create_pr` already set title/body/base/draft; reusing the same values
    // makes `apply_frontmatter_diff` skip those calls.
    let before_fm = Frontmatter {
        title: final_fm.title.clone(),
        base: final_fm.base.clone(),
        labels: vec![],
        reviewers: vec![],
        draft: final_fm.draft,
        auto_merge: false,
        auto_merge_method: final_fm.auto_merge_method,
    };
    let ctx = ApplyChangesCtx {
        owner: &target.owner,
        repo: &target.repo,
        pr_number: created.number,
        pr_node_id: &created.node_id,
        has_merge_queue: created.has_merge_queue,
        before_label_ids: HashMap::new(),
    };
    editor::apply_frontmatter_diff(gh, &ctx, &before_fm, &body, &final_fm, &body)
        .await
        .with_context(|| {
            format!(
                "PR created ({}), but applying metadata failed",
                created.html_url
            )
        })?;

    println!("{}", created.html_url);
    Ok(())
}
