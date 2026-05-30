use crate::{
    auth::EnvReader,
    cli::AuthArgs,
    config::Config,
    gh::{Gh, PrDetails, remote::Target},
    jj::Jj,
    pr::{
        self, PrLookup,
        editor::{EditorRoundTrip, resolve_editor_argv},
        frontmatter::Frontmatter,
    },
};
use anyhow::{Context, Result};
use anyhow::{anyhow, bail};
use serde::Serialize;

#[derive(Debug, clap::Args, Serialize)]
pub struct EditArgs {
    /// Revision to create the PR from.
    #[arg(value_name = "REV")]
    #[serde(skip)]
    pub rev: String,

    /// Force edit the PR; by default, if the PR body is empty,
    /// `jj-gh` will refuse to edit it to avoid unexpectedly
    /// deleting a PR body that does exist, but we were unable
    /// to load due to an error.
    #[arg(default_value = "false", alias = "f")]
    pub force: bool,

    /// Remote to check for PRs on, e.g. "origin" or "upstream"
    #[arg(value_name = "REMOTE_NAME")]
    #[serde(skip)]
    pub remote: Option<String>,

    /// Editor command; shell-words split, e.g. `--editor "nvim +7"`. Default:
    /// `editor` in config, then `$VISUAL`, then `$EDITOR`.
    #[arg(short = 'e', long, value_name = "CMD", value_parser = shell_words::split)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor: Option<Vec<String>>,

    #[command(flatten)]
    #[serde(flatten)]
    pub auth: AuthArgs,
}

/// Fetch a PR and edit it in the provided editor.
///
/// # Errors
///
/// Returns an error from any step (rev resolution, GH API, push, editor, etc.).
#[expect(clippy::unused_async)]
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
    E: EditorRoundTrip,
    ENV: EnvReader,
{
    let EditArgs {
        editor: _, // merged into config by figment layer
        rev,
        remote,
        auth,
        force,
    } = args;
    let editor_argv = resolve_editor_argv(config, env)?;
    let PrLookup {
        branch,
        target: Target { repo, owner, .. },
        head_spec,
        default_base,
        summary,
    } = pr::resolve_pr_for_rev(jj, gh, config, &rev).await?;
    let summary = summary.context("No PR summary, is there an open PR?")?;
    let PrDetails {
        number,
        title,
        html_url,
        head_ref,
        head_sha,
        head_user_login,
        head_repo_name,
        graphql_node_id,
        in_merge_queue,
        labels,
        is_draft,
        auto_merge,
        auto_merge_method,
        reviewers,
        body,
    } = gh
        .get_pr(&owner, &repo, summary.number, true)
        .await
        .context("Fetching PR from GitHub")?;
    let fm = Frontmatter {
        title,
        labels,
        reviewers,
        auto_merge,
        base: head_ref,
        draft: is_draft,
        auto_merge_method: auto_merge_method.unwrap_or_default(),
    };
    if body.is_none() {
        if *force {
            log::warn!("PR body is empty, but `--force` was passed");
        } else {
            bail!(
                "PR body is empty when attempting to edit! Refusing to edit to avoid data loss. Pass `--force` to override this."
            );
        }
    }
    let buffer = editor
        .edit(&editor_argv, fm.render(summary))
        .await
        .context("Editing a PR")?;
    let (
        Frontmatter {
            title,
            base,
            labels,
            reviewers,
            draft,
            auto_merge,
            auto_merge_method,
        },
        body,
    ) = Frontmatter::parse(buffer).context("Parsing PR frontmatter")?;

    todo!()
}
