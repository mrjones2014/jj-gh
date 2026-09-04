use crate::{
    cli::GlobalOpts,
    config::{self, AutoMergeMethod, DefaultTitleSource},
    editor::{self, ApplyChangesCtx},
    frontmatter::Frontmatter,
    fs::RealFs,
    gh::{CreatePrRequest, Gh, remote},
    jj::{
        self, Jj,
        inject::{TemplateAliases, quote_jj},
    },
    model::Model,
    template::{self, TemplateSource},
    ui::{PrLinks, Stream},
};
use anyhow::{Context, Result, anyhow, bail};
use jj_gh_config_derive::subcommand_args;
use std::collections::HashMap;

mod title_picker;

/// A PR this run produced: freshly created, or the already-open one found for
/// the revision. The URL rides along so the summary can hyperlink it.
struct CreatedPr {
    number: u64,
    html_url: String,
}

subcommand_args! {
    pub struct CreateArgs {
        /// Revision(s) to create PR(s) from. Pass multiple revisions to create a stack.
        #[arg(value_name = "REV")]
        pub revs: Vec<String>,

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
        /// - `pr_title`: default title (first-line description of the oldest or newest commit, depending on `default_title_source`).
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

        /// Which commit's description to use as the default PR title.
        /// `base` uses the oldest commit (default), `head` uses the newest.
        /// Overrides config `default_title_source`.
        #[arg(long = "title-source", value_name = "SOURCE", value_enum)]
        #[config]
        pub default_title_source: crate::config::DefaultTitleSource,

        /// Editor command, e.g. `--editor "nvim +7"`. Precedence: this flag,
        /// then `editor` in config, then `$VISUAL`, then `$EDITOR`.
        #[arg(short = 'e', long, value_name = "CMD", value_parser = crate::util::parse_shell_command)]
        #[config]
        pub editor: Option<crate::util::ShellCommand>,

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

        /// Automatically link created PRs into a GitHub stack when stacked.
        /// Default: true. Use `--no-stack` to disable.
        #[arg(
            long = "stack",
            num_args = 0,
            default_missing_value = "true",
            default_value_if("no_stack", "true", Some("false"))
        )]
        #[config]
        pub auto_stack: bool,

        /// Disable automatic stack linking. Overrides config.
        #[arg(long = "no-stack", conflicts_with = "auto_stack")]
        pub no_stack: bool,
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
        revs,
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
        // stack linking control
        no_stack: _,
        auto_stack,
        // title source
        default_title_source,
    } = args;

    let upstream_remote = crate::gh::remote::resolved_upstream_remote(upstream_remote);
    let (remote, target) = model.resolve_target(remote, Some(upstream_remote)).await?;

    let mut created_prs = Vec::with_capacity(revs.len());

    for rev in revs {
        let created = create_single_pr(
            jj,
            gh,
            env,
            editor,
            args,
            rev,
            base,
            draft,
            auto_merge,
            editor_argv.as_ref(),
            no_edit,
            auto_merge_method,
            show_diffs,
            pick_title,
            title_template,
            default_title_source,
            &remote,
            &target,
        )
        .await?;
        created_prs.push(created);
    }

    // A summary block we format ourselves, so it goes to stderr directly rather
    // than through the logger, which would prefix and recolor every line.
    if created_prs.len() > 1 {
        let links = created_prs
            .iter()
            .map(|pr| (pr.number, pr.html_url.clone()))
            .collect::<PrLinks>();
        eprintln!("\nCreated {} PRs:", created_prs.len());
        for pr in &created_prs {
            eprintln!("  {}", links.number(Stream::Stderr, pr.number));
        }
    }

    if !*auto_stack {
        return Ok(());
    }

    // For multi-revision: detect chains among just the created PRs
    if created_prs.len() >= 2 {
        let numbers = created_prs.iter().map(|pr| pr.number).collect::<Vec<u64>>();
        let spinner = crate::ui::Spinner::start("Fetching PR details for stack detection");
        let pr_details = fetch_pr_details(gh, &target, &numbers).await;
        spinner.stop();

        // Build head_ref -> local_commit_id mapping from the revisions we created
        let commit_infos = revs
            .iter()
            .map(|rev| jj.resolve_rev(rev))
            .collect::<futures::future::JoinAll<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
        let head_to_local = commit_infos
            .iter()
            .filter_map(|info| {
                info.bookmarks
                    .first()
                    .map(|bookmark| (bookmark.clone(), info.commit_id.clone()))
            })
            .collect::<HashMap<String, String>>();

        let spinner = crate::ui::Spinner::start("Detecting stack chains");
        let results = link_stacks(gh, jj, &target, &pr_details, &head_to_local).await?;
        spinner.stop();
        crate::commands::pr::stack::print_stack_results(
            &PrLinks::from_details(&pr_details),
            &results,
        );
    } else if created_prs.len() == 1 {
        // For single-revision: detect chains among all pushed PRs + the new one
        let spinner = crate::ui::Spinner::start("Fetching local PRs");
        let bookmarks = jj.pushed_bookmarks(&remote).await?;
        let branch_names = bookmarks
            .iter()
            .map(|b| b.name.clone())
            .collect::<Vec<String>>();
        let all_prs = gh
            .local_pulls(
                &target.owner,
                &target.repo,
                target.origin_owner(),
                &branch_names,
            )
            .await?;
        spinner.stop();

        // Build head_to_local_commit mapping from bookmarks
        let head_to_local = bookmarks
            .iter()
            .map(|b| (b.name.clone(), b.local_commit_id.clone()))
            .collect::<HashMap<String, String>>();

        // Fetch full details for stack detection
        let spinner = crate::ui::Spinner::start("Fetching PR details for stack detection");
        let mut pr_details = Vec::with_capacity(all_prs.len());
        for pr in &all_prs {
            pr_details.push(
                gh.get_pr(&target.owner, &target.repo, pr.number)
                    .await
                    .context("Failed to fetch PR")?,
            );
        }

        // Ensure the newly created PR is included
        let new_pr_number = created_prs[0].number;
        let already_included = pr_details.iter().any(|p| p.number == new_pr_number);
        if !already_included
            && let Ok(details) = gh.get_pr(&target.owner, &target.repo, new_pr_number).await
        {
            pr_details.push(details);
        }
        spinner.stop();

        let spinner = crate::ui::Spinner::start("Detecting stack chains");
        let results = link_stacks(gh, jj, &target, &pr_details, &head_to_local).await?;
        spinner.stop();
        crate::commands::pr::stack::print_stack_results(
            &PrLinks::from_details(&pr_details),
            &results,
        );
    }

    Ok(())
}

