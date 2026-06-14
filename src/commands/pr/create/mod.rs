use crate::{
    cli::GlobalOpts,
    config::{self, AutoMergeMethod},
    editor::{self, ApplyChangesCtx, resolve_editor_argv},
    frontmatter::Frontmatter,
    fs::RealFs,
    gh::{CreatePrRequest, Gh, remote},
    jj::{
        self, Jj,
        inject::{TemplateAliases, escape_jj_string},
    },
    model::Model,
    template::{self, TemplateSource},
};
use anyhow::{Context, Result, anyhow, bail};
use jj_gh_config_derive::subcommand_args;
use std::collections::HashMap;

mod title_picker;

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

        /// Interactively choose which commit supplies the PR title.
        #[arg(long)]
        pub pick_title: bool,

        /// jj template string used to render candidate PR titles. Evaluated
        /// once per commit in the PR revset.
        #[arg(long, value_name = "TEMPLATE")]
        #[config(maps_to = "pr_create_title_template")]
        pub title_template: String,

        /// Editor command, e.g. `--editor "nvim +7"`. Default:
        /// `editor` in config, then `$VISUAL`, then `$EDITOR`.
        #[arg(short = 'e', long, value_name = "CMD", value_parser = shell_words::split)]
        #[config]
        pub editor: Option<Vec<String>>,

        /// Create the PR without opening an editor. Useful when combined with
        /// `--draft`.
        #[arg(long)]
        pub no_edit: bool,

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
pub async fn run(model: &impl Model, args: &CreateArgs) -> Result<()> {
    let jj = model.jj();
    let gh = model.gh().await?;
    let env = model.env();
    let editor = model.editor();
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
        no_edit,
        auto_merge_method,
        template: _,
        show_diffs,
        template_file: _,
        // these are resolved by clap/macro into positive fields or standalone control flags
        no_diffs: _,
        no_auto_merge: _,
        no_draft: _,
        no_template: _,
        pick_title,
        title_template,
    } = args;

    let (remote, target) = model
        .resolve_target(remote.as_ref(), Some(upstream_remote))
        .await?;
    let info = jj.resolve_rev(rev).await?;
    let existing_branch = info.bookmarks.first().cloned();

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
                jj.trunk_branch()
                    .await
                    .inspect_err(|e| log::debug!("could not detect trunk bookmark: {e:#}"))
                    .ok()
                    .flatten()
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
    let candidates = resolve_title_candidates(jj, &title_revset, title_template).await?;
    let default_title = if *pick_title {
        crate::ui::tui::require_tty("--pick-title")?;
        title_picker::pick(&candidates)?
    } else {
        candidates
            .first()
            .context("no commits found in the PR revset")?
            .valid_title()
            .context("oldest commit produced an invalid PR title")?
            .to_string()
    };

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
    let (final_fm, body) = if *no_edit {
        (initial_fm, raw_template.unwrap_or_default())
    } else {
        let editor_argv = resolve_editor_argv(editor_argv.as_deref(), env)?;
        let diff_preview = if *show_diffs {
            jj.diff(&title_revset)
                .await
                .inspect_err(|e| log::debug!("could not render diff preview: {e:#}"))
                .ok()
        } else {
            None
        };
        let preview = diff_preview
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        editor::round_trip(
            editor,
            &editor_argv,
            &initial_fm,
            raw_template.as_deref().unwrap_or_default(),
            preview,
        )
        .await?
    };
    final_fm.validate()?;
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

const TITLE_RECORD_OPEN: char = '\u{E010}';
const TITLE_RECORD_SEPARATOR: char = '\u{E011}';
const TITLE_RECORD_CLOSE: char = '\u{E012}';

#[derive(Debug, Clone)]
pub(crate) struct TitleCandidate {
    pub change_id: String,
    pub title: String,
}

impl TitleCandidate {
    pub(crate) fn valid_title(&self) -> Option<&str> {
        let title = self.title.trim();
        (!title.is_empty() && !title.contains(['\n', '\r'])).then_some(title)
    }
}

