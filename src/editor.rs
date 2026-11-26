//! Editor command resolution + edit round-trip.
//!
//! Production [`TempfileEditor`] writes the initial buffer to a tempfile, spawns
//! the editor (inheriting stdio), then reads back. Tests use a fake.

use crate::{
    commands::pr::auto_merge::{self, AutoMergeTarget},
    config::AutoMergeMode,
    frontmatter::Frontmatter,
    gh::{Gh, Reviewer, UpdatePr, remote},
    logging,
    util::ShellCommand,
};
use anyhow::{Context, Result, anyhow};
use std::collections::HashMap;

const EDITOR_BASE_ENV: [(&str, &str); 1] = [("JJ_GH", "1")];

pub trait Editor {
    /// Edit `initial` with the configured editor and return the resulting buffer.
    ///
    /// # Errors
    ///
    /// Propagates IO and process errors.
    async fn edit(&self, argv: &[String], initial: &str) -> Result<String>;
}

/// Resolve the editor argv. `editor` carries the `--editor` flag merged over
/// the figment config stack, which layers the jj-gh `editor` key over jj's own
/// `ui.editor` (see [`crate::config`]). jj resolves `$JJ_EDITOR`, `$VISUAL`,
/// and `$EDITOR` into `ui.editor` itself, so there is no separate env fallback
/// here. Precedence: flag > jj-gh `editor` > `$JJ_EDITOR` > jj `ui.editor` >
/// `$VISUAL` > `$EDITOR`.
///
/// # Errors
///
/// Returns an error when every source is empty.
pub fn resolve_editor(editor: Option<&ShellCommand>) -> Result<ShellCommand> {
    editor.filter(|c| !c.is_empty()).cloned().ok_or_else(|| {
        anyhow!(
            "no editor configured; set --editor, `editor` in jj-gh config, jj's `ui.editor`, $VISUAL, or $EDITOR"
        )
    })
}

/// Render `fm` + `body` into the editor buffer, run the editor, and parse the
/// edited buffer back into `(Frontmatter, body)`. The buffer round-trip is the
/// only thing `pr create` and `pr edit` share verbatim.
///
/// When `preview` is `Some`, append a sentinel marker heading + fenced
/// `diff` block after the body. The parser strips everything from
/// the marker onward, so the preview never lands in the submitted PR body.
///
/// # Errors
///
/// Propagates render, IO/process, and parse errors.
pub async fn round_trip<E: Editor>(
    editor: &E,
    argv: &[String],
    fm: &Frontmatter,
    body: &str,
    preview: Option<&str>,
) -> Result<(Frontmatter, String)> {
    let initial = fm.render(body, preview)?;
    let edited = editor.edit(argv, &initial).await?;
    Frontmatter::parse(&edited).context("parsing PR frontmatter")
}

/// Identifiers and context needed by [`apply_frontmatter_diff`] to address the
/// right PR and translate label-name removals to GraphQL node IDs. Bundling
/// these keeps the diff function's signature tractable.
pub struct ApplyChangesCtx<'a> {
    pub owner: &'a str,
    pub repo: &'a str,
    pub pr_number: u64,
    pub pr_node_id: &'a str,
    /// `label_name -> label_node_id` for labels on the PR at fetch time.
    /// Empty for `pr create` since a brand-new PR has no labels to remove.
    pub before_label_ids: HashMap<String, String>,
    /// GitHub target used to move an auto-merge request, if the diff changes it.
    pub auto_merge_target: &'a remote::Target,
    /// The PR's current auto-merge state, as rendered in the editor.
    pub auto_merge_pr: AutoMergeTarget,
    /// Auto-merge value shown in the editor before the user edited it.
    pub auto_merge_before: Option<AutoMergeMode>,
}