/// Fetch full PR details for a list of PR numbers, logging warnings for failures.
async fn fetch_pr_details(
    gh: &impl Gh,
    target: &crate::gh::remote::Target,
    pr_numbers: &[u64],
) -> Vec<crate::gh::PrDetails> {
    let mut details = Vec::with_capacity(pr_numbers.len());
    for &pr_number in pr_numbers {
        if let Ok(d) = gh
            .get_pr(&target.owner, &target.repo, pr_number)
            .await
            .inspect_err(|e| {
                log::warn!("Failed to fetch PR #{pr_number} for stack detection: {e:#}");
            })
        {
            details.push(d);
        }
    }
    details
}

/// Detect stack chains among the given PRs and create stacks on GitHub.
///
/// Linking is best-effort: the PRs themselves are already created by the time
/// this runs, so a stack failure is reported but does not fail the command.
///
/// Base refs are only realigned *within* a chain, by
/// [`crate::gh::stack_create::create_stacks`]. The bottom PR keeps whatever
/// base the user chose in the editor; reconciling every base against the graph
/// is `jj-gh pr stack`'s job, not something `pr create` should do behind the
/// user's back.
///
/// Returns what happened so the caller can stop its spinner before printing;
/// writing to stdout under a live spinner leaves the glyph on the line.
async fn link_stacks(
    gh: &impl Gh,
    jj: &impl Jj,
    target: &crate::gh::remote::Target,
    pr_details: &[crate::gh::PrDetails],
    head_to_local_commit: &HashMap<String, String>,
) -> Result<Vec<crate::gh::stack_create::ChainResult>> {
    let shape = crate::gh::stack_detect::detect(pr_details, jj, head_to_local_commit, None).await?;
    if shape.chains.is_empty() {
        return Ok(Vec::new());
    }
    let existing_stacks = gh.list_stacks(&target.owner, &target.repo).await?;
    Ok(crate::gh::stack_create::create_stacks(
        gh,
        target,
        &shape.chains,
        pr_details,
        &existing_stacks,
    )
    .await
    .inspect_err(|e| log::warn!("Failed to link PR stacks: {e:#}"))
    .unwrap_or_default())
}

