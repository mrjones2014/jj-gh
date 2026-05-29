use crate::{
    cli::AuthArgs,
    config::{AutoMergeMethod, Config},
    gh::{CreatePrRequest, Gh, remote},
    jj::{self, Jj},
    pr::{
        editor::{EditorRoundTrip, resolve_editor_argv},
        frontmatter::Frontmatter,
        load_template_for, resolve_base, validation,
    },
};
use anyhow::{Context, Result, anyhow};
use serde::Serialize;

#[derive(Debug, clap::Args, Serialize)]
pub struct CreateArgs {
    /// Revision to create the PR from.
    #[arg(value_name = "REV")]
    #[serde(skip)]
    pub rev: String,

    /// Override the base bookmark. Default: closest ancestor bookmark on the
    /// stack, falling back to the remote's `main` / `master` / configured
    /// `default_base_branch`.
    #[arg(long, value_name = "BRANCH")]
    #[serde(skip)]
    pub base: Option<String>,

    /// Force the PR to be a draft. Overrides config (default: `draft = false`).
    /// Use `--no-draft` to force non-draft.
    #[arg(
        long,
        num_args = 0,
        default_missing_value = "true",
        default_value_if("no_draft", "true", Some("false"))
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft: Option<bool>,

    /// Force the PR to be non-draft. Overrides config.
    #[arg(long = "no-draft", conflicts_with = "draft")]
    #[serde(skip)]
    pub no_draft: bool,

    /// Enable auto-merge on the PR after creation (merges once required checks
    /// pass). Overrides config (default: `auto_merge = false`). Use
    /// `--no-auto-merge` to force no auto-merge.
    #[arg(
        long = "auto-merge",
        num_args = 0,
        default_missing_value = "true",
        default_value_if("no_auto_merge", "true", Some("false"))
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_merge: Option<bool>,

    /// Disable auto-merge on the created PR. Overrides config.
    #[arg(long = "no-auto-merge", conflicts_with = "auto_merge")]
    #[serde(skip)]
    pub no_auto_merge: bool,

    /// Merge method used when auto-merge is enabled. Overrides config
    /// `auto_merge_method` (default `merge`).
    #[arg(long = "auto-merge-method", value_name = "METHOD", value_enum)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_merge_method: Option<AutoMergeMethod>,

    /// jj template string used to render the PR body. Evaluated against the
    /// revset being PR'd in chronological order (`--reversed`), so a
    /// multi-commit stack renders bottom-up.
    ///
    /// All standard jj template builtins are available (`description`,
    /// `commit_id`, `author`, etc.). The following string aliases are also
    /// injected:
    ///
    /// - `pr_title`: default title (first-line description of the oldest
    ///   commit on the stack).
    /// - `pr_base`: resolved base branch.
    /// - `pr_head_branch`: existing local bookmark on the rev, or empty if
    ///   the rev is unpushed.
    ///
    /// Mutually exclusive with `--template-file` and `--no-template`.
    #[arg(short = 'T', long, value_name = "TEMPLATE", conflicts_with_all = ["template_file", "no_template"])]
    #[serde(skip)]
    pub template: Option<String>,

    /// Path or name (under `.github/PULL_REQUEST_TEMPLATE/`) of a markdown
    /// template file to use as the PR body. Mutually exclusive with `-T` and
    /// `--no-template`.
    #[arg(long = "template-file", value_name = "PATH_OR_NAME", conflicts_with_all = ["template", "no_template"])]
    #[serde(skip)]
    pub template_file: Option<String>,

    /// Skip body templating entirely.
    #[arg(long = "no-template", conflicts_with_all = ["template", "template_file"])]
    #[serde(skip)]
    pub no_template: bool,

    /// Editor command; shell-words split, e.g. `--editor "nvim +7"`. Default:
    /// `editor` in config, then `$VISUAL`, then `$EDITOR`.
    #[arg(short = 'e', long, value_name = "CMD", value_parser = shell_words::split)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor: Option<Vec<String>>,

    #[command(flatten)]
    #[serde(flatten)]
    pub auth: AuthArgs,
}