async fn resolve_title_candidates(
    jj: &impl Jj,
    title_revset: &str,
    title_template: &str,
) -> Result<Vec<TitleCandidate>> {
    let template = format!(
        r#""{TITLE_RECORD_OPEN}" ++ change_id.shortest(8) ++ "{TITLE_RECORD_SEPARATOR}" ++ ({title_template}) ++ "{TITLE_RECORD_CLOSE}""#
    );
    let rendered = jj
        .eval_template(title_revset, &template, None, true)
        .await
        .context("evaluating PR title template")?;
    parse_title_candidates(&rendered)
}

fn parse_title_candidates(rendered: &str) -> Result<Vec<TitleCandidate>> {
    let mut candidates = Vec::new();
    let mut rest = rendered;
    while let Some(open) = rest.find(TITLE_RECORD_OPEN) {
        rest = &rest[open + TITLE_RECORD_OPEN.len_utf8()..];
        let separator = rest
            .find(TITLE_RECORD_SEPARATOR)
            .context("malformed PR title candidate: missing separator")?;
        let change_id = &rest[..separator];
        rest = &rest[separator + TITLE_RECORD_SEPARATOR.len_utf8()..];
        let close = rest
            .find(TITLE_RECORD_CLOSE)
            .context("malformed PR title candidate: missing closing marker")?;
        let title = &rest[..close];
        rest = &rest[close + TITLE_RECORD_CLOSE.len_utf8()..];
        candidates.push(TitleCandidate {
            change_id: change_id.to_string(),
            title: title.to_string(),
        });
    }
    if candidates.is_empty() {
        bail!("no commits found in the PR revset");
    }
    Ok(candidates)
}

async fn load_template_for(
    args: &CreateArgs,
    jj: &impl Jj,
    title_revset: &str,
    default_title: &str,
    base: &str,
    head_branch: Option<&str>,
) -> Result<Option<String>> {
    let repo_root = std::env::current_dir().context("could not read cwd")?;
    let fs = RealFs;
    let user_layer = config::user_layer_template()?;
    let repo_layer = config::repo_layer_template()?;
    match template::resolve_template_source(args, &repo_layer, &user_layer, &repo_root, &fs) {
        TemplateSource::None => Ok(None),
        TemplateSource::File(p) => template::load_template_file(&p, &fs),
        TemplateSource::JjTemplate(t) => {
            let oldest_rev_id = jj
                .eval_template(title_revset, r#"commit_id.short(40) ++ "\n""#, None, true)
                .await
                .context("resolving oldest commit id for `pr_oldest_rev_id` alias")?
                .lines()
                .next()
                .unwrap_or("")
                .to_string();
            let aliases = TemplateAliases::builder()
                .alias("pr_title", quote_jj(default_title))
                .alias("pr_base", quote_jj(base))
                .alias("pr_head_branch", quote_jj(head_branch.unwrap_or("")))
                .alias("pr_oldest_rev_id", quote_jj(&oldest_rev_id));
            let tmp = aliases.write_temp_config()?;
            let body = jj
                .eval_template(title_revset, &t, Some(tmp.path()), true)
                .await
                .context("evaluating PR body template")?;
            Ok(Some(body.trim_end_matches('\n').to_string()))
        }
    }
}

/// Wrap `s` as a jj template double-quoted string literal, escaping `\` and `"`.
fn quote_jj(s: &str) -> String {
    format!(r#""{}""#, escape_jj_string(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_candidate_requires_single_nonempty_line() {
        let valid = TitleCandidate {
            change_id: "abcdefgh".into(),
            title: "  good title  ".into(),
        };
        assert_eq!(valid.valid_title(), Some("good title"));

        for title in ["", " \t ", "first\nsecond", "first\rsecond"] {
            let invalid = TitleCandidate {
                change_id: "abcdefgh".into(),
                title: title.into(),
            };
            assert!(invalid.valid_title().is_none());
        }
    }

    #[test]
    fn parses_marker_delimited_title_candidates() {
        let rendered = format!(
            "{TITLE_RECORD_OPEN}abc{TITLE_RECORD_SEPARATOR}one{TITLE_RECORD_CLOSE}\
             {TITLE_RECORD_OPEN}def{TITLE_RECORD_SEPARATOR}two\nlines{TITLE_RECORD_CLOSE}"
        );
        let candidates = parse_title_candidates(&rendered).unwrap();
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].change_id, "abc");
        assert_eq!(candidates[1].title, "two\nlines");
    }

    #[test]
    fn rejects_empty_candidate_set() {
        assert!(parse_title_candidates("").is_err());
    }
}