/// Apply the diff between `before` and `after` (and their bodies) to an
/// existing PR via [`Gh`]. Used by `pr edit` (real diff) and `pr create`
/// (post-creation: text fields match the create-request, so only labels,
/// reviewers, and auto-merge actually fire).
///
/// # Errors
///
/// Returns the first GH API error. Earlier successful mutations are not
/// rolled back; the caller is responsible for surfacing partial-success
/// context (e.g. `"PR created, but applying labels failed"`).
pub async fn apply_frontmatter_diff<G: Gh>(
    gh: &G,
    ctx: &ApplyChangesCtx<'_>,
    before: &Frontmatter,
    before_body: &str,
    after: &Frontmatter,
    after_body: &str,
) -> Result<()> {
    let before_base = remote::branch_from_base_spec(ctx.owner, &before.base)?;
    let after_base = remote::branch_from_base_spec(ctx.owner, &after.base)?;
    gh.update_pr(UpdatePr {
        pr_node_id: ctx.pr_node_id.to_string(),
        title: (before.title != after.title).then(|| after.title.clone()),
        body: (before_body != after_body).then(|| after_body.to_string()),
        base_ref_name: (before_base != after_base).then_some(after_base),
    })
    .await?;

    if before.draft != after.draft {
        gh.set_draft(ctx.pr_node_id, after.draft).await?;
    }

    let labels_added = after
        .labels
        .iter()
        .filter(|l| !before.labels.contains(l))
        .cloned()
        .collect::<Vec<String>>();
    let labels_removed_ids = before
        .labels
        .iter()
        .filter(|name| !after.labels.contains(name))
        .filter_map(|name| ctx.before_label_ids.get(name).cloned())
        .collect::<Vec<String>>();
    gh.add_labels(ctx.owner, ctx.repo, ctx.pr_number, &labels_added)
        .await?;
    gh.remove_labels(ctx.pr_node_id, &labels_removed_ids)
        .await?;

    let reviewers_added = after
        .reviewers
        .iter()
        .filter(|r| !before.reviewers.contains(r))
        .cloned()
        .collect::<Vec<Reviewer>>();
    let reviewers_removed = before
        .reviewers
        .iter()
        .filter(|r| !after.reviewers.contains(r))
        .cloned()
        .collect::<Vec<Reviewer>>();
    gh.add_reviewers(ctx.owner, ctx.repo, ctx.pr_number, reviewers_added)
        .await?;
    gh.remove_reviewers(ctx.owner, ctx.repo, ctx.pr_number, reviewers_removed)
        .await?;

    // Auto-merge needs disable+enable when the user keeps it on but changes
    // the merge method, since `enablePullRequestAutoMerge` does not update the
    // method on an already-enabled PR. Whole-stack requests are moved through
    // the stack tip rather than re-enabled on this PR.
    // NB: Do not hard error here, just print a warning, in case
    // GitHub fixes their stupid API on their end. Auto-merge
    // cannot be enabled via API for repos with merge queues enabled
    // because GitHub's API is stupid.
    if let Err(err) = auto_merge::apply_editor_change(
        gh,
        auto_merge::AutoMergeEditorChange {
            target: ctx.auto_merge_target,
            pr: ctx.auto_merge_pr.clone(),
            before: ctx.auto_merge_before,
            after: after.auto_merge,
            method: after.auto_merge_method,
            method_explicit: before.auto_merge_method != after.auto_merge_method,
        },
    )
    .await
    {
        logging::warn!("Failed to enable auto-merge!");
        if ctx.auto_merge_pr.base_has_merge_queue {
            logging::warn!(
                "Merge queues are enabled for this repository. This is \
                currently a GitHub limitation, auto-merge cannot be enabled via \
                the API for repositories with merge queues enabled."
            );
        }
        logging::error!("{err}");
    }

    Ok(())
}

/// Production [`EditorRoundTrip`]: tempfile + spawn editor + read back.
pub struct TempfileEditor;

impl Editor for TempfileEditor {
    async fn edit(&self, argv: &[String], initial: &str) -> Result<String> {
        // The recognizable name lets editors associate the published
        // frontmatter JSON schema with just this buffer.
        let tmp = tempfile::Builder::new()
            .prefix(".jj-gh-pr-")
            .suffix(".md")
            .tempfile()
            .context("could not create tempfile for editor buffer")?;
        std::fs::write(tmp.path(), initial).context("could not write editor buffer")?;

        if argv.is_empty() {
            return Err(anyhow!("editor argv is empty"));
        }
        let tmp_arg = tmp.path().to_string_lossy().into_owned();
        let full = argv
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(tmp_arg.as_str()))
            .collect::<Vec<&str>>();
        crate::proc::interactive(&full, &EDITOR_BASE_ENV).await?;

        std::fs::read_to_string(tmp.path()).context("could not read edited buffer")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(parts: &[&str]) -> ShellCommand {
        ShellCommand(parts.iter().map(|s| (*s).to_string()).collect())
    }

    /// Mirror how `pr create` / `pr edit` resolve the editor. `flag.or(cfg)`
    /// models the figment merge where a present `--editor` wins over the
    /// config value (which itself layers jj-gh `editor` over jj `ui.editor`;
    /// see `crate::config` tests for that merge).
    fn resolve(flag: Option<ShellCommand>, cfg: Option<ShellCommand>) -> Option<ShellCommand> {
        let merged = flag.or(cfg);
        resolve_editor(merged.as_ref()).ok()
    }

    #[test]
    fn flag_outranks_config() {
        let got = resolve(Some(cmd(&["code", "--wait"])), Some(cmd(&["nano"])));
        assert_eq!(got, Some(cmd(&["code", "--wait"])));
    }

    #[test]
    fn config_used_when_no_flag() {
        let got = resolve(None, Some(cmd(&["nano"])));
        assert_eq!(got, Some(cmd(&["nano"])));
    }

    #[test]
    fn empty_command_treated_as_unset() {
        assert_eq!(resolve(None, Some(cmd(&[]))), None);
    }

    #[test]
    fn none_when_all_empty() {
        assert_eq!(resolve(None, None), None);
    }
}