/// Create a single PR for one revision.
#[expect(clippy::too_many_lines)]
#[expect(clippy::too_many_arguments)]
async fn create_single_pr(
    jj: &impl Jj,
    gh: &impl Gh,
    env: &impl crate::auth::EnvReader,
    editor: &impl crate::editor::Editor,
    args: &CreateArgs,
    rev: &str,
    base: &crate::util::EvalWithCfgFallback<String>,
    draft: &bool,
    auto_merge: &bool,
    editor_argv: Option<&crate::util::ShellCommand>,
    no_edit: &bool,
    auto_merge_method: &AutoMergeMethod,
    show_diffs: &bool,
    pick_title: &bool,
    title_template: &str,
    default_title_source: &crate::config::DefaultTitleSource,
    remote: &str,
    target: &crate::gh::remote::Target,
) -> Result<CreatedPr> {
    if *pick_title {
        crate::ui::tui::require_tty("--pick-title")?;
    }

    let spinner = crate::ui::Spinner::start("Resolving revision");
    let info = jj.resolve_rev(rev).await?;
    let existing_branch = info.bookmarks.first().cloned();

    // Pre-flight only when we already have a bookmark; an unpushed rev can't have
    // a matching open PR.
    if let Some(branch) = &existing_branch {
        spinner.set_message("Checking for existing PR".into());
        let head_spec = target.head_spec(branch);
        let existing = gh
            .find_open_pr(&target.owner, &target.repo, &head_spec)
            .await?;
        if let Some(existing) = existing {
            spinner.stop();
            log::info!(
                "PR #{} is already {} for `{}`: {}",
                existing.number,
                existing.state,
                head_spec,
                existing.title,
            );
            crate::ui::print_url(&existing.html_url);
            return Ok(CreatedPr {
                number: existing.number,
                html_url: existing.html_url,
            });
        }
    }

    // Jujutsu gives us the revision for an automatic base. For an explicit or
    // configured base, find the revision on the target remote. A local bookmark
    // with the same name can point to a different revision.
    spinner.set_message("Detecting base branch".into());
    let (base_branch, local_base_rev) = if let Some(branch) = base.cli() {
        (branch.clone(), None)
    } else if let Some(ancestor) = jj.stacked_ancestor_bookmark(rev).await? {
        (ancestor.clone(), Some(ancestor))
    } else if let Some(trunk) = jj
        .trunk_branch()
        .await
        .inspect_err(|e| log::debug!("could not detect trunk bookmark: {e:#}"))
        .ok()
        .flatten()
    {
        (trunk, Some("trunk()".to_string()))
    } else if let Some(branch) = base.fallback() {
        (branch.clone(), None)
    } else {
        spinner.stop();
        bail!(
            "could not detect base branch: `--base` not passed, no ancestor \
             bookmark on the stack, jj `trunk()` resolves to nothing, and \
             `default_base_branch` is not set in config"
        );
    };

    spinner.set_message("Verifying base branch on GitHub".into());
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

    let base_rev = if let Some(base_rev) = local_base_rev {
        base_rev
    } else {
        spinner.set_message("Resolving base revision".into());
        jj.remote_bookmark_sha(&base_branch, remote)
            .await?
            .with_context(|| {
                format!(
                    "base branch `{base_branch}` exists on GitHub, but the local repository does \
                     not have `{base_branch}@{remote}`; fetch that remote and try again"
                )
            })?
    };
    let title_revset = jj::title_base_revset(rev, &base_rev);
    spinner.set_message("Generating title candidates".into());
    let candidates = resolve_title_candidates(jj, &title_revset, title_template).await?;
    let default_title = if *pick_title {
        title_picker::pick(&candidates)?
    } else {
        let candidate = match default_title_source {
            DefaultTitleSource::Base => candidates.first(),
            DefaultTitleSource::Head => candidates.last(),
        }
        .context("no commits found in the PR revset")?;
        let source_label = match default_title_source {
            DefaultTitleSource::Base => "oldest",
            DefaultTitleSource::Head => "newest",
        };
        candidate
            .valid_title()
            .with_context(|| format!("{source_label} commit produced an invalid PR title"))?
            .to_string()
    };

    spinner.set_message("Loading PR template".into());
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
        let editor_argv = editor::resolve_editor(editor_argv, env)?;
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
        spinner.stop();
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
        let spinner = crate::ui::spinner::Spinner::start("Verifying updated base branch");
        let lookup = gh
            .lookup_base(&target.owner, &target.repo, &final_base_branch)
            .await?;
        spinner.stop();
        if !lookup.branch_exists {
            return Err(anyhow!(
                "base branch `{final_base_branch}` does not exist on {}/{}",
                target.owner,
                target.repo,
            ));
        }
        lookup
    };

    jj.push(rev, existing_branch.as_deref(), remote.to_string())
        .await?;

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

    let spinner = crate::ui::spinner::Spinner::start("Creating pull request");
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
    spinner.set_message("Applying PR metadata".into());
    editor::apply_frontmatter_diff(gh, &ctx, &before_fm, &body, &final_fm, &body)
        .await
        .with_context(|| {
            format!(
                "PR created ({}), but applying metadata failed",
                created.html_url
            )
        })?;
    spinner.stop();

    crate::ui::print_url(&created.html_url);
    Ok(CreatedPr {
        number: created.number,
        html_url: created.html_url,
    })
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
        .eval_template(title_revset, &template, None, true, false)
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
                .eval_template(
                    title_revset,
                    r#"commit_id.short(40) ++ "\n""#,
                    None,
                    true,
                    false,
                )
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
                .eval_template(title_revset, &t, Some(tmp.path()), true, false)
                .await
                .context("evaluating PR body template")?;
            Ok(Some(body.trim_end_matches('\n').to_string()))
        }
    }
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

    #[test]
    fn title_source_base_selects_first_candidate() {
        let candidates = [
            TitleCandidate {
                change_id: "first".into(),
                title: "First commit".into(),
            },
            TitleCandidate {
                change_id: "second".into(),
                title: "Second commit".into(),
            },
            TitleCandidate {
                change_id: "third".into(),
                title: "Third commit".into(),
            },
        ];

        let candidate = match crate::config::DefaultTitleSource::Base {
            crate::config::DefaultTitleSource::Base => candidates.first(),
            crate::config::DefaultTitleSource::Head => candidates.last(),
        }
        .unwrap();

        assert_eq!(candidate.change_id, "first");
        assert_eq!(candidate.title, "First commit");
    }

    #[test]
    fn title_source_head_selects_last_candidate() {
        let candidates = [
            TitleCandidate {
                change_id: "first".into(),
                title: "First commit".into(),
            },
            TitleCandidate {
                change_id: "second".into(),
                title: "Second commit".into(),
            },
            TitleCandidate {
                change_id: "third".into(),
                title: "Third commit".into(),
            },
        ];

        let candidate = match crate::config::DefaultTitleSource::Head {
            crate::config::DefaultTitleSource::Base => candidates.first(),
            crate::config::DefaultTitleSource::Head => candidates.last(),
        }
        .unwrap();

        assert_eq!(candidate.change_id, "third");
        assert_eq!(candidate.title, "Third commit");
    }
}