/// Run the full pr-create flow.
///
/// # Errors
///
/// Returns an error from any step (rev resolution, GH API, push, editor, etc.).
#[expect(clippy::too_many_lines)]
pub async fn run<J, G, E>(
    jj: &J,
    gh: &G,
    editor: &E,
    config: &Config,
    args: &CreateArgs,
) -> Result<()>
where
    J: Jj,
    G: Gh,
    E: EditorRoundTrip,
{
    let info = jj.resolve_rev(&args.rev).await?;
    let existing_branch = info.bookmarks.first().cloned();

    let origin_url = jj
        .remote_url(&config.default_remote)
        .await?
        .ok_or_else(|| anyhow!("`{}` remote is not configured", config.default_remote))?;
    let upstream_url = jj.remote_url(&config.upstream_remote).await?;
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

    let ancestor = jj.stacked_ancestor_bookmark(&args.rev).await?;
    let detected_base = jj
        .trunk_branch()
        .await?
        .unwrap_or_else(|| config.default_base_branch.clone());
    let base = resolve_base(args, ancestor.as_deref(), &detected_base);

    let base_lookup = gh.lookup_base(&target.owner, &target.repo, &base).await?;
    if !base_lookup.branch_exists {
        return Err(anyhow!(
            "base branch `{base}` does not exist on {}/{}",
            target.owner,
            target.repo,
        ));
    }

    let title_revset = jj::title_base_revset(&args.rev, ancestor.as_deref());
    let default_title = jj.first_commit_description(&title_revset).await?;

    let raw_template = load_template_for(
        args,
        jj,
        &title_revset,
        &default_title,
        &base,
        existing_branch.as_deref(),
    )
    .await?;
    let initial_fm = Frontmatter {
        title: default_title,
        base: base.clone(),
        labels: vec![],
        reviewers: vec![],
        draft: config.draft,
        auto_merge: config.auto_merge,
        auto_merge_method: config.auto_merge_method,
    };
    let initial_buffer = initial_fm.render(raw_template.as_deref().unwrap_or(""))?;
    let raw_template_body = raw_template.unwrap_or_default();

    let visual = std::env::var("VISUAL").ok();
    let editor_env = std::env::var("EDITOR").ok();
    let editor_argv = resolve_editor_argv(config, visual.as_deref(), editor_env.as_deref())?;
    let edited = editor.edit(&editor_argv, &initial_buffer).await?;
    let (final_fm, body) = Frontmatter::parse(&edited)?;
    validation::validate(&final_fm, &body, &raw_template_body)?;

    let Frontmatter {
        title,
        base,
        labels,
        reviewers,
        draft,
        auto_merge,
        auto_merge_method,
    } = final_fm;

    jj.push(&args.rev).await?;

    let branch = if let Some(b) = existing_branch {
        b
    } else {
        let refreshed = jj.resolve_rev(&args.rev).await?;
        refreshed
            .bookmarks
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("`jj git push -c {}` did not create a bookmark", args.rev))?
    };
    let head_spec = target.head_spec(&branch);
    let final_base = base.clone();

    let created = gh
        .create_pr(CreatePrRequest {
            title,
            body,
            draft,
            repo_node_id: base_lookup.repo_node_id,
            head: head_spec,
            base: final_base,
        })
        .await?;

    if !labels.is_empty() {
        gh.add_labels(&target.owner, &target.repo, created.number, &labels)
            .await
            .context(format!(
                "PR created ({}), but adding labels failed",
                created.html_url
            ))?;
    }

    if !reviewers.is_empty() {
        gh.add_reviewers(&target.owner, &target.repo, created.number, reviewers)
            .await
            .context(format!(
                "PR created ({}), but adding reviewers failed",
                created.html_url
            ))?;
    }

    if auto_merge {
        gh.enable_auto_merge(&created.node_id, created.has_merge_queue, auto_merge_method)
            .await
            .context(format!(
                "PR created ({}), but enabling auto-merge failed",
                created.html_url
            ))?;
    }

    println!("{}", created.html_url);
    Ok(())
}
